import { expect, it } from "vitest";
import { MAX_SONG_TICKS } from "../geometry";
import { viewport } from "../viewport";
import { dropClientPoint, hitAudioDrop, type DropPlatform } from "./dropPosition";

const platforms: DropPlatform[] = ["macos", "windows", "linux"];

it.each(platforms)("converts physical drop points on %s at DPR 1 and 2", (platform) => {
  const raw = { x: 401, y: 241 };
  expect(dropClientPoint(raw, 1, platform)).toEqual({ x: 401, y: 241 });
  expect(dropClientPoint(raw, 2, platform)).toEqual({ x: 200.5, y: 120.5 });
});

const rect = { left: 40, top: 100, width: 200, height: 192 };
const view = viewport(200, 192, 0, 0, 192, 3840);
const tracks = [{ id: 9 }, { id: 3 }, { id: 51 }];
const quantum = 480;

it.each([
  { x: 39, y: 100 }, { x: 240, y: 100 }, { x: 40, y: 99 }, { x: 40, y: 292 },
])("rejects a drop outside the canvas at $x, $y", (point) => {
  expect(hitAudioDrop(point, rect, view, tracks, quantum)).toBeNull();
});

it("snaps either side of the midpoint and preserves a grid line", () => {
  expect(hitAudioDrop({ x: 51.9, y: 100 }, rect, view, tracks, quantum)).toEqual({ track: 9, startTick: 0 });
  expect(hitAudioDrop({ x: 52, y: 100 }, rect, view, tracks, quantum)).toEqual({ track: 9, startTick: 480 });
  expect(hitAudioDrop({ x: 64, y: 100 }, rect, view, tracks, quantum)).toEqual({ track: 9, startTick: 480 });
});

it("uses the scrolled viewport and returns the last real track ID", () => {
  const scrolled = viewport(200, 192, 24, 64, 192, 3840);
  expect(hitAudioDrop({ x: 40, y: 227.9 }, rect, scrolled, tracks, quantum)).toEqual({ track: 51, startTick: 480 });
  expect(hitAudioDrop({ x: 40, y: 228 }, rect, scrolled, tracks, quantum)).toBeNull();
});

it("keeps the final grid slot inside the song and rejects drops at or past the horizon", () => {
  const atEnd = { ...view, scrollTick: MAX_SONG_TICKS - 480 };
  expect(hitAudioDrop({ x: 63, y: 100 }, rect, atEnd, tracks, quantum))
    .toEqual({ track: 9, startTick: MAX_SONG_TICKS - 480 });
  expect(hitAudioDrop({ x: 64, y: 100 }, rect, atEnd, tracks, quantum)).toBeNull();
  expect(hitAudioDrop({ x: 65, y: 100 }, rect, atEnd, tracks, quantum)).toBeNull();
});
