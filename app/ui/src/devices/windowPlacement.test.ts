import { describe, expect, it } from "vitest";
import { DESIGN_H, DESIGN_W, clampPlacement, containPlacement, fitScale, initialPlacement } from "./windowPlacement";

const HINT_BAR_HEIGHT = 32;

describe("device window placement", () => {
  it("never scales above the design size", () => {
    expect(fitScale(1920, 1080, HINT_BAR_HEIGHT)).toBe(1);
  });

  it("draws at design size at the supported minimum", () => {
    expect(fitScale(900, 560, HINT_BAR_HEIGHT)).toBe(1);
  });

  it("scales down below the supported minimum width", () => {
    expect(fitScale(890, 560, HINT_BAR_HEIGHT)).toBeCloseTo(870 / 880, 6);
  });

  it("scales down when the usable height limits the fit", () => {
    expect(fitScale(900, 550, HINT_BAR_HEIGHT)).toBeCloseTo(510 / 520, 6);
  });

  it("centres the first editor above the hint bar at the supported minimum", () => {
    expect(initialPlacement(900, 560, 1, 0, HINT_BAR_HEIGHT)).toEqual({ x: 10, y: 4 });
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
    const scale = fitScale(width, height, HINT_BAR_HEIGHT);
    for (const index of [0, 1, 2, 40]) {
      const placement = initialPlacement(width, height, scale, index, HINT_BAR_HEIGHT);
      expect(placement.x).toBeGreaterThanOrEqual(0);
      expect(placement.y).toBeGreaterThanOrEqual(0);
      expect(placement.x + DESIGN_W * scale).toBeLessThanOrEqual(width);
      expect(placement.y + DESIGN_H * scale).toBeLessThanOrEqual(height - HINT_BAR_HEIGHT);
    }
  });

  it("anchors new windows when the minimum physical size cannot fit", () => {
    expect(initialPlacement(100, 80, fitScale(100, 80, HINT_BAR_HEIGHT), 3, HINT_BAR_HEIGHT)).toEqual({ x: 0, y: 0 });
  });

  it("windows_open_centred_then_cascade", () => {
    const first = initialPlacement(1280, 800, 1, 0, HINT_BAR_HEIGHT);
    expect(first).toEqual({ x: (1280 - DESIGN_W) / 2, y: (800 - HINT_BAR_HEIGHT - DESIGN_H) / 2 });
    const second = initialPlacement(1280, 800, 1, 1, HINT_BAR_HEIGHT);
    expect(second.x).toBeGreaterThan(first.x);
    expect(second.y).toBeGreaterThan(first.y);
  });
});

describe("resize containment", () => {
  it("contains retained and cascaded positions at the supported minimum", () => {
    const s = fitScale(900, 560, HINT_BAR_HEIGHT);
    for (const x of [-800, 200, 520, 2000]) {
      const p = containPlacement({ x, y: 320 }, 900, 560, s, HINT_BAR_HEIGHT);
      expect(p.x).toBeGreaterThanOrEqual(0);
      expect(p.y).toBeGreaterThanOrEqual(0);
      expect(p.x + DESIGN_W * s).toBeLessThanOrEqual(900);
      expect(p.y + DESIGN_H * s).toBeLessThanOrEqual(528);
    }
  });
  it("anchors impossible fits at the reachable top left", () => {
    expect(containPlacement({ x: 200, y: 200 }, 100, 80, fitScale(100, 80, HINT_BAR_HEIGHT), HINT_BAR_HEIGHT)).toEqual({ x: 0, y: 0 });
  });
});
