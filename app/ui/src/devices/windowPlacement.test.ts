import { describe, expect, it } from "vitest";
import { DESIGN_H, DESIGN_W, clampPlacement, containPlacement, fitScale, initialPlacement } from "./windowPlacement";

describe("device window placement", () => {
  it("never scales above the design size", () => {
    expect(fitScale(1920, 1080)).toBe(1);
  });

  it("draws at design size at the supported minimum", () => {
    expect(fitScale(900, 560)).toBe(1);
  });

  it("scales down between the supported minimum and the design size", () => {
    expect(fitScale(890, 540)).toBeCloseTo(870 / 880, 6);
  });

  it("scales down when height limits the fit", () => {
    expect(fitScale(900, 530)).toBeCloseTo(510 / 520, 6);
  });

  it("a_dragged_window_stays_reachable", () => {
    const p = clampPlacement({ x: -5000, y: -40 }, 1280, 800, 1);
    expect(p.x + DESIGN_W).toBeGreaterThanOrEqual(80);
    expect(p.y).toBe(0);
    const q = clampPlacement({ x: 5000, y: 5000 }, 1280, 800, 1);
    expect(q.x).toBeLessThanOrEqual(1280 - 80);
    expect(q.y).toBeLessThanOrEqual(800 - 80);
  });

  it.each([
    { width: 900, height: 560 },
    { width: 901, height: 561 },
    { width: 1279, height: 799 },
  ])("contains new cascades at $width × $height", ({ width, height }) => {
    const scale = fitScale(width, height);
    for (const index of [0, 1, 2, 40]) {
      const placement = initialPlacement(width, height, scale, index);
      expect(placement.x).toBeGreaterThanOrEqual(0);
      expect(placement.y).toBeGreaterThanOrEqual(0);
      expect(placement.x + DESIGN_W * scale).toBeLessThanOrEqual(width);
      expect(placement.y + DESIGN_H * scale).toBeLessThanOrEqual(height);
    }
  });

  it("anchors new windows when the minimum physical size cannot fit", () => {
    expect(initialPlacement(100, 80, fitScale(100, 80), 3)).toEqual({ x: 0, y: 0 });
  });

  it("windows_open_centred_then_cascade", () => {
    const first = initialPlacement(1280, 800, 1, 0);
    expect(first).toEqual({ x: (1280 - DESIGN_W) / 2, y: (800 - DESIGN_H) / 2 });
    const second = initialPlacement(1280, 800, 1, 1);
    expect(second.x).toBeGreaterThan(first.x);
    expect(second.y).toBeGreaterThan(first.y);
  });
});

describe("resize containment", () => {
  it("contains retained and cascaded positions at the supported minimum", () => {
    const s = fitScale(900, 560);
    for (const x of [-800, 200, 520, 2000]) {
      const p = containPlacement({ x, y: 320 }, 900, 560, s);
      expect(p.x).toBeGreaterThanOrEqual(0);
      expect(p.y).toBeGreaterThanOrEqual(0);
      expect(p.x + DESIGN_W * s).toBeLessThanOrEqual(900);
      expect(p.y + DESIGN_H * s).toBeLessThanOrEqual(560);
    }
  });
  it("anchors impossible fits at the reachable top left", () => {
    expect(containPlacement({ x: 200, y: 200 }, 100, 80, fitScale(100, 80))).toEqual({ x: 0, y: 0 });
  });
});
