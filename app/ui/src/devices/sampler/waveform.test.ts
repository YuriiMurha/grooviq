import { describe, expect, it } from "vitest";
import { decodePeaks, dragMarker, f32Above, f32Below, markerAt, markerPosition, markerRange, nudgeMarker, peaksToPoints, type Marker, type Region } from "./waveform";

/** The polygon as [x, y] pairs. */
const pairs = (pts: Float32Array) => Array.from({ length: pts.length / 2 }, (_, i) => [pts[2 * i], pts[2 * i + 1]]);

describe("waveform", () => {
  it("points_trace_both_edges", () => {
    // Four (min, max) pairs, one column each: the top edge runs left to right through the
    // maxima, the bottom edge back right to left through the minima.
    const peaks = new Float32Array([-0.5, 0.5, -1, 1, -0.25, 0.75, 0, 0]);
    expect(pairs(peaksToPoints(peaks, 4, 100))).toEqual([
      [0.5, 25],
      [1.5, 0],
      [2.5, 12.5],
      [3.5, 50],
      [3.5, 50],
      [2.5, 62.5],
      [1.5, 100],
      [0.5, 75],
    ]);

    // Fewer columns than pairs: each column spans its pairs' lowest minimum and highest maximum.
    expect(pairs(peaksToPoints(peaks, 2, 100))).toEqual([
      [0.5, 0],
      [1.5, 12.5],
      [1.5, 62.5],
      [0.5, 100],
    ]);

    // More columns than pairs: each column reads the pair under it.
    const wide = pairs(peaksToPoints(peaks, 8, 100));
    expect(wide.length).toBe(16);
    expect(wide.slice(0, 8).map(([, y]) => y)).toEqual([25, 25, 0, 0, 12.5, 12.5, 50, 50]);
    expect(wide.slice(8).map(([, y]) => y)).toEqual([50, 50, 62.5, 62.5, 100, 100, 75, 75]);
    // The bottom edge retraces the top edge's x positions backwards.
    expect(wide.slice(8).map(([x]) => x)).toEqual(wide.slice(0, 8).map(([x]) => x).reverse());

    // A sample past full scale stays inside the box; a fractional width keeps its columns.
    const loud = pairs(peaksToPoints(new Float32Array([-3, 2]), 1, 10));
    expect(loud).toEqual([
      [0.5, 0],
      [0.5, 10],
    ]);
    expect(pairs(peaksToPoints(peaks, 3, 60)).map(([x]) => x)).toEqual([0.5, 1.5, 2.5, 2.5, 1.5, 0.5]);

    // Nothing to draw: no points.
    expect(peaksToPoints(new Float32Array(0), 100, 50).length).toBe(0);
    expect(peaksToPoints(peaks, 0, 50).length).toBe(0);
  });

  it("decodes_the_little_endian_peak_pairs", () => {
    const buf = new ArrayBuffer(16);
    const v = new DataView(buf);
    [-0.5, 0.25, -1, 1].forEach((x, i) => v.setFloat32(i * 4, x, true));
    expect(Array.from(decodePeaks(buf))).toEqual([-0.5, 0.25, -1, 1]);
    // A torn buffer keeps its whole pairs.
    expect(decodePeaks(buf.slice(0, 12)).length).toBe(2);
  });

  it("markers_stay_ordered", () => {
    expect(markerAt(25, 100)).toBe(0.25);
    expect(markerAt(-5, 100)).toBe(0);
    expect(markerAt(150, 100)).toBe(1);
    expect(markerAt(10, 0)).toBe(0);

    const r: Region = { start: 0.2, end: 0.6, loopStart: 0.5, reverse: false };
    // Start and end never cross: each stops one f32 step short of the other (the engine holds
    // f32 values, and plays at least one frame of a region narrower than that).
    expect(dragMarker("start", 0.9, r)).toBe(f32Below(0.6));
    expect(dragMarker("start", 0.9, r)).toBeLessThan(Math.fround(0.6));
    expect(dragMarker("end", 0.1, r)).toBe(f32Above(0.2));
    expect(dragMarker("end", 0.1, r)).toBeGreaterThan(Math.fround(0.2));
    expect(dragMarker("start", 0.3, r)).toBe(Math.fround(0.3));
    expect(dragMarker("end", 2, r)).toBe(1);
    expect(dragMarker("start", -1, r)).toBe(0);

    // The loop point is a fraction of the played region (in play order), so it is always
    // inside the region whatever the pointer does.
    expect(markerPosition("loop_start", r)).toBeCloseTo(0.4, 9);
    expect(dragMarker("loop_start", 0.5, r)).toBeCloseTo(0.75, 9);
    expect(dragMarker("loop_start", 0.95, r)).toBe(1);
    expect(dragMarker("loop_start", 0.05, r)).toBe(0);
    // Reversed, the region plays from its end: the loop point counts back from there.
    const rev = { ...r, reverse: true };
    expect(markerPosition("loop_start", rev)).toBeCloseTo(0.4, 9);
    expect(markerPosition("loop_start", { ...rev, loopStart: 0.25 })).toBeCloseTo(0.5, 9);
    expect(dragMarker("loop_start", 0.5, rev)).toBeCloseTo(0.25, 9);

    // Stored out of order (the engine plays either order): the first drag puts them in order.
    const crossed: Region = { start: 0.8, end: 0.2, loopStart: 0, reverse: false };
    expect(dragMarker("start", 0.9, crossed)).toBeLessThan(0.2);
    expect(dragMarker("end", 0.1, crossed)).toBeGreaterThan(0.8);
    expect(markerPosition("loop_start", crossed)).toBeCloseTo(0.2, 9);

    // Any drag of any marker, from any region: start stays before end, the loop point inside.
    let seed = 7;
    const rnd = () => ((seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648);
    for (let i = 0; i < 2000; i++) {
      const a = rnd();
      const b = rnd();
      const region: Region = { start: Math.fround(Math.min(a, b)), end: f32Above(Math.max(a, b)), loopStart: rnd(), reverse: rnd() < 0.5 };
      const marker = (["start", "end", "loop_start"] as Marker[])[i % 3];
      const at = rnd() * 1.4 - 0.2;
      const value = dragMarker(marker, at, region);
      const [lo, hi] = markerRange(marker, region);
      expect(value).toBeGreaterThanOrEqual(lo);
      expect(value).toBeLessThanOrEqual(hi);
      const next = { ...region, [marker === "loop_start" ? "loopStart" : marker]: value };
      expect(next.start).toBeLessThan(next.end);
      const loop = markerPosition("loop_start", next);
      expect(loop).toBeGreaterThanOrEqual(next.start - 1e-9);
      expect(loop).toBeLessThanOrEqual(next.end + 1e-9);
    }
  });

  it("narrow_regions_stay_as_stored_and_nudges_never_reverse", () => {
    // The f32 neighbours, as the engine stores parameters.
    expect(f32Below(Math.fround(0.5))).toBe(Math.fround(0.5) - 2 ** -25);
    expect(f32Above(Math.fround(0.5))).toBe(Math.fround(0.5) + 2 ** -24);
    expect(f32Below(0)).toBe(0);
    expect(f32Above(1)).toBe(1);
    expect(f32Above(0)).toBeGreaterThan(0);
    // A value between two f32s: the neighbours of the f32 the engine stores for it.
    expect(f32Below(0.1)).toBeLessThan(Math.fround(0.1));
    expect(f32Above(0.1)).toBeGreaterThan(Math.fround(0.1));

    // A region a fifth of a thousandth wide (a few frames of a long sample) is valid: its range
    // contains it, and a nudge moves inwards by up to the step, never outwards past the other.
    const narrow: Region = { start: Math.fround(0.1), end: Math.fround(0.1002), loopStart: 0, reverse: false };
    for (const m of ["start", "end"] as const) {
      const [lo, hi] = markerRange(m, narrow);
      expect(narrow[m]).toBeGreaterThanOrEqual(lo);
      expect(narrow[m]).toBeLessThanOrEqual(hi);
    }
    const right = nudgeMarker("start", narrow, narrow.start, 0.001);
    expect(right).toBeGreaterThan(narrow.start);
    expect(right).toBeLessThan(narrow.end);
    const left = nudgeMarker("end", narrow, narrow.end, -0.001);
    expect(left).toBeLessThan(narrow.end);
    expect(left).toBeGreaterThan(narrow.start);
    // Outwards they move freely.
    expect(nudgeMarker("start", narrow, narrow.start, -0.001)).toBe(Math.fround(0.1 - 0.001));
    expect(nudgeMarker("end", narrow, narrow.end, 0.001)).toBe(Math.fround(0.1002 + 0.001));
    // Already one step apart: an inward nudge has nowhere to go and stays.
    const tight: Region = { start: Math.fround(0.3), end: f32Above(Math.fround(0.3)), loopStart: 0, reverse: false };
    expect(nudgeMarker("start", tight, tight.start, 0.001)).toBe(tight.start);
    expect(nudgeMarker("end", tight, tight.end, -0.001)).toBe(tight.end);
    // Stored out of order (the engine plays either order): a nudge never jumps the other way.
    const crossed: Region = { start: Math.fround(0.8), end: Math.fround(0.2), loopStart: 0, reverse: false };
    expect(nudgeMarker("start", crossed, crossed.start, 0.001)).toBe(crossed.start);
    expect(nudgeMarker("start", crossed, crossed.start, Infinity)).toBe(crossed.start);
    expect(nudgeMarker("start", crossed, crossed.start, -0.001)).toBe(f32Below(crossed.end));
    expect(nudgeMarker("end", crossed, crossed.end, -Infinity)).toBe(crossed.end);
    // Home and End go to the ends of the range, in their own direction.
    expect(nudgeMarker("start", narrow, narrow.start, -Infinity)).toBe(0);
    expect(nudgeMarker("start", narrow, narrow.start, Infinity)).toBe(f32Below(narrow.end));
    expect(nudgeMarker("end", narrow, narrow.end, Infinity)).toBe(1);

    // Every nudge of every marker, from any valid region: towards its sign or not at all, an
    // f32, inside its range, and start stays before end.
    let seed = 11;
    const rnd = () => ((seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648);
    for (let i = 0; i < 3000; i++) {
      const a = Math.fround(rnd());
      const w = [0, 2 ** -20, 1e-4, 1e-3, 0.05][i % 5];
      const region: Region = { start: a, end: Math.min(1, f32Above(a + w)), loopStart: Math.fround(rnd()), reverse: rnd() < 0.5 };
      if (!(region.start < region.end)) continue;
      const marker = (["start", "end", "loop_start"] as Marker[])[i % 3];
      const value = marker === "loop_start" ? region.loopStart : region[marker];
      const delta = (rnd() - 0.5) * 0.01;
      const next = nudgeMarker(marker, region, value, delta);
      expect(Math.sign(next - value) === Math.sign(delta) || next === value).toBe(true);
      expect(Math.fround(next)).toBe(next);
      const [lo, hi] = markerRange(marker, region);
      if (next !== value) expect(next >= lo && next <= hi).toBe(true);
      const moved = { ...region, [marker === "loop_start" ? "loopStart" : marker]: next };
      expect(moved.start).toBeLessThan(moved.end);
    }
  });
});
