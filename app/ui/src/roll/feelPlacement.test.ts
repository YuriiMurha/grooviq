import { describe, expect, it } from "vitest";
import { placeFeelPanel, type FeelRect } from "./feelPlacement";

const usable: FeelRect = { left: 8, top: 8, right: 892, bottom: 520 };
describe("Feel panel placement", () => {
  it("keeps a six-pixel gap below when the whole panel fits", () => {
    expect(placeFeelPanel({ left: 100, top: 50, right: 150, bottom: 70 }, { width: 330, height: 92 }, usable))
      .toEqual({ left: 100, top: 76, maxWidth: 884, maxHeight: 444, side: "below" });
  });
  it("flips above the compact toolbar and retains full readout height", () => {
    expect(placeFeelPanel({ left: 100, top: 436, right: 150, bottom: 455 }, { width: 330, height: 92 }, usable))
      .toEqual({ left: 100, top: 338, maxWidth: 884, maxHeight: 422, side: "above" });
  });
  it("clamps fractional horizontal edges and bounds oversized content locally", () => {
    const p = placeFeelPanel({ left: 880.5, top: 436, right: 900, bottom: 455 }, { width: 1000, height: 600 }, usable);
    expect(p).toEqual({ left: 8, top: 8, maxWidth: 884, maxHeight: 422, side: "above" });
  });
  it("chooses below on a tie and does not return negative space", () => {
    expect(placeFeelPanel({ left: -10, top: 254, right: 10, bottom: 274 }, { width: 30, height: 1000 }, usable))
      .toEqual({ left: 8, top: 280, maxWidth: 884, maxHeight: 240, side: "below" });
    const p = placeFeelPanel({ left: 10, top: 0, right: 30, bottom: 50 }, { width: 300, height: 100 }, { left: 8, top: 8, right: 20, bottom: 20 });
    expect(p.left).toBe(8);
    expect(p.top).toBeGreaterThanOrEqual(8);
    expect(p.top + p.maxHeight).toBeLessThanOrEqual(20);
  });
});
