import type { ArrangementOperation } from "../api/arrangement";
import type { GestureGuard, GestureInput } from "./gesture";
import { integer, MAX_SONG_TICKS, projectClipRect, snapTick, type PlaylistView, type Point, type Rect } from "./geometry";
import type { RenderClip } from "./viewport";

export const togglesSelection = (event: { shiftKey: boolean; ctrlKey: boolean; metaKey: boolean }): boolean => event.shiftKey || event.ctrlKey || event.metaKey;
export const crossedThreshold = (start: Point, end: Point): boolean => Math.hypot(end.x - start.x, end.y - start.y) >= 4;
export function toggleSelection(ids: readonly number[], id: number): number[] {
  return ids.includes(id) ? ids.filter(value => value !== id) : [...ids, id];
}
export function marqueeRect(start: Point, end: Point): Rect {
  return { x: Math.min(start.x, end.x), y: Math.min(start.y, end.y), width: Math.abs(start.x - end.x), height: Math.abs(start.y - end.y) };
}
export function marqueeSelection(clips: readonly RenderClip[], rect: Rect, view: PlaylistView): number[] {
  if (rect.width === 0 || rect.height === 0) return [];
  return clips.flatMap(clip => {
    if (clip.id === null) return [];
    const bounds = projectClipRect(clip, view);
    return bounds.x < rect.x + rect.width && bounds.x + bounds.width > rect.x && bounds.y < rect.y + rect.height && bounds.y + bounds.height > rect.y ? [clip.id] : [];
  });
}
export type GroupPreview = Readonly<{ kind: "move" | "copy"; deltaTicks: number; deltaTracks: number }>;

export type ClipGroupInput = GestureGuard & Pick<GestureInput, "originTick" | "originTrack" | "quantum" | "tracks"> &
  Readonly<{ kind: GroupPreview["kind"] }>;

/** One shared constrained delta: independently clamping each clip would deform the group. */
export class ClipGroupGesture {
  readonly memberIds: ReadonlySet<number>;
  private readonly input: GestureGuard & Pick<GestureInput, "originTick" | "originTrack" | "quantum">;
  private readonly clips: readonly { id: number; startTick: number; trackIndex: number }[];
  private readonly tracks: readonly number[];
  private readonly bounds: { minTick: number; maxTick: number; minTrack: number; maxTrack: number };
  private value: GroupPreview;
  private closed = false;

  constructor(input: ClipGroupInput, clips: readonly RenderClip[]) {
    const kind = input.kind;
    if (!clips.length) throw new RangeError("Invalid clip group");
    this.tracks = input.tracks.map(track => track.id);
    this.input = { pointerId: input.pointerId, stamp: { ...input.stamp }, replacements: input.replacements,
      originTick: input.originTick, originTrack: input.originTrack, quantum: input.quantum };
    let firstTick = MAX_SONG_TICKS, lastTick = 0, firstTrack = this.tracks.length, lastTrack = 0;
    const ids = new Set<number>();
    this.clips = clips.map(({ id, startTick, trackIndex, lengthTicks }) => {
      integer(startTick, "clip start");
      integer(lengthTicks, "clip length", 1, MAX_SONG_TICKS - startTick);
      integer(trackIndex, "track index", 0, this.tracks.length - 1);
      if (id === null || ids.has(id)) throw new RangeError("Invalid clip group member");
      integer(id, "clip identity", 0, 0xffffffff);
      ids.add(id);
      firstTick = Math.min(firstTick, startTick);
      lastTick = Math.max(lastTick, startTick + lengthTicks);
      firstTrack = Math.min(firstTrack, trackIndex);
      lastTrack = Math.max(lastTrack, trackIndex);
      return { id, startTick, trackIndex };
    });
    this.memberIds = ids;
    this.bounds = { minTick: -firstTick, maxTick: MAX_SONG_TICKS - lastTick, minTrack: -firstTrack, maxTrack: this.tracks.length - 1 - lastTrack };
    this.value = { kind, deltaTicks: 0, deltaTracks: 0 };
  }

  get preview(): GroupPreview { return this.value; }
  cancel(): void { this.closed = true; }

  update(tick: number, row: number): void {
    if (this.closed) return;
    integer(row, "pointer track", 0, Number.MAX_SAFE_INTEGER);
    const { minTick, maxTick, minTrack, maxTrack } = this.bounds;
    const deltaTicks = snapTick(tick - this.input.originTick, this.input.quantum, minTick, maxTick);
    const deltaTracks = Math.max(minTrack, Math.min(row - this.input.originTrack, maxTrack));
    if (deltaTicks !== this.value.deltaTicks || deltaTracks !== this.value.deltaTracks) {
      this.value = { kind: this.value.kind, deltaTicks, deltaTracks };
    }
  }

  take(guard: GestureGuard): readonly ArrangementOperation[] | null {
    if (this.closed || guard.pointerId !== this.input.pointerId) return null;
    this.closed = true;
    if (guard.replacements !== this.input.replacements || guard.stamp.generation !== this.input.stamp.generation ||
        guard.stamp.revision !== this.input.stamp.revision) return null;
    const { kind, deltaTicks, deltaTracks } = this.value;
    if (kind === "move" && deltaTicks === 0 && deltaTracks === 0) return [];
    return this.clips.map(({ id, startTick, trackIndex }) => ({
      kind, clip: id, startTick: startTick + deltaTicks, track: this.tracks[trackIndex + deltaTracks],
    }));
  }
}
