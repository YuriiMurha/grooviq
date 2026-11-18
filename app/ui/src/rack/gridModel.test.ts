import { describe, expect, it } from "vitest";
import {
  FLAG_BEAT_B,
  FLAG_FOCUS,
  FLAG_PLAYHEAD,
  STRIDE,
  backingScale,
  buildInstances,
  cellAt,
  fillHeight,
  gridSize,
  nextFocus,
  velocityFromDrag,
  type GridLayout,
} from "./gridModel";

const LAYOUT: GridLayout = { rows: 2, steps: 4, cell: 20, gap: 4, dpr: 2 };

/** One record of the instance buffer as an object, for readable assertions. */
function record(data: Float32Array, index: number) {
  const o = index * STRIDE;
  return {
    x: data[o],
    y: data[o + 1],
    w: data[o + 2],
    h: data[o + 3],
    ink: data[o + 4],
    velocity: data[o + 5],
    flags: data[o + 6],
  };
}

describe("cellAt", () => {
  it("cell_at_maps_points_to_cells_and_gaps_to_null", () => {
    expect(cellAt(LAYOUT, 0, 0)).toEqual({ row: 0, step: 0 });
    expect(cellAt(LAYOUT, 19.9, 19.9)).toEqual({ row: 0, step: 0 });
    // The gap after the first column and after the first row.
    expect(cellAt(LAYOUT, 20, 5)).toBeNull();
    expect(cellAt(LAYOUT, 23.9, 5)).toBeNull();
    expect(cellAt(LAYOUT, 5, 21)).toBeNull();
    expect(cellAt(LAYOUT, 24, 0)).toEqual({ row: 0, step: 1 });
    expect(cellAt(LAYOUT, 3 * 24 + 10, 24 + 10)).toEqual({ row: 1, step: 3 });
    // Outside the grid.
    expect(cellAt(LAYOUT, -1, 5)).toBeNull();
    expect(cellAt(LAYOUT, 5, -0.5)).toBeNull();
    expect(cellAt(LAYOUT, 4 * 24, 5)).toBeNull();
    expect(cellAt(LAYOUT, 5, 2 * 24)).toBeNull();
  });

  it("the grid is as big as its cells and the gaps between them", () => {
    expect(gridSize(LAYOUT)).toEqual({ width: 4 * 20 + 3 * 4, height: 2 * 20 + 4 });
    expect(gridSize({ ...LAYOUT, rows: 0 })).toEqual({ width: 92, height: 0 });
  });
});

describe("backingScale", () => {
  it("backing_store_stays_within_canvas_limits", () => {
    // A usual grid keeps the device pixel ratio.
    expect(backingScale(600, 300, 2)).toBe(2);
    expect(backingScale(600, 300, 1)).toBe(1);
    // A very wide one keeps each side within 8192 device pixels.
    expect(backingScale(8704, 300, 2)).toBeCloseTo(8192 / 8704);
    expect(Math.round(8704 * backingScale(8704, 300, 2))).toBeLessThanOrEqual(8192);
    // The largest grid also keeps the area within 16.7 M pixels (4096 x 4096).
    const s = backingScale(8712, 8712, 2);
    expect(8712 * s * (8712 * s)).toBeLessThanOrEqual(4096 * 4096 + 1);
    // An empty grid has no limit to hit.
    expect(backingScale(0, 0, 2)).toBe(2);
  });
});

describe("buildInstances", () => {
  it("instances_hold_one_record_per_cell_with_velocity_fill", () => {
    const cells = [
      [1, null, 0.5, null],
      [null, 0.25], // shorter than the pattern: the rest is empty
    ];
    const data = buildInstances(LAYOUT, cells, [0, 4], null, null);
    expect(data.length).toBe(2 * 4 * STRIDE);

    expect(record(data, 0)).toEqual({ x: 0, y: 0, w: 20, h: 20, ink: 0, velocity: 1, flags: 0 });
    expect(record(data, 1).velocity).toBe(-1);
    expect(record(data, 2)).toMatchObject({ x: 48, y: 0, velocity: 0.5 });
    // Second row: its ink, its y.
    expect(record(data, 5)).toMatchObject({ x: 24, y: 24, ink: 4, velocity: 0.25 });
    expect(record(data, 7).velocity).toBe(-1);

    // Lit cells fill from the bottom by velocity.
    expect(fillHeight(20, 1)).toBe(20);
    expect(fillHeight(20, 0.5)).toBe(10);
    expect(fillHeight(20, 0)).toBe(0);
    expect(fillHeight(20, 2)).toBe(20);
  });

  it("the_playhead_and_focus_flags_mark_their_cells", () => {
    const cells = [
      [null, null, null, null],
      [null, null, null, null],
    ];
    const layout = { ...LAYOUT, steps: 8 };
    const data = buildInstances(layout, cells, [0, 1], { row: 1, step: 2 }, 2);
    const flags = (row: number, step: number) => record(data, row * 8 + step).flags;
    expect(flags(1, 2) & FLAG_FOCUS).toBe(FLAG_FOCUS);
    expect(flags(0, 2) & FLAG_FOCUS).toBe(0);
    expect(flags(0, 2) & FLAG_PLAYHEAD).toBe(FLAG_PLAYHEAD);
    expect(flags(1, 2) & FLAG_PLAYHEAD).toBe(FLAG_PLAYHEAD);
    expect(flags(0, 3) & FLAG_PLAYHEAD).toBe(0);
    // Beats 2 and 4 of a bar (steps 4..7) take the other off-step colour.
    expect(flags(0, 3) & FLAG_BEAT_B).toBe(0);
    expect(flags(0, 4) & FLAG_BEAT_B).toBe(FLAG_BEAT_B);
    expect(flags(1, 7) & FLAG_BEAT_B).toBe(FLAG_BEAT_B);
    // Without either, nothing is marked.
    const plain = buildInstances(layout, cells, [0, 1], null, null);
    for (let i = 0; i < 16; i++) expect(record(plain, i).flags & (FLAG_FOCUS | FLAG_PLAYHEAD)).toBe(0);
  });

  it("the_largest_grid_reads_each_cell_once", () => {
    const layout: GridLayout = { rows: 256, steps: 256, cell: 28, gap: 6, dpr: 2 };
    const cells = Array.from({ length: 256 }, (_, r) =>
      Array.from({ length: 256 }, (_, s) => ((r + s) % 3 === 0 ? 0.8 : null)),
    );
    const inks = Array.from({ length: 256 }, (_, r) => r % 6);
    let reads = 0;
    const observed = cells.map((row) => new Proxy(row, {
      get(target, key, receiver) {
        if (typeof key === "string" && /^\d+$/.test(key)) reads++;
        return Reflect.get(target, key, receiver);
      },
    }));
    const data = buildInstances(layout, observed, inks, { row: 3, step: 3 }, 7);
    expect(data.length).toBe(256 * 256 * STRIDE);
    expect(reads).toBe(256 * 256);
    for (let r = 0; r < 256; r++) {
      for (let step = 0; step < 256; step++) {
        expect(record(data, r * 256 + step)).toEqual({
          x: step * 34, y: r * 34, w: 28, h: 28, ink: r % 6,
          velocity: (r + step) % 3 === 0 ? Math.fround(0.8) : -1,
          flags: (Math.floor(step / 4) % 2 === 1 ? FLAG_BEAT_B : 0)
            | (r === 3 && step === 3 ? FLAG_FOCUS : 0)
            | (step === 7 ? FLAG_PLAYHEAD : 0),
        });
      }
    }
  });
});

