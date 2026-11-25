import type { ArrangementPage } from "../api/arrangement";
import { integer, MAX_SONG_TICKS, type PlaylistView } from "./geometry";

export type RenderClip = Readonly<{
  id: number | null; name: string; kind: "pattern" | "audio" | "automation";
  trackIndex: number; startTick: number; lengthTicks: number; ink: number; muted: boolean;
}>;
export function viewport(width: number, height: number, scrollLeft: number, scrollTop: number, pixelsPerBar: number, barTicks: number): PlaylistView {
  if (![width, height, pixelsPerBar, barTicks].every(n => Number.isFinite(n) && n > 0) || ![scrollLeft, scrollTop].every(n => Number.isFinite(n) && n >= 0)) throw new RangeError("Playlist input area is unavailable");
  return { width, height, scrollTick: scrollLeft * barTicks / pixelsPerBar, scrollTrack: scrollTop / 64, tickWidth: pixelsPerBar / barTicks, rowHeight: 64 };
}
/** Two exactly representable f32 parts; avoid losing one-tick input near the song horizon. */
export function splitTick(tick: number): [number, number] {
  if (!Number.isFinite(tick) || tick < 0 || tick > MAX_SONG_TICKS) throw new RangeError("Invalid timeline coordinate");
  const high = Math.floor(tick / 4096);
  return [high, tick - high * 4096];
}
export function projectClips(page: ArrangementPage, bpm: number): RenderClip[] {
  const tracks = new Map(page.tracks.map((track, index) => [track.id, { index, ink: track.color, mute: track.mute }]));
  const assets = new Map(page.audioAssets.map(asset => [asset.id, asset]));
  return page.clips.map(clip => {
    const track = tracks.get(clip.track);
    if (!track) throw new Error("A clip refers to an unavailable track. Refresh the playlist.");
    let length: number;
    if (clip.content.type === "audio") {
      const asset = assets.get(clip.content.asset);
      if (!asset || !(asset.sourceRate > 0) || !(bpm > 0)) throw new Error("Audio clip timing is unavailable. Refresh the playlist.");
      length = Math.ceil((clip.content.sourceEnd - clip.content.sourceStart) / asset.sourceRate * bpm * 16);
    } else length = clip.content.lengthTicks;
    integer(length, "visible clip length", 1); integer(clip.startTick, "visible clip onset");
    return { id: clip.id, name: clip.name, kind: clip.content.type, trackIndex: track.index, startTick: clip.startTick, lengthTicks: Math.min(length, MAX_SONG_TICKS - clip.startTick), ink: track.ink, muted: clip.muted || track.mute };
  });
}
export function extent(clips: readonly RenderClip[], tracks: number, view: PlaylistView, barTicks: number): { width: number; height: number } {
  const end = clips.reduce((max, clip) => Math.max(max, clip.startTick + clip.lengthTicks), 0);
  return { width: Math.max(view.width, Math.min(MAX_SONG_TICKS, end + 16 * barTicks) * view.tickWidth), height: Math.max(view.height, Math.min(256, tracks + 1) * view.rowHeight) };
}
export function hitClip(clips: readonly RenderClip[], x: number, y: number, view: PlaylistView): { clip: RenderClip; edge: "start" | "end" | null } | null {
  if (!Number.isFinite(x) || !Number.isFinite(y) || x < 0 || y < 0 || x >= view.width || y >= view.height) return null;
  for (let i = clips.length - 1; i >= 0; i--) {
    const clip = clips[i], left = (clip.startTick - view.scrollTick) * view.tickWidth, top = (clip.trackIndex - view.scrollTrack) * view.rowHeight + 18;
    const width = Math.max(1, clip.lengthTicks * view.tickWidth);
    if (x < left || x >= left + width || y < top || y >= top + 42) continue;
    const before = x - left, after = left + width - x;
    const edge = Math.min(before, after) <= Math.min(6, width / 2) ? before <= after ? "start" : "end" : null;
    return { clip, edge };
  }
  return null;
}
