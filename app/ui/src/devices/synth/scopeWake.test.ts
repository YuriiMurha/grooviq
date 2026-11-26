import { describe, expect, it } from "vitest";
import type { SynthMeter } from "../../api/synthMeter";
import { SILENT_SCOPE_DISPLAY, scopeDisplayChanged } from "./scopeWake";

const live: SynthMeter = {
  instance: 10,
  frames: 120,
  scope: [0, 0.5, -0.5],
  levels: [0.4, 0.3, 1, 0.1, 0.6, 0.2],
  motion: { ampStage: "decay", filterStage: "release", lfoPhase: 0.2 },
};

describe("scope display changes", () => {
  it("ignores new snapshot clocks and motion for hidden dots", () => {
    const resting: SynthMeter = {
      ...live,
      scope: Array<number>(256).fill(0),
      levels: [0, 0, 0, 0.2, 0.6, 0.2],
    };
    const laterResting: SynthMeter = {
      ...resting,
      frames: 240,
      motion: { ampStage: "release", filterStage: "idle", lfoPhase: 0.8 },
    };
    const laterLive: SynthMeter = { ...live, frames: 240, scope: [...live.scope] };
    expect(scopeDisplayChanged(SILENT_SCOPE_DISPLAY, resting)).toBe(false);
    expect(scopeDisplayChanged(resting, laterResting)).toBe(false);
    expect(scopeDisplayChanged(live, laterLive)).toBe(false);
  });

  it("wakes for waveform, status, envelope or LFO changes", () => {
    const changed: SynthMeter[] = [
      { ...live, scope: [0, 0.5, -0.6] },
      { ...live, levels: [0.8, 0.3, 1, 0.1, 0.6, 0.2] },
      { ...live, levels: [0.4, 0.3, 2, 0.1, 0.6, 0.2] },
      { ...live, levels: [0.4, 0.3, 1, 0.1, 0.7, 0.2] },
      { ...live, levels: [0.4, 0.3, 1, 0.1, 0.6, 0.3] },
      { ...live, levels: [0.4, 0.3, 1, -0.1, 0.6, 0.2] },
      { ...live, motion: { ampStage: "sustain", filterStage: "release", lfoPhase: 0.2 } },
      { ...live, motion: { ampStage: "decay", filterStage: "idle", lfoPhase: 0.2 } },
      { ...live, motion: { ampStage: "decay", filterStage: "release", lfoPhase: 0.3 } },
      { ...live, motion: null },
    ];
    for (const next of changed) expect(scopeDisplayChanged(live, next)).toBe(true);
    expect(scopeDisplayChanged(live, SILENT_SCOPE_DISPLAY)).toBe(true);
  });

  it("ignores differences hidden by clipping or peak rounding", () => {
    const clipped = { ...live, levels: [0.4, 0.3, 1, 0.1, 1, 0.2], scope: [0, 1, -1] };
    expect(scopeDisplayChanged(clipped, {
      ...clipped,
      levels: [0.401, 0.35, 1, 0.1, 1.1, 0.2],
      scope: [0, 1.2, -1.1],
    })).toBe(false);
  });
});
