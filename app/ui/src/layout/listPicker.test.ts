import { describe, expect, test } from "vitest";
import { placeListPicker, stepListPicker, type ListPickerAction, type ListPickerState } from "./listPickerState";

const options = [
  { id: "master", name: "Master" },
  { id: "cue", name: "Cue", disabled: true },
  { id: "room", name: "Room" },
  { id: "reverb", name: "Reverb" },
];

function step(state: ListPickerState, action: ListPickerAction, value: string | null = null) {
  return stepListPicker(state, action, options, value);
}

describe("list picker", () => {
  test("only opens from closed and starts on the current enabled value or the requested edge", () => {
    expect(step(null, { type: "choose", id: "master" })).toEqual({ state: null, commit: null });
    expect(step(null, { type: "open" }, "room")).toEqual({
      state: { highlight: { id: "room", index: 2 } },
      commit: null,
    });
    expect(step(null, { type: "open", fromEnd: true }).state?.highlight?.id).toBe("reverb");
    expect(step(null, { type: "open" }, "cue").state?.highlight?.id).toBe("master");
  });

  test("arrows wrap and Home, End and type-ahead move the highlight without committing", () => {
    let state = step(null, { type: "open" }).state;
    const moves: [ListPickerAction, string][] = [
      [{ type: "move", direction: 1 }, "room"],
      [{ type: "move", direction: -1 }, "master"],
      [{ type: "move", direction: -1 }, "reverb"],
      [{ type: "move", direction: 1 }, "master"],
      [{ type: "edge", fromEnd: true }, "reverb"],
      [{ type: "edge", fromEnd: false }, "master"],
      [{ type: "search", key: "R" }, "room"],
      [{ type: "search", key: "r" }, "reverb"],
      [{ type: "search", key: "c" }, "reverb"],
    ];

    for (const [action, id] of moves) {
      const result = step(state, action);
      expect(result.state?.highlight?.id).toBe(id);
      expect(result.commit).toBeNull();
      state = result.state;
    }
  });

  test("confirmation emits a new enabled value once and closes", () => {
    const open = step(null, { type: "open" }).state;
    const choice = step(open, { type: "choose" });
    expect(choice).toEqual({ state: null, commit: "master" });
    expect(step(choice.state, { type: "choose" }).commit).toBeNull();
    expect(step(open, { type: "choose" }, "master")).toEqual({ state: null, commit: null });
    expect(step(open, { type: "choose", id: "reverb" })).toEqual({ state: null, commit: "reverb" });
    expect(step(open, { type: "choose", id: "cue" })).toEqual({ state: open, commit: null });
  });

  test.each(["dismiss", "blur"] as const)("%s closes without committing", (type) => {
    const open = step(null, { type: "open" }).state;
    expect(step(open, { type })).toEqual({ state: null, commit: null });
  });

  test("keeps the highlighted id on reorder and uses the nearest enabled option after removal", () => {
    const open = step(null, { type: "open" }, "room").state;
    const reordered = [options[2], options[0], options[1], options[3]];
    const update = { type: "options" } as const;
    const moved = stepListPicker(open, update, reordered, null);
    expect(moved.state?.highlight).toEqual({ id: "room", index: 0 });

    const replaced = [options[0], options[1], { ...options[2], disabled: true }, options[3]];
    expect(stepListPicker(open, update, replaced, null).state?.highlight).toEqual({ id: "reverb", index: 3 });
    expect(stepListPicker(moved.state, update, reordered.slice(1), null).state?.highlight?.id).toBe("master");
    expect(stepListPicker(open, update, [options[1]], null)).toEqual({ state: { highlight: null }, commit: null });
  });

  test("an empty list can open without a selectable highlight", () => {
    const open = stepListPicker(null, { type: "open" }, [], null);
    expect(open).toEqual({ state: { highlight: null }, commit: null });
    expect(stepListPicker(open.state, { type: "move", direction: 1 }, [], null)).toEqual(open);
    expect(stepListPicker(open.state, { type: "choose" }, [], null)).toEqual(open);
  });
});

describe("list picker placement", () => {
  const viewport = { width: 400, height: 300 };
  const anchor = { left: 200, right: 280, top: 60, bottom: 88 };

  test("opens down when it fits and honors end alignment", () => {
    expect(placeListPicker(anchor, { width: 100, height: 90 }, viewport, "end")).toEqual({
      left: 180, top: 94, maxHeight: 198,
    });
  });

  test("flips up near the bottom and clamps the horizontal edge", () => {
    expect(placeListPicker(
      { left: 350, right: 420, top: 244, bottom: 272 }, { width: 110, height: 90 }, viewport,
    )).toEqual({ left: 282, bottom: 62, maxHeight: 230 });
  });

  test("constrains a tall list to the larger side of the trigger", () => {
    expect(placeListPicker(anchor, { width: 100, height: 500 }, viewport)).toEqual({
      left: 200, top: 94, maxHeight: 198,
    });
  });
});
