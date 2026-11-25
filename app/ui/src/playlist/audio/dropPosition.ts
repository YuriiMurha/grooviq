import { localPoint, MAX_SONG_TICKS, snapTick, tickAtX, type PlaylistView, type Point } from "../geometry";

export type DropPlatform = "macos" | "windows" | "linux";

export function dropClientPoint(position: Point, devicePixelRatio: number, platform: DropPlatform): Point {
  switch (platform) {
    case "macos":
      // Wry may label Cocoa points PhysicalPosition (#636). Until verified, fall through to physical / DPR.
    case "windows":
    case "linux":
      return { x: position.x / devicePixelRatio, y: position.y / devicePixelRatio };
  }
}

export function hitAudioDrop(
  client: Point,
  rect: Readonly<{ left: number; top: number; width: number; height: number }>,
  view: PlaylistView,
  tracks: readonly Readonly<{ id: number }>[],
  quantum: number,
): { track: number; startTick: number } | null {
  if (!(client.x >= rect.left && client.x - rect.left < rect.width &&
        client.y >= rect.top && client.y - rect.top < rect.height)) return null;

  const point = localPoint(client, rect, view);
  const track = tracks[Math.floor(point.y / view.rowHeight + view.scrollTrack)];
  const tick = tickAtX(point.x, view);
  if (!track || tick >= MAX_SONG_TICKS) return null;

  // Native import requires startTick < MAX_SONG_TICKS, including after rounding.
  const lastGridTick = Math.floor((MAX_SONG_TICKS - 1) / quantum) * quantum;
  return { track: track.id, startTick: snapTick(tick, quantum, 0, lastGridTick) };
}