describe("velocityFromDrag", () => {
  it("velocity_follows_vertical_drag_and_clamps", () => {
    // Up (a negative dy on screen) raises it; 100 px is the full range.
    expect(velocityFromDrag(0.5, -25)).toBeCloseTo(0.75);
    expect(velocityFromDrag(0.5, 25)).toBeCloseTo(0.25);
    expect(velocityFromDrag(0.5, 0)).toBe(0.5);
    expect(velocityFromDrag(0, -100)).toBe(1);
    expect(velocityFromDrag(0.8, -500)).toBe(1);
    expect(velocityFromDrag(0.2, 500)).toBe(0);
  });
});

describe("nextFocus", () => {
  it("arrow_keys_move_focus_and_wrap_within_a_row", () => {
    expect(nextFocus({ row: 0, step: 0 }, "ArrowRight", 3, 16)).toEqual({ row: 0, step: 1 });
    expect(nextFocus({ row: 0, step: 15 }, "ArrowRight", 3, 16)).toEqual({ row: 0, step: 0 });
    expect(nextFocus({ row: 1, step: 0 }, "ArrowLeft", 3, 16)).toEqual({ row: 1, step: 15 });
    expect(nextFocus({ row: 1, step: 4 }, "ArrowDown", 3, 16)).toEqual({ row: 2, step: 4 });
    // Clamps across rows.
    expect(nextFocus({ row: 2, step: 4 }, "ArrowDown", 3, 16)).toEqual({ row: 2, step: 4 });
    expect(nextFocus({ row: 0, step: 4 }, "ArrowUp", 3, 16)).toEqual({ row: 0, step: 4 });
    expect(nextFocus({ row: 1, step: 4 }, "ArrowUp", 3, 16)).toEqual({ row: 0, step: 4 });
    // Home and End go to the ends of the row; other keys leave it.
    expect(nextFocus({ row: 1, step: 4 }, "Home", 3, 16)).toEqual({ row: 1, step: 0 });
    expect(nextFocus({ row: 1, step: 4 }, "End", 3, 16)).toEqual({ row: 1, step: 15 });
    expect(nextFocus({ row: 1, step: 4 }, "a", 3, 16)).toEqual({ row: 1, step: 4 });
    // A focus left outside a grid that shrank comes back inside.
    expect(nextFocus({ row: 5, step: 40 }, "ArrowRight", 3, 16)).toEqual({ row: 2, step: 0 });
  });
});

describe("roll rows", () => {
  it("roll_rows_replace_their_steps_with_the_preview", async () => {
    const { rollInstances } = await import("./rollPreview");
    const layout: GridLayout = { rows: 3, steps: 4, cell: 28, gap: 6, dpr: 1 };
    const note = { id: 1, channel: 7, tick: 240, length: 480, velocity: 1, key: 62, slide: false };
    const preview = rollInstances(layout, 1, [note], 7, 2, false);
    const out = buildInstances(layout, [[0.5], [1, 1, 1, 1], []], [0, 2, 3], null, null, [null, preview]);
    // Rows 0 and 2 keep a record per step; row 1 is its well and one bar, and none of its steps.
    expect(out.length).toBe((4 + 2 + 4) * STRIDE);
    expect(Array.from(out.subarray(4 * STRIDE, 6 * STRIDE))).toEqual(Array.from(preview));
    expect(out[6 * STRIDE + 1]).toBe(2 * 34);
  });
});
