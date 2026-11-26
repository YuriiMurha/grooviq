import type { PatternContent } from "../api/arrangement";

export const MAX_SONG_TICKS = 38_400_000;
export type PatternPreview = Readonly<{
  /** Null is a local preview, never a native identity or an optimistic acknowledgement. */
  id: number | null;
  trackIndex: number;
  name: string;
  startTick: number;
  muted: boolean;
  content: Readonly<PatternContent>;
}>;
export type Point = Readonly<{ x: number; y: number }>;
export type Rect = Readonly<{ x: number; y: number; width: number; height: number }>;
export type PlaylistView = Readonly<{
  width: number; height: number; tickWidth: number; rowHeight: number;
  scrollTick: number; scrollTrack: number;
}>;

export function integer(value: number, name: string, min = 0, max = MAX_SONG_TICKS): void {
  if (!Number.isSafeInteger(value) || value < min || value > max) throw new RangeError(`Invalid ${name}`);
}
function finite(value: number): void {
  if (!Number.isFinite(value)) throw new RangeError("Invalid playlist coordinate");
}
export function snapTick(tick: number, quantum: number, min = 0, max = MAX_SONG_TICKS): number {
  finite(tick); integer(quantum, "snap", 1); integer(min, "minimum", -MAX_SONG_TICKS); integer(max, "maximum", min);
  return Math.max(min, Math.min(max, Math.round(tick / quantum) * quantum));
}
function validateView(view: PlaylistView): void {
  for (const n of [view.width, view.height, view.tickWidth, view.rowHeight]) {
    if (!Number.isFinite(n) || n <= 0) throw new RangeError("Invalid playlist viewport");
  }
  finite(view.scrollTick); finite(view.scrollTrack);
}
export function localPoint(client: Point, rect: { left: number; top: number; width: number; height: number }, view: PlaylistView): Point {
  validateView(view); finite(client.x); finite(client.y); finite(rect.left); finite(rect.top);
  if (!(rect.width > 0) || !(rect.height > 0) || !Number.isFinite(rect.width) || !Number.isFinite(rect.height)) throw new RangeError("Playlist has no input area");
  return { x: (client.x - rect.left) * view.width / rect.width, y: (client.y - rect.top) * view.height / rect.height };
}
export function tickAtX(x: number, view: PlaylistView): number {
  validateView(view); finite(x); return view.scrollTick + x / view.tickWidth;
}
export function validatePattern(clip: PatternPreview): void {
  if (clip.content.type !== "pattern") throw new RangeError("Not a pattern clip");
  if (clip.id !== null) integer(clip.id, "clip identity", 0, 0xffffffff);
  integer(clip.trackIndex, "track index", 0, 256);
  integer(clip.startTick, "clip start"); integer(clip.content.lengthTicks, "clip length", 1);
  integer(clip.content.offsetTick, "source offset"); integer(clip.content.pattern, "pattern identity", 0, 0xffffffff);
  if (clip.startTick + clip.content.lengthTicks > MAX_SONG_TICKS) throw new RangeError("Clip exceeds the song horizon");
}
export function clipRect(clip: PatternPreview, view: PlaylistView): Rect {
  validateView(view); validatePattern(clip);
  return projectClipRect({ ...clip, lengthTicks: clip.content.lengthTicks }, view);
}
export function projectClipRect(clip: { startTick: number; trackIndex: number; lengthTicks: number }, view: PlaylistView): Rect {
  return { x: (clip.startTick - view.scrollTick) * view.tickWidth, y: (clip.trackIndex - view.scrollTrack) * view.rowHeight + 18,
    width: Math.max(1, clip.lengthTicks * view.tickWidth), height: Math.min(42, Math.max(1, view.rowHeight - 22)) };
}
export function hitPattern(clips: readonly PatternPreview[], point: Point, view: PlaylistView): { id: number; edge: "start" | "end" | null } | null {
  validateView(view); finite(point.x); finite(point.y);
  if (point.x < 0 || point.x >= view.width || point.y < 0 || point.y >= view.height) return null;
  // Saved paint order, not a new sound priority: the last painted rectangle receives input.
  for (let i = clips.length - 1; i >= 0; i--) {
    const clip = clips[i]; if (clip.id === null) continue;
    const rect = clipRect(clip, view);
    if (point.x < rect.x || point.x >= rect.x + rect.width || point.y < rect.y || point.y >= rect.y + rect.height) continue;
    const start = point.x - rect.x, end = rect.x + rect.width - point.x, handle = Math.min(6, rect.width / 2);
    return { id: clip.id, edge: start <= handle && start <= end ? "start" : end <= handle ? "end" : null };
  }
  return null;
}
export function resizePattern(clip: PatternPreview, edge: "start" | "end", delta: number, sourceTicks: number): PatternPreview {
  validatePattern(clip); integer(delta, "resize delta", -MAX_SONG_TICKS);
  integer(sourceTicks, "pattern source duration", 1);
  if (edge === "end") return { ...clip, content: { ...clip.content, offsetTick: clip.content.offsetTick % sourceTicks,
    lengthTicks: Math.max(1, Math.min(MAX_SONG_TICKS - clip.startTick, clip.content.lengthTicks + delta)) } };
  const shift = Math.max(-Math.min(clip.startTick, clip.content.offsetTick), Math.min(delta, clip.content.lengthTicks - 1));
  return { ...clip, startTick: clip.startTick + shift, content: { ...clip.content,
    offsetTick: (clip.content.offsetTick + shift) % sourceTicks, lengthTicks: clip.content.lengthTicks - shift } };
}
