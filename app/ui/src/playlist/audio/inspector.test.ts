import { describe, expect, it } from "vitest";
import type { AudioContent } from "../../api/arrangement";
import { audioInspectorEdit, audioInspectorInfo, formatFade } from "./inspector";

const content: AudioContent = {
  type: "audio", asset: 4, sourceStart: 22050, sourceEnd: 66150,
  gain: 0.75, reverse: false, fadeIn: 441, fadeOut: 882,
};
const clip = { id: 5, startTick: 960, content };
const asset = { sourceRate: 44100, sourceFrames: 88200, originalTempo: 99.5, originalKey: { root: 1, scale: "minor" } };

describe("audio inspector edits", () => {
  it.each([[1.25, 1.25], [-1, 0], [3, 2]])("maps gain %s to %s, preserving the clip and source bounds", (input, gain) => {
    expect(audioInspectorEdit(clip, { field: "gain", value: input })).toEqual({
      kind: "resize", clip: 5, startTick: 960, content: { ...content, gain },
    });
  });

  it.each(["fadeIn", "fadeOut"] as const)("maps %s to integer frames within the trimmed length", field => {
    for (const [value, frames] of [[-5, 0], [500.6, 501], [44100, 44100], [88200, 44100]]) {
      expect(audioInspectorEdit(clip, { field, value })).toEqual({
        kind: "resize", clip: 5, startTick: 960, content: { ...content, [field]: frames },
      });
    }
  });

  it("sets reverse in either direction without swapping physical source bounds or fades", () => {
    for (const reverse of [true, false]) {
      expect(audioInspectorEdit({ ...clip, content: { ...content, reverse: !reverse } }, { field: "reverse", value: reverse })).toEqual({
        kind: "resize", clip: 5, startTick: 960, content: { ...content, reverse },
      });
    }
  });

  it("skips unchanged and non-finite values", () => {
    expect(audioInspectorEdit(clip, { field: "reverse", value: false })).toBeNull();
    expect(audioInspectorEdit(clip, { field: "gain", value: 0.75 })).toBeNull();
    expect(audioInspectorEdit(clip, { field: "fadeIn", value: 441.1 })).toBeNull();
    expect(audioInspectorEdit(clip, { field: "gain", value: NaN })).toBeNull();
    expect(audioInspectorEdit(clip, { field: "fadeOut", value: Infinity })).toBeNull();
  });
});

describe("audio inspector display", () => {
  it("displays frames as milliseconds at the source rate", () => {
    expect(formatFade(441, 44100)).toBe("10 ms");
    expect(formatFade(1, 44100)).toBe("0.023 ms");
    expect(formatFade(44100, 44100)).toBe("1000 ms");
  });

  it("keeps one second of audio at 44.1 kHz while the timeline duration changes with tempo and meter", () => {
    expect(audioInspectorInfo(content, asset, 120, 4, 4)).toEqual({
      seconds: "1 s", musicalLength: "2 beats · 0.5 bars", originalTempo: "99.5 BPM", originalKey: "C# minor",
    });
    expect(audioInspectorInfo(content, asset, 137.1, 4, 4)).toMatchObject({
      seconds: "1 s", musicalLength: "2.285 beats · 0.571 bars",
    });
    expect(audioInspectorInfo(content, asset, 120, 3, 8)).toMatchObject({
      seconds: "1 s", musicalLength: "4 beats · 1.333 bars",
    });
  });

  it("displays absent tempo and key as em dashes", () => {
    expect(audioInspectorInfo(content, { ...asset, originalTempo: null, originalKey: null }, 120, 4, 4))
      .toMatchObject({ originalTempo: "—", originalKey: "—" });
  });
});
