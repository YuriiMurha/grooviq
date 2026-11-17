import { describe, expect, it } from "vitest";
import type { WireNote } from "../api/notes";
import { NoteStore } from "./noteStore";
import { DRAW_VELOCITY, endGesture, moveGesture, startGesture, type Pointer, type Step, type Tool, type ToolContext } from "./tools";
import { toScreen, type RollView } from "./view";

// 0.125 px a tick: a step (240 ticks) is 30 px wide; a key row is 14 px tall.
const VIEW: RollView = { tickPx: 0.125, keyPx: 14, scrollTick: 0, topKey: 100, width: 1200, height: 600 };

const note = (id: number, tick: number, length: number, key: number, channel = 1): WireNote => ({
  id,
  channel,
  tick,
  length,
  key,
  velocity: 0.7,
  slide: false,
});

/** The pointer over (tick, key): `key` is the row, the pointer sits in its middle. */
function at(tick: number, key: number, button = 0, shift = false): Pointer {
  const { x, y } = toScreen(VIEW, tick, key + 0.5);
  return { x, y, tick, key: key + 0.5, button, shift };
}

/** A context and a store, applying each step's edit and selection the way the piano roll does. */
function setup(notes: WireNote[], tool: Tool, grid = 240, lastLength = 240) {
  const store = new NoteStore();
  store.load(1, notes);
  let temp = 0;
  const ctx: ToolContext = { store, channel: 1, tool, grid, lastLength, view: VIEW, newId: () => --temp };
  const edits: Step["edit"][] = [];
  const take = (step: Step) => {
    if (step.edit) {
      store.apply(step.edit);
      edits.push(step.edit);
    }
    if (step.select) store.setSelection(step.select);
    if (step.lastLength !== undefined) ctx.lastLength = step.lastLength;
    return step;
  };
  /** A whole gesture: down at the first pointer, moves through the rest, up at the last. */
  const gesture = (...points: Pointer[]) => {
    let s = take(startGesture(ctx, points[0]));
    for (const p of points.slice(1)) s = take(moveGesture(ctx, s.gesture, p));
    return take(endGesture(ctx, s.gesture, points[points.length - 1]));
  };
  return { store, ctx, edits, take, gesture };
}

