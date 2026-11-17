import { describe, expect, it } from "vitest";
import { LiveCoalescer, dragValue, fromNorm, keyValue, quantize, toNorm, wheelValue, type ParamRange } from "./paramDrag";

const lin: ParamRange = { min: 0, max: 1, default: 0.5, curve: "lin", stepped: false };
const cutoff: ParamRange = { min: 20, max: 20000, default: 4000, curve: "log", stepped: false };
const octave: ParamRange = { min: -3, max: 3, default: 0, curve: "lin", stepped: true };
const glide: ParamRange = { min: 0, max: 1000, default: 0, curve: "sqr", stepped: false };

describe("parameter drags", () => {
  it("vertical_drag_shift_fine_and_clamp", () => {
    // Up is positive; 180 px cover a continuous range.
    expect(dragValue(lin, 0.5, 45, false)).toBeCloseTo(0.75, 6);
    expect(dragValue(lin, 0.5, -45, false)).toBeCloseTo(0.25, 6);
    // Shift moves five times slower.
    expect(dragValue(lin, 0.5, 45, true)).toBeCloseTo(0.55, 6);
    // Far past either end clamps.
    expect(dragValue(lin, 0.5, 5000, false)).toBe(1);
    expect(dragValue(lin, 0.5, -5000, false)).toBe(0);
    expect(dragValue(cutoff, 4000, 5000, false)).toBe(20000);
    expect(dragValue(cutoff, 4000, -5000, true)).toBe(20);
    // A log range moves by octaves: half the travel from the geometric middle reaches the top.
    expect(dragValue(cutoff, Math.sqrt(20 * 20000), 90, false)).toBeCloseTo(20000, 3);
    // A stepped range moves in whole steps, at least 14 px each.
    expect(dragValue(octave, 0, 13, false)).toBe(0);
    expect(dragValue(octave, 0, 28, false)).toBe(1);
    expect(dragValue(octave, 0, -60, false)).toBe(-2);
    expect(dragValue(octave, 0, 500, false)).toBe(3);
  });

  it("stepped_values_round_half_away_from_zero_like_rust", () => {
    // The synth's `set_param` uses f32::round: halves go away from zero.
    const semi: ParamRange = { min: -12, max: 12, default: 0, curve: "lin", stepped: true };
    for (const [v, want] of [
      [-1.5, -2],
      [1.5, 2],
      [-0.5, -1],
      [0.5, 1],
      [-2.5, -3],
      [2.5, 3],
      [-1.49, -1],
      [-2.51, -3],
      [-0.4, 0],
      [-2.999, -3],
      [-3.4, -3],
      [3.6, 3],
    ] as const) {
      expect(quantize(octave, v), `octave ${v}`).toBe(want);
    }
    expect(quantize(semi, -11.5)).toBe(-12);
    expect(quantize(semi, -12.5)).toBe(-12);
    expect(quantize(semi, 11.5)).toBe(12);
    // A drag that lands on a negative half step rounds the same way.
    expect(dragValue(octave, 0, -45, false)).toBe(-2);
  });

  it("norm_round_trips_on_every_curve", () => {
    for (const p of [lin, cutoff, glide]) {
      for (const n of [0, 0.1, 0.5, 0.9, 1]) expect(toNorm(p, fromNorm(p, n)), `${p.curve} ${n}`).toBeCloseTo(n, 6);
    }
    expect(fromNorm(glide, 0.5)).toBeCloseTo(250, 6);
    expect(fromNorm(octave, 0.6)).toBe(1);
  });

  it("keys_and_wheel_step_and_clamp", () => {
    expect(keyValue(lin, 0.5, "ArrowUp", false)).toBeCloseTo(0.51, 6);
    expect(keyValue(lin, 0.5, "ArrowLeft", true)).toBeCloseTo(0.498, 6);
    expect(keyValue(lin, 0.5, "PageUp", false)).toBeCloseTo(0.6, 6);
    expect(keyValue(lin, 0.5, "Home", false)).toBe(0);
    expect(keyValue(lin, 0.5, "End", false)).toBe(1);
    expect(keyValue(lin, 0.5, "KeyA", false)).toBeNull();
    expect(keyValue(octave, 3, "ArrowUp", false)).toBe(3);
    expect(keyValue(octave, 0, "ArrowDown", false)).toBe(-1);
    // Wheel down lowers; a stepped value moves one step per notch.
    expect(wheelValue(lin, 0.5, 100, false)).toBeLessThan(0.5);
    expect(wheelValue(octave, 0, -100, false)).toBe(1);
    expect(wheelValue(lin, 1, -100, false)).toBe(1);
  });
});

describe("live values", () => {
  it("live_values_go_out_at_most_once_per_frame", () => {
    const sent: [string, number][] = [];
    const frames: (() => void)[] = [];
    const live = new LiveCoalescer(
      (id, v) => sent.push([id, v]),
      (cb) => frames.push(cb) - 1,
      (h) => {
        frames[h] = () => {};
      },
    );
    live.push("a", 1);
    live.push("a", 2);
    live.push("b", 5);
    live.push("a", 3);
    expect(sent).toEqual([]);
    expect(frames.length).toBe(1);
    frames.shift()!();
    expect(sent).toEqual([
      ["a", 3],
      ["b", 5],
    ]);
    // A settled id (its gesture committed) is not sent late.
    live.push("a", 4);
    live.settle("a");
    frames.shift()?.();
    expect(sent.length).toBe(2);
    // Nothing pending: no frame is asked for.
    expect(frames.length).toBe(0);
  });
});
