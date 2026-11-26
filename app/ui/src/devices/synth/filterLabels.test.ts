import { describe, expect, it } from "vitest";
import { zeroDbLabel } from "./filterLabels";

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
