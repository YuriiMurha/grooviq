import { describe, expect, it } from "vitest";
import { GRIDS, DEFAULT_GRID, barTicks, gridTicks, snapTick } from "./snap";

describe("snap", () => {
  it("every_grid_snaps_floor_and_round", () => {
    const sig = { beats: 4, unit: 4 };
    // The Global Constraints table, by name.
    expect(GRIDS.map((g) => [g.name, gridTicks(g, sig)])).toEqual([
      ["Line", 1],
      ["1/6 step", 40],
      ["1/4 step", 60],
      ["1/3 step", 80],
      ["1/2 step", 120],
      ["Step", 240],
      ["1/3 beat", 320],
      ["1/2 beat", 480],
      ["Beat", 960],
      ["Bar", 3840],
    ]);
    expect(DEFAULT_GRID.name).toBe("Step");

    for (const g of GRIDS) {
      const t = gridTicks(g, sig);
      // A tick just past a line floors to it and rounds to it; one past the middle rounds up.
      const at = 7 * t;
      expect(snapTick(at, t, "floor")).toBe(at);
      expect(snapTick(at, t, "round")).toBe(at);
      if (t > 1) {
        expect(snapTick(at + 1, t, "floor")).toBe(at);
        expect(snapTick(at + 1, t, "round")).toBe(at);
        expect(snapTick(at + t - 1, t, "floor")).toBe(at);
        expect(snapTick(at + t - 1, t, "round")).toBe(at + t);
        // Half way rounds up.
        expect(snapTick(at + Math.ceil(t / 2), t, "round")).toBe(at + t);
        expect(snapTick(at + Math.floor((t - 1) / 2), t, "round")).toBe(at);
      }
      // Fractions of a tick (a pointer between pixels) snap too.
      expect(snapTick(at + 0.4, t, "floor")).toBe(at);
    }
    // Line keeps whole ticks.
    expect(snapTick(12.6, 1, "round")).toBe(13);
    expect(snapTick(12.6, 1, "floor")).toBe(12);
  });

  it("bar_grid_follows_the_time_signature", () => {
    const bar = GRIDS.find((g) => g.name === "Bar")!;
    expect(barTicks({ beats: 4, unit: 4 })).toBe(3840);
    expect(gridTicks(bar, { beats: 3, unit: 4 })).toBe(2880);
    expect(gridTicks(bar, { beats: 7, unit: 8 })).toBe(3360);
    expect(gridTicks(bar, { beats: 6, unit: 8 })).toBe(2880);
    expect(gridTicks(bar, { beats: 5, unit: 4 })).toBe(4800);
    expect(snapTick(3000, gridTicks(bar, { beats: 3, unit: 4 }), "floor")).toBe(2880);
    // Every other grid ignores the signature.
    const step = GRIDS.find((g) => g.name === "Step")!;
    expect(gridTicks(step, { beats: 7, unit: 8 })).toBe(240);
  });
});
