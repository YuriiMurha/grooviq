import { describe, expect, it } from "vitest";
import { MIN_REGION, decodePeaks, dragMarker, markerAt, markerPosition, markerRange, peaksToPoints, type Marker, type Region } from "./waveform";

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
    // Start and end never cross: each stops a minimum region short of the other.
    expect(dragMarker("start", 0.9, r)).toBeCloseTo(0.6 - MIN_REGION, 9);
    expect(dragMarker("end", 0.1, r)).toBeCloseTo(0.2 + MIN_REGION, 9);
    expect(dragMarker("start", 0.3, r)).toBe(0.3);
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
      const region: Region = { start: Math.min(a, b), end: Math.max(a, b) + MIN_REGION, loopStart: rnd(), reverse: rnd() < 0.5 };
      if (region.end > 1) continue;
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
});
