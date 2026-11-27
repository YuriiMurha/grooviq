import { describe, expect, it } from "vitest";
import { MAX_SONG_TICKS } from "../geometry";
import { automationPlacement, automationStartOptions, buildAutomationCreation, currentControlValue, selectedAutomationTrack } from "./creation";

const sampler = { id: 7, name: "Kick", kind: "sampler" as const, sampler: { fine: -12.375, tune: 0 }, synth: {} };
const synth = { id: 8, name: "Lead", kind: "synth" as const, sampler: {}, synth: { osc1_octave: -1, osc1_level: 0.625 } };
const fine = { type: "instrumentParam", channel: 7, instrument: "sampler", param: "fine" } as const;
const octave = { type: "instrumentParam", channel: 8, instrument: "synth", param: "osc1_octave" } as const;
const tracks = [{ id: 12 }, { id: 13 }];

describe("automation creation", () => {
  it("uses the exact native knob or numeric field value, including signed and stepped values", () => {
    expect(currentControlValue([sampler], fine, -12.375)).toBe(-12.375);
    expect(currentControlValue([synth], octave, -1)).toBe(-1);
    expect(currentControlValue([sampler], fine, -12.37)).toBeNull();
    expect(currentControlValue([{ ...sampler, sampler: { fine: Infinity } }], fine, Infinity)).toBeNull();
    expect(currentControlValue([{ ...sampler, sampler: { fine: 1_000 } }], fine, 1_000)).toBeNull();
  });

  it("requires a real channel of the right instrument and excludes non-Knob controls", () => {
    expect(currentControlValue([synth], fine, 0)).toBeNull();
    expect(currentControlValue([sampler], { ...fine, instrument: "synth" }, 0)).toBeNull();
    for (const param of ["reverse", "filter_type", "start", "unknown"]) {
      expect(currentControlValue([{ ...sampler, sampler: { [param]: 0 } }], { ...fine, param }, 0)).toBeNull();
    }
  });

  it("distinguishes a current playhead, enabled loop start and an explicit song start", () => {
    const loop = { startTick: 960, endTick: 2880, enabled: true };
    expect(automationStartOptions(1441, loop)).toEqual(["playhead", "loop"]);
    expect(automationStartOptions(null, loop)).toEqual(["loop", "song"]);
    expect(automationStartOptions(null, { ...loop, enabled: false })).toEqual(["song"]);
    expect(automationPlacement("playhead", 1441, loop, { beats: 7, unit: 8 })).toEqual({ startTick: 1441, lengthTicks: 3360 });
    expect(automationPlacement("loop", 1441, loop, { beats: 7, unit: 8 })?.startTick).toBe(960);
    expect(automationPlacement("playhead", null, loop, { beats: 4, unit: 4 })).toBeNull();
    expect(automationPlacement("song", null, null, { beats: 4, unit: 4 })?.startTick).toBe(0);
  });

  it("preserves a valid late start with positive remaining length and refuses an invalid tick", () => {
    const meter = { beats: 4, unit: 4 };
    expect(automationPlacement("playhead", MAX_SONG_TICKS - 2, null, meter)).toEqual({ startTick: MAX_SONG_TICKS - 2, lengthTicks: 2 });
    for (const invalid of [-1, 2.5, MAX_SONG_TICKS, Infinity]) {
      expect(automationPlacement("playhead", invalid, null, meter)).toBeNull();
    }
  });

  it("uses a selection on one live track and requires a choice for ambiguous or removed tracks", () => {
    expect(selectedAutomationTrack([{ track: 13 }, { track: 13 }], tracks)).toBe(13);
    expect(selectedAutomationTrack([{ track: 12 }, { track: 13 }], tracks)).toBeNull();
    expect(selectedAutomationTrack([{ track: 9 }], tracks)).toBeNull();
    expect(selectedAutomationTrack([], tracks)).toBeNull();
  });

  it("creates a source-local seed and batches a new track only with explicit empty-arrangement intent", () => {
    const placement = { startTick: 1441, lengthTicks: 3840 };
    const clip = { name: "Fine", startTick: placement.startTick, muted: false, content: {
      type: "automation", target: fine, priority: 0, offsetTick: 0, lengthTicks: placement.lengthTicks,
      points: [{ tick: 0, value: -12.375, mode: "linear" }],
    } };
    expect(buildAutomationCreation(fine, -12.375, "Fine", placement, tracks, 13)).toEqual([{ kind: "createClip", track: 13, ...clip }]);
    expect(buildAutomationCreation(fine, -12.375, "Fine", placement, [], "new")).toEqual([
      { kind: "createTrack", name: "Track 1", color: 0 },
      { kind: "createClipOnCreatedTrack", trackOperation: 0, ...clip },
    ]);
    expect(buildAutomationCreation(fine, -12.375, "Fine", placement, tracks, "new")).toBeNull();
    expect(buildAutomationCreation(fine, -12.375, "Fine", placement, tracks, 9)).toBeNull();
  });
});
