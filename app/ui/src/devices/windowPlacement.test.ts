import { describe, expect, it } from "vitest";
import { DESIGN_H, DESIGN_W, clampPlacement, containPlacement, fitScale, initialPlacement } from "./windowPlacement";

describe("device window placement", () => {
  it("scales_down_to_fit_but_never_up", () => {
    expect(fitScale(1920, 1080)).toBe(1);
    expect(fitScale(DESIGN_W + 32, DESIGN_H + 32)).toBe(1);
    const s = fitScale(600, 1000);
    expect(DESIGN_W * s).toBeLessThanOrEqual(600 - 32);
    expect(DESIGN_W * s).toBeGreaterThan(600 - 33);
  });

  it("a_dragged_window_stays_reachable", () => {
    const p = clampPlacement({ x: -5000, y: -40 }, 1280, 800, 1);
    expect(p.x + DESIGN_W).toBeGreaterThanOrEqual(80);
    expect(p.y).toBe(0);
    const q = clampPlacement({ x: 5000, y: 5000 }, 1280, 800, 1);
    expect(q.x).toBeLessThanOrEqual(1280 - 80);
    expect(q.y).toBeLessThanOrEqual(800 - 80);
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
