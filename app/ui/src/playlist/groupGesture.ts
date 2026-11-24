import type { ArrangementOperation } from "../api/arrangement";
import { PatternGesture, type GestureGuard, type GestureInput } from "./gesture";
import { clipRect, MAX_SONG_TICKS, snapTick, type PatternPreview, type PlaylistView, type Point, type Rect } from "./geometry";

export const togglesSelection = (event: { shiftKey: boolean; ctrlKey: boolean; metaKey: boolean }): boolean => event.shiftKey || event.ctrlKey || event.metaKey;
export const crossedThreshold = (start: Point, end: Point): boolean => Math.hypot(end.x - start.x, end.y - start.y) >= 4;
export function toggleSelection(ids: readonly number[], id: number): number[] {
  return ids.includes(id) ? ids.filter(value => value !== id) : [...ids, id];
}
export function marqueeRect(start: Point, end: Point): Rect {
  return { x: Math.min(start.x, end.x), y: Math.min(start.y, end.y), width: Math.abs(start.x - end.x), height: Math.abs(start.y - end.y) };
}
export function marqueeSelection(clips: readonly PatternPreview[], rect: Rect, view: PlaylistView): number[] {
  return clips.flatMap(clip => {
    const bounds = clipRect(clip, view);
    return clip.id !== null && bounds.x < rect.x + rect.width && bounds.x + bounds.width > rect.x && bounds.y < rect.y + rect.height && bounds.y + bounds.height > rect.y ? [clip.id] : [];
  });
}
/** One shared constrained delta: independently clamping each clip would deform the group. */
export class PatternGroupGesture {
  private readonly gestures: PatternGesture[];
  private readonly clips: readonly PatternPreview[];
  constructor(private readonly input: GestureInput, clips: readonly PatternPreview[]) {
    if (!clips.length || !["move", "copy"].includes(input.kind) || new Set(clips.map(clip => clip.id)).size !== clips.length) throw new RangeError("Invalid pattern group");
    this.clips = clips.map(clip => ({ ...clip, content: { ...clip.content } }));
    this.gestures = this.clips.map(clip => new PatternGesture({ ...input, quantum: 1, clip }));
  }
  get previews(): PatternPreview[] { return this.gestures.map(gesture => gesture.preview); }
  get preview(): PatternPreview { return this.previews[0]; }
  get sliceTick(): null { return null; }
  cancel(): void { this.gestures.forEach(gesture => gesture.cancel()); }
  update(tick: number, row: number): void {
    const delta = Math.max(-Math.min(...this.clips.map(c => c.startTick)), Math.min(snapTick(tick - this.input.originTick, this.input.quantum, -MAX_SONG_TICKS), MAX_SONG_TICKS - Math.max(...this.clips.map(c => c.startTick + c.content.lengthTicks))));
    const trackDelta = Math.max(-Math.min(...this.clips.map(c => c.trackIndex)), Math.min(row - this.input.originTrack, this.input.tracks.length - 1 - Math.max(...this.clips.map(c => c.trackIndex))));
    // Boundaries may be off-grid. Children use a unit grid after the common snap.
    this.gestures.forEach(gesture => gesture.update(this.input.originTick + delta, this.input.originTrack + trackDelta));
  }
  take(guard: GestureGuard): readonly ArrangementOperation[] | null {
    const batches = this.gestures.map(gesture => gesture.take(guard));
    return batches.some(batch => batch === null) ? null : batches.flatMap(batch => batch!);
  }
}