describe("piano roll tools", () => {
  it("draw_adds_a_note_with_the_last_length", () => {
    const { store, ctx, edits, gesture } = setup([note(1, 0, 960, 60)], "draw", 240, 480);
    // A click on an empty cell: a note there, snapped down to the grid, the last length long.
    gesture(at(1000, 64));
    expect(edits).toEqual([{ add: [{ id: -1, channel: 1, tick: 960, length: 480, key: 64, velocity: DRAW_VELOCITY, slide: false }], update: [], delete: [] }]);
    expect(store.selectedIds()).toEqual([-1]);

    // Grabbing a note makes its length the last length (and a click on it changes nothing).
    gesture(at(100, 60));
    expect(ctx.lastLength).toBe(960);
    expect(edits).toHaveLength(1);
    gesture(at(2000, 50));
    expect(edits[1]?.add[0]).toMatchObject({ tick: 1920, length: 960, key: 50 });

    // On a finer grid the start snaps to it.
    ctx.grid = 60;
    gesture(at(2990, 70));
    expect(edits[2]?.add[0]).toMatchObject({ tick: 2940, key: 70 });
    // A note drawn and dragged before release lands where it was let go: added, then moved.
    gesture(at(4800, 72), at(5000, 72), at(5300, 73));
    expect(edits[3]?.add[0]).toMatchObject({ tick: 4800, key: 72 });
    expect(edits[4]?.update).toEqual([expect.objectContaining({ id: edits[3]?.add[0].id, tick: 5280, key: 73 })]);
  });

  it("drag_moves_the_selection_by_a_snapped_delta", () => {
    const notes = [note(1, 250, 240, 60), note(2, 1000, 120, 64), note(3, 3000, 240, 50)];
    const { store, edits, gesture } = setup(notes, "select");
    store.setSelection([1, 2]);
    // Grab note 1 (start 250) and drag 700 ticks right and two rows up: its start snaps to 960
    // (250 + 700 = 950, rounded to the step grid), so the delta is 710 for every selected note.
    const s = gesture(at(300, 60), at(600, 61), at(1000, 62));
    expect(s.gesture.kind).toBe("move");
    expect(edits).toEqual([
      {
        add: [],
        update: [
          { ...note(1, 960, 240, 62) },
          { ...note(2, 1710, 120, 66) },
        ],
        delete: [],
      },
    ]);
    // Note 3 was not selected and stays.
    expect(store.get(3)).toEqual(note(3, 3000, 240, 50));

    // While dragging, the delta is what the renderer offsets the selection by.
    const g = setup(notes, "select");
    g.store.setSelection([1, 2]);
    const down = g.take(startGesture(g.ctx, at(300, 60)));
    const mid = moveGesture(g.ctx, down.gesture, at(1000, 62));
    expect(mid.edit).toBeNull();
    expect(mid.gesture).toMatchObject({ kind: "move", dTick: 710, dKey: 2 });

    // The selection never moves before tick 0 or outside 0..=127.
    const edge = setup([note(1, 240, 240, 126), note(2, 600, 240, 3)], "select");
    edge.store.setSelection([1, 2]);
    edge.gesture(at(300, 126), at(-5000, 140));
    expect(edge.edits[0]?.update).toEqual([note(1, 0, 240, 127), note(2, 360, 240, 4)]);

    // Dragged down past key 0, the group stops where its lowest note reaches 0, keeping its shape.
    const low = setup([note(1, 0, 240, 3), note(2, 480, 240, 10)], "select");
    low.store.setSelection([1, 2]);
    const held = low.take(startGesture(low.ctx, at(100, 3)));
    expect(moveGesture(low.ctx, held.gesture, at(100, -7)).gesture).toMatchObject({ kind: "move", dTick: 0, dKey: -3 });
    low.take(endGesture(low.ctx, held.gesture, at(100, -7)));
    expect(low.edits[0]?.update).toEqual([note(1, 0, 240, 0), note(2, 480, 240, 7)]);

    // Grabbing an unselected note selects only it; shift adds it.
    const one = setup(notes, "select");
    one.store.setSelection([1]);
    one.gesture(at(3010, 50), at(3500, 50));
    expect(one.store.selectedIds()).toEqual([3]);
    // 3000 + 490 rounds to 3600 on the step grid.
    expect(one.edits[0]?.update).toEqual([note(3, 3600, 240, 50)]);
    one.gesture(at(1010, 64, 0, true));
    expect(one.store.selectedIds().sort()).toEqual([2, 3]);
    // A click without a drag moves nothing, even for a note off the grid.
    expect(one.edits).toHaveLength(1);
  });

  it("resize_from_the_edge_snaps_the_end_and_keeps_a_minimum", () => {
    // Note 1 spans 0..720: its right edge is at x = 90; the zone is the last 6 px.
    const notes = [note(1, 0, 720, 60), note(2, 960, 500, 62)];
    const { store, ctx, edits, gesture } = setup(notes, "draw");
    store.setSelection([1, 2]);
    // Drag the edge 320 ticks right: the end (720 + 320 = 1040) snaps to 960, +240 for both.
    const s = gesture(at(700, 60), at(1000, 60), at(1020, 60));
    expect(s.gesture.kind).toBe("resize");
    expect(edits[0]).toEqual({ add: [], update: [note(1, 0, 960, 60), note(2, 960, 740, 62)], delete: [] });
    // The new length is the last length.
    expect(ctx.lastLength).toBe(960);

    // Dragging far left stops each note at one grid cell.
    gesture(at(950, 60), at(-4000, 60));
    expect(edits[1]?.update).toEqual([note(1, 0, 240, 60), note(2, 960, 240, 62)]);
    // On Line the minimum is one tick.
    ctx.grid = 1;
    gesture(at(235, 60), at(-4000, 60));
    expect(edits[2]?.update).toEqual([note(1, 0, 1, 60), note(2, 960, 1, 62)]);

    // 7 px from the edge moves instead.
    const m = setup([note(1, 0, 720, 60)], "draw");
    const g = m.gesture(at(720 - 7 * 8, 60), at(1500, 60));
    expect(g.gesture.kind).toBe("move");
  });

  it("paint_adds_one_note_per_cell", () => {
    const { edits, store, gesture } = setup([note(9, 960, 240, 60)], "paint");
    // Down in step 1, across to step 6 along row 60 (the pointer wanders a row up), then back.
    gesture(at(250, 60), at(400, 60), at(800, 61), at(1500, 60), at(1300, 60));
    const added = edits.flatMap((e) => e?.add ?? []);
    // One note in each cell it crossed, a cell long, in the row it started on; step 4 already
    // held note 9, and returning to a painted cell adds nothing.
    expect(added.map((n) => [n.tick, n.length, n.key])).toEqual([
      [240, 240, 60],
      [480, 240, 60],
      [720, 240, 60],
      [1200, 240, 60],
      [1440, 240, 60],
    ]);
    expect(store.count).toBe(6);
  });

  it("slice_splits_at_the_snapped_tick", () => {
    const { edits, store, ctx, gesture } = setup([{ ...note(1, 0, 960, 60), slide: true }], "slice");
    // 470 rounds to 480 on the step grid.
    gesture(at(470, 60));
    expect(edits[0]).toEqual({
      add: [{ id: -1, channel: 1, tick: 480, length: 480, key: 60, velocity: 0.7, slide: false }],
      update: [{ ...note(1, 0, 480, 60), slide: true }],
      delete: [],
    });
    expect(store.count).toBe(2);
    // A cut that would land on the note's start or end does nothing.
    gesture(at(10, 60));
    gesture(at(950, 60));
    expect(edits).toHaveLength(1);
    // On Line the cut is exact.
    ctx.grid = 1;
    gesture(at(601, 60));
    expect(edits[1]?.update).toEqual([note(-1, 480, 121, 60)]);
    expect(edits[1]?.add[0]).toMatchObject({ tick: 601, length: 359 });
  });

  it("right_click_deletes_in_every_tool", () => {
    for (const tool of ["draw", "paint", "select", "slice", "delete"] as Tool[]) {
      const { edits, store, gesture } = setup([note(1, 0, 240, 60), note(2, 480, 240, 60), note(3, 960, 240, 61)], tool);
      gesture(at(100, 60, 2));
      expect(edits, tool).toEqual([{ add: [], update: [], delete: [1] }]);
      // Held and dragged, it deletes what it passes over.
      gesture(at(300, 60, 2), at(500, 60, 2), at(1000, 61, 2));
      expect(edits.slice(1), tool).toEqual([
        { add: [], update: [], delete: [2] },
        { add: [], update: [], delete: [3] },
      ]);
      expect(store.count, tool).toBe(0);
      // On empty space it adds nothing.
      gesture(at(5000, 70, 2));
      expect(edits, tool).toHaveLength(3);
    }
    // The delete tool deletes with the left button too.
    const d = setup([note(1, 0, 240, 60)], "delete");
    d.gesture(at(10, 60));
    expect(d.edits).toEqual([{ add: [], update: [], delete: [1] }]);
  });

  it("scale_snap_puts_drawn_painted_and_moved_notes_in_key", () => {
    // C major: key 61 (C#) is a tie between C and D, and snaps down to 60.
    const draw = setup([], "draw");
    draw.ctx.scale = { root: 0, scale: "major" };
    draw.gesture(at(10, 61));
    expect(draw.edits[0]?.add[0]).toMatchObject({ key: 60 });
    // Without the snap it lands where it was clicked.
    draw.ctx.scale = null;
    draw.gesture(at(500, 61));
    expect(draw.edits[1]?.add[0]).toMatchObject({ key: 61 });

    // Paint puts the whole stroke in the snapped row.
    const paint = setup([], "paint");
    paint.ctx.scale = { root: 0, scale: "major" };
    paint.gesture(at(10, 66), at(500, 66));
    expect(paint.edits.flatMap((e) => e?.add ?? []).map((n) => n.key)).toEqual([65, 65, 65]);

    // A drag moves the grabbed note to a key in the scale; the others keep their distance to it.
    const move = setup([note(1, 0, 240, 60), note(2, 480, 240, 67)], "select");
    move.ctx.scale = { root: 0, scale: "major" };
    move.store.setSelection([1, 2]);
    // One row up from C is C#, which snaps back down to C: nothing moves.
    move.gesture(at(10, 60), at(10, 61));
    expect(move.edits).toHaveLength(0);
    // Two rows up is D: both move two rows.
    move.gesture(at(10, 60), at(10, 62));
    expect(move.edits[0]?.update.map((n) => n.key)).toEqual([62, 69]);
  });

  it("slide_notes_are_drawn_when_the_slide_toggle_is_on", () => {
    const draw = setup([], "draw");
    draw.ctx.slide = true;
    draw.gesture(at(10, 60));
    expect(draw.edits[0]?.add[0]).toMatchObject({ slide: true });
    const paint = setup([], "paint");
    paint.ctx.slide = true;
    paint.gesture(at(10, 60), at(300, 60));
    expect(paint.edits.flatMap((e) => e?.add ?? []).every((n) => n.slide)).toBe(true);
    // Off by default.
    const plain = setup([], "draw");
    plain.gesture(at(10, 60));
    expect(plain.edits[0]?.add[0]).toMatchObject({ slide: false });
  });

  it("a_chord_stamp_adds_every_voice_selected_and_dragged_together", () => {
    const { store, ctx, edits, gesture } = setup([], "draw");
    ctx.chord = { name: "major", inversion: 0 };
    gesture(at(1000, 60));
    const added = edits[0]!.add;
    expect(added.map((n) => [n.tick, n.key, n.length, n.id])).toEqual([
      [960, 60, 240, -1],
      [960, 64, 240, -2],
      [960, 67, 240, -3],
    ]);
    expect(store.selectedIds()).toEqual([-1, -2, -3]);
    // In key: D major triad on D in C major, F# snapped to F.
    ctx.scale = { root: 0, scale: "major" };
    gesture(at(2000, 62));
    expect(edits[1]!.add.map((n) => n.key)).toEqual([62, 65, 69]);
    // Dragged before release the whole chord moves, and the edit that follows is the move.
    ctx.scale = null;
    gesture(at(3000, 60), at(3000, 62), at(3500, 63));
    expect(edits[3]!.update.map((n) => [n.tick, n.key])).toEqual([[3360, 63], [3360, 67], [3360, 70]]);
  });
});
