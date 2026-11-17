import { describe, expect, it } from "vitest";
import { lfoWave, oscWave } from "./waveShapes";

// The synth builds its four tables from these sine series (dsp/src/wavetable.rs `coef`) and
// blends neighbouring tables linearly (`Morph`: k = min(floor(shape), 2), t = shape - k).
// Summed here independently of the display code, with enough harmonics that the band limit
// is far below the tolerance away from the jumps.
const HARMONICS = 20_000;
function coef(shape: number, k: number): number {
  if (shape === 0) return k === 1 ? 1 : 0;
  if (shape === 1) return k % 2 === 1 ? ((Math.floor(k / 2) % 2 === 0 ? 1 : -1) * 8) / (Math.PI * Math.PI * k * k) : 0;
  if (shape === 2) return 2 / (Math.PI * k);
  return k % 2 === 1 ? 4 / (Math.PI * k) : 0;
}
function series(shape: number, phase: number): number {
  let sum = 0;
  for (let k = 1; k <= HARMONICS; k++) sum += coef(shape, k) * Math.sin(2 * Math.PI * k * phase);
  return sum;
}
function synthMorph(shape: number, phase: number): number {
  const s = Math.min(3, Math.max(0, shape));
  const k = Math.min(2, Math.floor(s));
  const t = s - k;
  return series(k, phase) * (1 - t) + (t > 0 ? series(k + 1, phase) * t : 0);
}

const PHASES = [0, 0.05, 0.125, 0.25, 0.375, 0.45, 0.5, 0.55, 0.625, 0.75, 0.875, 0.95];

describe("oscillator shapes", () => {
  it("the_shape_morph_matches_the_synth", () => {
    for (const shape of [0, 1, 2, 3, 1.5, 0.25, 2.75]) {
      for (const phase of PHASES) {
        expect(oscWave(shape, phase), `shape ${shape} at phase ${phase}`).toBeCloseTo(synthMorph(shape, phase), 2);
      }
    }
  });

  it("shapes_outside_the_range_clamp_and_phase_wraps", () => {
    expect(oscWave(-1, 0.25)).toBeCloseTo(oscWave(0, 0.25), 6);
    expect(oscWave(9, 0.25)).toBeCloseTo(oscWave(3, 0.25), 6);
    expect(oscWave(2, 1.25)).toBeCloseTo(oscWave(2, 0.25), 6);
  });
});

describe("LFO shapes", () => {
  it("the_lfo_shapes_match_the_synth", () => {
    // dsp/src/synth.rs `Lfo::next`: sine, triangle, a falling saw and a square.
    expect(lfoWave(0, 0.25)).toBeCloseTo(1, 6);
    expect(lfoWave(1, 0.125)).toBeCloseTo(0.5, 6);
    expect(lfoWave(1, 0.5)).toBeCloseTo(0, 6);
    expect(lfoWave(2, 0)).toBeCloseTo(1, 6);
    expect(lfoWave(2, 0.75)).toBeCloseTo(-0.5, 6);
    expect(lfoWave(3, 0.25)).toBe(1);
    expect(lfoWave(3, 0.75)).toBe(-1);
  });
});
