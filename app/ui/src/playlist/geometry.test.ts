import { describe, expect, it } from "vitest";
import { clipRect, hitPattern, localPoint, MAX_SONG_TICKS, resizePattern, snapTick, tickAtX, type PatternPreview, type PlaylistView } from "./geometry";

const view: PlaylistView = { width: 800, height: 256, tickWidth: 0.1, scrollTick: 960, scrollTrack: 0, rowHeight: 64 };
const clip: PatternPreview = { id: 12, trackIndex: 1, name: "Beat", startTick: 1920, muted: false, content: { type: "pattern", pattern: 7, offsetTick: 480, lengthTicks: 1920 } };

describe("playlist coordinates and bounds", () => {
  it("snaps musical ticks before clamping to the shared song bounds", () => {
    expect(snapTick(1550, 960)).toBe(1920);
    expect(snapTick(-200, 960)).toBe(0);
    expect(snapTick(MAX_SONG_TICKS + 500, 960)).toBe(MAX_SONG_TICKS);
    expect(() => snapTick(NaN, 960)).toThrow();
    expect(() => snapTick(100, 0)).toThrow();
    expect(() => snapTick(100, 1.5)).toThrow();
  });

  it("uses CSS coordinates once across scroll, zoom and a scaled canvas", () => {
    const point = localPoint({ x: 500, y: 168 }, { left: 100, top: 40, width: 1600, height: 512 }, view);
    expect(point).toEqual({ x: 200, y: 64 });
    expect(tickAtX(point.x, view)).toBe(2960);
    expect(tickAtX(200, { ...view, tickWidth: 0.2 })).toBe(1960);
    // A DPR2 drawing buffer has the same CSS-space hit point, not twice the tick.
    expect(localPoint({ x: 300, y: 104 }, { left: 100, top: 40, width: 800, height: 256 }, view)).toEqual(point);
    expect(() => localPoint({ x: 0, y: 0 }, { left: 0, top: 0, width: 0, height: 256 }, view)).toThrow();
  });

  it("hits the last painted clip with half-open rectangles and the nearest edge", () => {
    const a = clipRect(clip, view);
    expect(a).toEqual({ x: 96, y: 82, width: 192, height: 42 });
    const front = { ...clip, id: 13 };
    expect(hitPattern([clip, front], { x: 100, y: 100 }, view)).toEqual({ id: 13, edge: "start" });
    expect(hitPattern([clip], { x: 280, y: 100 }, view)).toEqual({ id: 12, edge: null });
    expect(hitPattern([clip], { x: 287, y: 100 }, view)).toEqual({ id: 12, edge: "end" });
    expect(hitPattern([clip], { x: 288, y: 100 }, view)).toBeNull();
    expect(hitPattern([clip], { x: 100, y: 124 }, view)).toBeNull();
    expect(hitPattern([clip], { x: 100, y: 70 }, view)).toBeNull();
    const outside = { ...clip, startTick: 12000 };
    expect(hitPattern([outside], { x: 1110, y: 100 }, view)).toBeNull();
  });
});

describe("pattern source phase under resizing", () => {
  it("moves onset and source offset together without retargeting the source", () => {
    const changed = resizePattern(clip, "start", 240, 960);
    expect(changed.startTick).toBe(2160);
    expect(changed.content).toEqual({ type: "pattern", pattern: 7, offsetTick: 720, lengthTicks: 1680 });
    expect(changed.startTick + changed.content.lengthTicks).toBe(3840);
    for (const songTick of [2160, 2400, 3100, 3839]) {
      const oldPhase = (clip.content.offsetTick + songTick - clip.startTick) % 960;
      const newPhase = (changed.content.offsetTick + songTick - changed.startTick) % 960;
      expect(newPhase).toBe(oldPhase);
    }
    expect(clip.content.offsetTick).toBe(480);
    expect(clip.startTick).toBe(1920);
  });

  it("keeps one tick, the original end, and the available stored left offset", () => {
    const lastTick = resizePattern(clip, "start", 99999, 960);
    expect(lastTick.startTick).toBe(3839);
    expect(lastTick.content.lengthTicks).toBe(1);
    const extended = resizePattern(clip, "start", -99999, 960);
    expect(extended.startTick).toBe(1440);
    expect(extended.content.offsetTick).toBe(0);
    expect(extended.content.lengthTicks).toBe(2400);
  });

  it("right resize retains phase and refuses to cross the shared song horizon", () => {
    const atEnd = { ...clip, startTick: MAX_SONG_TICKS - 10, content: { ...clip.content, lengthTicks: 10 } };
    expect(resizePattern(atEnd, "end", 500, 960).content.lengthTicks).toBe(10);
    expect(resizePattern(atEnd, "end", -500, 960).content.lengthTicks).toBe(1);
    expect(resizePattern(clip, "end", 240, 960).content.offsetTick).toBe(480);
    expect(() => resizePattern(clip, "end", Infinity, 960)).toThrow();
  });
});
