import { describe, expect, it } from "vitest";
import { filterAxis, zeroDbLabel } from "./filterLabels";
import { filterDb, FILTER_BP, FILTER_LP24 } from "./waveShapes";

describe("filter frequency axis", () => {
  it("reserves 16 px at the bottom and insets the tick baseline", () => {
    const axis = filterAxis(140);
    expect(axis.bandTop).toBe(124);
    expect(axis.tickBaseline).toBe(136);
    expect(zeroDbLabel(256, axis.bandTop, 0, 120).y).toBe(121);
  });

  it("preserves the plot's dB scale above the frequency band", () => {
    const { yd } = filterAxis(140);
    expect(yd(24)).toBe(10);
    expect(yd(0)).toBeCloseTo(55.6);
    expect(yd(-36)).toBe(124);
  });

  it.each([
    // The 1k label extends to the right of its grid line.
    ["LP24 at 400 Hz", FILTER_LP24, 400, 1300],
    ["BP at 80 Hz", FILTER_BP, 80, 10000],
  ] as const)("keeps the %s stopband out of the frequency labels", (_name, type, cutoff, sampleHz) => {
    const { bandTop, yd } = filterAxis(140);
    expect(yd(filterDb(type, sampleHz, cutoff, 0.05, 0))).toBe(bandTop);
  });
});

describe("zero dB label placement", () => {
  it("anchors at the edge farther from the cutoff", () => {
    expect(zeroDbLabel(172, 140, 85.9, 54.5)).toMatchObject({ x: 168, textAnchor: "end" });
    expect(zeroDbLabel(172, 140, 86, 54.5)).toMatchObject({ x: 4, textAnchor: "start" });
  });

  it("puts the baseline a glyph height and a gap below the zero line on either side", () => {
    expect(zeroDbLabel(256, 140, 0, 54.5).y).toBe(65.5);
    expect(zeroDbLabel(256, 140, 256, 54.5).y).toBe(65.5);
  });

  it("retains a bottom inset for a shallow SVG and stays in bounds at zero height", () => {
    expect(zeroDbLabel(172, 24, 70, 14.5).y).toBe(21);
    expect(zeroDbLabel(172, 0, 70, 0).y).toBe(0);
  });
});
