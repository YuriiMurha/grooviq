import { describe, expect, it } from "vitest";
import type { WireNote } from "../api/notes";
import { NoteStore } from "./noteStore";
import { laneEdit, laneSet, velocityAt } from "./velocity";

const note = (id: number, tick: number, o: Partial<WireNote> = {}): WireNote => ({
  id,
  channel: 1,
  tick,
  length: 240,
  velocity: 0.8,
  key: 60,
  slide: false,
  ...o,
});

describe("velocity lane", () => {
  it("lane_drag_sets_selected_velocities_and_clamps", () => {
    const store = new NoteStore();
    store.load(1, [note(1, 0), note(2, 240), note(3, 480), note(4, 720, { channel: 2 })]);
    store.setSelection([1, 3]);
    // The top of the lane is full velocity and the bottom is silent; past either end clamps.
    expect([velocityAt(0, 100), velocityAt(50, 100), velocityAt(100, 100), velocityAt(-30, 100), velocityAt(160, 100)]).toEqual([1, 0.5, 0, 1, 0]);

    // A drag with a selection sets every selected note of the channel, wherever the pointer is.
    const acc = new Map<number, number>();
    laneSet(store, 1, 5000, 0.25, 10, acc);
    expect([...acc]).toEqual([[1, 0.25], [3, 0.25]]);
    laneSet(store, 1, 0, 1.7, 10, acc);
    expect([...acc]).toEqual([[1, 1], [3, 1]]);
    laneSet(store, 1, 0, -0.4, 10, acc);
    expect([...acc]).toEqual([[1, 0], [3, 0]]);

    // One edit with the final values; notes outside the selection and channel are left alone.
    const edit = laneEdit(store, new Map([[1, 0.5], [3, 0.8]]))!;
    // Note 3 is already 0.8: not sent.
    expect(edit.update).toEqual([note(1, 0, { velocity: 0.5 })]);
    expect(edit.add).toEqual([]);
    expect(laneEdit(store, new Map([[3, 0.8]]))).toBeNull();
  });

  it("with_no_selection_the_drag_paints_the_bars_it_passes_over", () => {
    const store = new NoteStore();
    store.load(1, [note(1, 0), note(2, 0, { key: 64 }), note(3, 240), note(4, 960)]);
    const acc = new Map<number, number>();
    // At tick 230, with 20 ticks of reach: the notes starting at 240 (and none at 0 or 960).
    laneSet(store, 1, 230, 0.4, 20, acc);
    expect([...acc]).toEqual([[3, 0.4]]);
    // Both notes of a chord share a bar.
    laneSet(store, 1, 5, 0.9, 20, acc);
    expect([...acc].sort((a, b) => a[0] - b[0])).toEqual([[1, 0.9], [2, 0.9], [3, 0.4]]);
    // Other channels' notes are never touched.
    store.load(2, [note(1, 0), note(9, 0, { channel: 2 })]);
    const other = new Map<number, number>();
    laneSet(store, 1, 0, 0.1, 5, other);
    expect([...other]).toEqual([[1, 0.1]]);
  });
});
