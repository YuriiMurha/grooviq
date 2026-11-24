import type { ArrangementOperation } from "../api/arrangement";
import type { GestureGuard, GestureInput } from "./gesture";
import { clipRect, integer, MAX_SONG_TICKS, snapTick, validatePattern, type PatternPreview, type PlaylistView, type Point, type Rect } from "./geometry";

export const togglesSelection = (event: { shiftKey: boolean; ctrlKey: boolean; metaKey: boolean }): boolean => event.shiftKey || event.ctrlKey || event.metaKey;
export const crossedThreshold = (start: Point, end: Point): boolean => Math.hypot(end.x - start.x, end.y - start.y) >= 4;
export function toggleSelection(ids: readonly number[], id: number): number[] {
  return ids.includes(id) ? ids.filter(value => value !== id) : [...ids, id];
}
export function marqueeRect(start: Point, end: Point): Rect {
  return { x: Math.min(start.x, end.x), y: Math.min(start.y, end.y), width: Math.abs(start.x - end.x), height: Math.abs(start.y - end.y) };
}
export function marqueeSelection(clips: readonly PatternPreview[], rect: Rect, view: PlaylistView): number[] {
  if (rect.width === 0 || rect.height === 0) return [];
  return clips.flatMap(clip => {
    const bounds = clipRect(clip, view);
    return clip.id !== null && bounds.x < rect.x + rect.width && bounds.x + bounds.width > rect.x && bounds.y < rect.y + rect.height && bounds.y + bounds.height > rect.y ? [clip.id] : [];
  });
}
export type GroupPreview = Readonly<{ kind: "move" | "copy"; deltaTicks: number; deltaTracks: number }>;

/** One shared constrained delta: independently clamping each clip would deform the group. */
export class PatternGroupGesture {
  private readonly input: GestureGuard & Pick<GestureInput, "originTick" | "originTrack" | "quantum">;
  private readonly clips: readonly { id: number; startTick: number; trackIndex: number }[];
  private readonly tracks: readonly number[];
  private readonly bounds: { minTick: number; maxTick: number; minTrack: number; maxTrack: number };
  private value: GroupPreview;
  private closed = false;

  constructor(input: GestureInput, clips: readonly PatternPreview[]) {
    const kind = input.kind;
    if (!clips.length || (kind !== "move" && kind !== "copy") || new Set(clips.map(clip => clip.id)).size !== clips.length) {
      throw new RangeError("Invalid pattern group");
    }
    this.tracks = input.tracks.map(track => track.id);
    this.input = { pointerId: input.pointerId, stamp: { ...input.stamp }, replacements: input.replacements,
      originTick: input.originTick, originTrack: input.originTrack, quantum: input.quantum };
    let firstTick = MAX_SONG_TICKS, lastTick = 0, firstTrack = this.tracks.length, lastTrack = 0;
    this.clips = clips.map(clip => {
      validatePattern(clip);
      const { id, startTick, trackIndex } = clip;
      if (id === null || trackIndex >= this.tracks.length) throw new RangeError("Invalid pattern group member");
      firstTick = Math.min(firstTick, startTick);
      lastTick = Math.max(lastTick, startTick + clip.content.lengthTicks);
      firstTrack = Math.min(firstTrack, trackIndex);
      lastTrack = Math.max(lastTrack, trackIndex);
      return { id, startTick, trackIndex };
    });
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
