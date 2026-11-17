import { describe, expect, it } from "vitest";
import { Playhead, toScreen, toWorld, zoomAt, type RollView } from "./view";

const view: RollView = { tickPx: 0.125, keyPx: 14, scrollTick: 960, topKey: 84.5, width: 900, height: 420 };

describe("roll view", () => {
  it("world_and_screen_round_trip", () => {
    // The top-left corner is the scroll position.
    expect(toWorld(view, 0, 0)).toEqual({ tick: 960, key: 84.5 });
    // A key row is keyPx tall and keys go up the screen: key 84 spans y -7..7, key 83 7..21.
    expect(toScreen(view, 960, 85)).toEqual({ x: 0, y: -7 });
    expect(toScreen(view, 960, 84)).toEqual({ x: 0, y: 7 });
    expect(Math.floor(toWorld(view, 10, 6).key)).toBe(84);
    expect(Math.floor(toWorld(view, 10, 8).key)).toBe(83);
    expect(Math.floor(toWorld(view, 10, 20).key)).toBe(83);
    // 0.125 px a tick: a step (240 ticks) is 30 px.
    expect(toScreen(view, 960 + 240, 84).x).toBe(30);

    for (const [x, y] of [
      [0, 0],
      [13.25, 400],
      [899, 1],
      [450.5, 210.75],
    ]) {
      const w = toWorld(view, x, y);
      const s = toScreen(view, w.tick, w.key);
      expect(s.x).toBeCloseTo(x, 9);
      expect(s.y).toBeCloseTo(y, 9);
    }
    for (const [tick, key] of [
      [0, 0],
      [3840, 60],
      [61439, 127],
    ]) {
      const s = toScreen(view, tick, key);
      const w = toWorld(view, s.x, s.y);
      expect(w.tick).toBeCloseTo(tick, 9);
      expect(w.key).toBeCloseTo(key, 9);
    }
  });

  it("zoom_keeps_the_tick_under_the_cursor", () => {
    const far = { ...view, scrollTick: 20000 };
    for (const factor of [2, 0.5, 1.37, 0.81]) {
      for (const x of [0, 120, 333.3, 899]) {
        const before = toWorld(far, x, 50).tick;
        const zoomed = zoomAt(far, factor, x);
        expect(zoomed.tickPx).toBeCloseTo(view.tickPx * factor, 12);
        expect(toWorld(zoomed, x, 50).tick).toBeCloseTo(before, 6);
        // Keys and the size do not move.
        expect([zoomed.keyPx, zoomed.topKey, zoomed.width, zoomed.height]).toEqual([14, 84.5, 900, 420]);
      }
    }
    // Zoom stops at its limits, and never scrolls before the pattern's start.
    expect(zoomAt(view, 1e9, 400).tickPx).toBeLessThan(1e6);
    expect(zoomAt(view, 1e-9, 400).tickPx).toBeGreaterThan(0);
    const out = zoomAt({ ...view, scrollTick: 0 }, 0.5, 400);
    expect(out.scrollTick).toBe(0);
  });

  it("the_playhead_follows_raf_timestamps_at_60_and_120_hz", () => {
    // 120 BPM: 16 * 120 ticks a second.
    const ticksPerMs = (16 * 120) / 1000;
    const loop = 16 * 240;
    // Live frames arrive about every 20 ms with a little jitter; each carries the engine's
    // position at the moment it was sent.
    const frames = Array.from({ length: 60 }, (_, k) => {
      const sent = 20 * k;
      return { tick: Math.floor(sent * ticksPerMs), receivedAt: sent + ((k * 7) % 5) * 0.4 };
    });

    /** The playhead on every animation frame of a display refreshing every `interval` ms. */
    const run = (count: number, stamp: (i: number) => number) => {
      const p = new Playhead();
      const out = new Map<number, number | null>();
      let next = 0;
      for (let i = 0; i < count; i++) {
        const now = stamp(i);
        while (next < frames.length && frames[next].receivedAt <= now) {
          p.observe(frames[next].tick, frames[next].receivedAt, ticksPerMs);
          next++;
        }
        out.set(now, p.at(now, loop));
      }
      return out;
    };
    // Exact shared timestamps: (2i) * 25 / 3 === i * 50 / 3.
    const at60 = run(70, (i) => (i * 50) / 3);
    const at120 = run(140, (i) => (i * 25) / 3);

    let shared = 0;
    for (const [now, tick] of at60) {
      expect(at120.has(now)).toBe(true);
      expect(at120.get(now)).toBe(tick);
      shared++;
    }
    expect(shared).toBe(70);

    // The playhead is the last frame's position carried forward by the elapsed time.
    const now = 500;
    const last = frames.filter((f) => f.receivedAt <= now).at(-1)!;
    expect(at120.get(now)).toBeCloseTo((last.tick + (now - last.receivedAt) * ticksPerMs) % loop, 9);
    // It moves on every frame at 120 Hz, not only when a live frame arrives.
    const stamps = [...at120.keys()].filter((t) => t > 100 && t < 900);
    for (let i = 1; i < stamps.length; i++) {
      expect(at120.get(stamps[i])).not.toBe(at120.get(stamps[i - 1]));
    }

    // Before any frame, and after a stop, there is no playhead.
    const p = new Playhead();
    expect(p.at(10, loop)).toBeNull();
    p.observe(100, 0, ticksPerMs);
    expect(p.at(10, loop)).toBeCloseTo(100 + 10 * ticksPerMs, 9);
    p.stop();
    expect(p.at(20, loop)).toBeNull();
  });
});
