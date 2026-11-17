// M2 Task 15: undo and redo of note changes (Mod+Z, Mod+Shift+Z). Assertions read the mock
// session's command log and notes, and what the piano roll draws.
import type { Page } from "@playwright/test";
import { clickCell, colourDistance, expect, pixelsAt, test, type Mock } from "./fixture";
import { at, canvas, local, nextFrame, openRoll, sessionNotes, shot, twoChannels } from "./rollHelpers";

const UNDO = "ControlOrMeta+z";
const REDO = "ControlOrMeta+Shift+z";

const cmds = async (mock: Mock) => (await mock.calls()).map((c) => c.cmd);
const count = async (mock: Mock, cmd: string) => (await mock.calls(cmd)).length;
const hold = (mock: Mock, cmd: string) => mock.eval(`(m) => m.holdCommand("${cmd}")`);
const release = (mock: Mock, cmd: string) => mock.eval(`(m) => m.releaseCommand("${cmd}")`);

/** What the roll shows at (tick, key). */
const pixel = async (page: Page, tick: number, key: number) => {
  await nextFrame(page);
  return (await pixelsAt(page, canvas(page), [await local(page, tick, key)]))[0];
};

test.describe("undo and redo", () => {
  // Bass (row 0) and Lead (row 1), two synth channels with no notes yet.
  test.use({ seed: { project: twoChannels([]) } });

  test("undo_and_redo_in_the_roll_and_the_grid", async ({ app, mock }, info) => {
    // The step grid: two steps, each one change.
    await clickCell(app, 0, 0);
    await clickCell(app, 0, 4);
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 0, 240, 60], [1, 960, 240, 60]]);
    await expect.poll(() => mock.view().then((v) => [v.canUndo, v.canRedo])).toEqual([true, false]);
    await app.keyboard.press(UNDO);
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 0, 240, 60]]);
    await expect.poll(() => mock.view().then((v) => [v.canUndo, v.canRedo, v.dirty])).toEqual([true, true, true]);
    await expect.poll(() => mock.view().then((v) => v.pattern.cells["1"][4])).toBeNull();
    await app.keyboard.press(REDO);
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 0, 240, 60], [1, 960, 240, 60]]);

    // An undo pressed while a step edit is still on its way waits for it.
    const gridUndos = await count(mock, "notes_undo");
    await hold(mock, "project_edit");
    await clickCell(app, 0, 8);
    await app.keyboard.press(UNDO);
    await nextFrame(app);
    expect(await count(mock, "notes_undo")).toBe(gridUndos);
    await release(mock, "project_edit");
    await expect.poll(() => count(mock, "notes_undo")).toBe(gridUndos + 1);
    const order = (await cmds(mock)).filter((c) => c === "project_edit" || c === "notes_undo");
    expect(order.slice(-2)).toEqual(["project_edit", "notes_undo"]);
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 0, 240, 60], [1, 960, 240, 60]]);

    // The piano roll: a drawn note goes and comes back, and the roll shows it each time.
    await openRoll(app);
    const empty = await pixel(app, 2_000, 64);
    await app.mouse.click(...xy(await at(app, 1_920, 64)));
    await expect.poll(() => sessionNotes(mock).then((n) => n.length)).toBe(3);
    const drawn = await pixel(app, 2_000, 64);
    expect(colourDistance(drawn, empty)).toBeGreaterThan(40);
    const fetches = await count(mock, "pattern_notes");
    await app.keyboard.press(UNDO);
    await expect.poll(() => sessionNotes(mock).then((n) => n.length)).toBe(2);
    // The roll fetched the notes again and no longer draws the note.
    await expect.poll(() => count(mock, "pattern_notes")).toBeGreaterThan(fetches);
    await expect.poll(async () => colourDistance(await pixel(app, 2_000, 64), empty)).toBeLessThan(8);
    await shot(app, info, "undone");
    await app.keyboard.press(REDO);
    await expect.poll(() => sessionNotes(mock).then((n) => n.length)).toBe(3);
    await expect.poll(async () => colourDistance(await pixel(app, 2_000, 64), drawn)).toBeLessThan(8);

    // An undo pressed while the roll's edit is in flight waits for it too.
    await hold(mock, "notes_edit");
    await app.mouse.click(...xy(await at(app, 2_880, 67)));
    await app.keyboard.press(UNDO);
    await nextFrame(app);
    const undos = await count(mock, "notes_undo");
    expect(undos).toBe(gridUndos + 2);
    await release(mock, "notes_edit");
    await expect.poll(() => count(mock, "notes_undo")).toBe(undos + 1);
    // The new note was made, then undone: the three notes from before stay.
    await expect.poll(() => sessionNotes(mock).then((n) => n.length)).toBe(3);
    const order2 = (await cmds(mock)).filter((c) => c === "notes_edit" || c === "notes_undo");
    expect(order2.slice(-2)).toEqual(["notes_edit", "notes_undo"]);

    // During a drag, undo is ignored.
    const before = await count(mock, "notes_undo");
    const from = await at(app, 1_960, 64);
    await app.mouse.move(from.x, from.y);
    await app.mouse.down();
    await app.mouse.move(from.x + 20, from.y - 10, { steps: 3 });
    await app.keyboard.press(UNDO);
    await app.mouse.up();
    await nextFrame(app);
    expect(await count(mock, "notes_undo")).toBe(before);
  });

  test("a_recorded_take_undoes_in_one_step", async ({ app, mock }) => {
    // Two notes played on the typing keyboard in one take, each committed on its own.
    await app.getByRole("button", { name: "Record" }).click();
    await app.getByRole("button", { name: "Play" }).click();
    for (const [key, at] of [["z", 0], ["x", 960]] as const) {
      await mock.eval(`(m) => m.hold(${at})`);
      await app.keyboard.down(key);
      await mock.eval(`(m) => m.hold(${at + 480})`);
      await app.keyboard.up(key);
    }
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 0, 480, 48], [1, 960, 480, 50]]);
    await app.getByRole("button", { name: "Stop" }).click();
    await app.keyboard.press(UNDO);
    await expect.poll(() => sessionNotes(mock)).toEqual([]);
    await expect.poll(() => mock.view().then((v) => v.canUndo)).toBe(false);
    await app.keyboard.press(REDO);
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 0, 480, 48], [1, 960, 480, 50]]);
    // A second take is a step of its own.
    await app.getByRole("button", { name: "Play" }).click();
    await mock.eval("(m) => m.hold(1920)");
    await app.keyboard.down("c");
    await mock.eval("(m) => m.hold(2400)");
    await app.keyboard.up("c");
    await expect.poll(() => sessionNotes(mock).then((n) => n.length)).toBe(3);
    await app.getByRole("button", { name: "Stop" }).click();
    await app.keyboard.press(UNDO);
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 0, 480, 48], [1, 960, 480, 50]]);
  });

  test("two_takes_without_an_undo_between_are_two_steps", async ({ app, mock }) => {
    await app.getByRole("button", { name: "Record" }).click();
    for (const [key, at] of [["z", 0], ["x", 960]] as const) {
      // Each take: Play, one note, Stop.
      await app.getByRole("button", { name: "Play" }).click();
      await mock.eval(`(m) => m.hold(${at})`);
      await app.keyboard.down(key);
      await mock.eval(`(m) => m.hold(${at + 480})`);
      await app.keyboard.up(key);
      await app.getByRole("button", { name: "Stop" }).click();
    }
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 0, 480, 48], [1, 960, 480, 50]]);
    await app.keyboard.press(UNDO);
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 0, 480, 48]]);
    await app.keyboard.press(UNDO);
    await expect.poll(() => sessionNotes(mock)).toEqual([]);
  });

  test("undo_is_ignored_while_typing_in_the_bpm_field", async ({ app, mock }) => {
    await clickCell(app, 0, 0);
    await expect.poll(() => sessionNotes(mock).then((n) => n.length)).toBe(1);
    const bpm = app.getByRole("textbox", { name: "BPM" });
    await bpm.click();
    await bpm.fill("128");
    await app.keyboard.press(UNDO);
    await app.keyboard.press(REDO);
    await nextFrame(app);
    // The field kept the key for its own text; no note change was undone.
    expect(await count(mock, "notes_undo")).toBe(0);
    expect(await count(mock, "notes_redo")).toBe(0);
    expect(await sessionNotes(mock)).toEqual([[1, 0, 240, 60]]);
    await bpm.press("Escape");
    // Outside the field the same keys undo.
    await app.locator("body").click({ position: { x: 5, y: 5 } });
    await app.keyboard.press(UNDO);
    await expect.poll(() => sessionNotes(mock)).toEqual([]);
  });
});

const xy = (p: { x: number; y: number }): [number, number] => [p.x, p.y];

test.describe("the live fixture's history", () => {
  test.use({ seed: { project: twoChannels([]) } });

  test("keeps_the_shells_note_budget", async ({ app, mock }) => {
    // undo.rs: MAX_UNDO_NOTES = 2,000,000 changed notes (before and after) over every step.
    // Eleven edits that each change all 100,000 notes hold 200,000 apiece: the oldest one goes.
    const steps = await app.evaluate(async () => {
      const ipc = (window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string, args?: object) => Promise<unknown> } })
        .__TAURI_INTERNALS__;
      const m = (window as unknown as { __grooviqMock: { state: { project: { pattern: { notes: object[] } } } } }).__grooviqMock;
      const count = 100_000;
      m.state.project.pattern.notes = Array.from({ length: count }, (_, i) => ({
        id: i + 1, channel: 1, tick: i, length: 10, key: 40, velocity: 0.5, slide: false,
      }));
      for (let edit = 0; edit < 11; edit++) {
        const view = (await ipc.invoke("project_view")) as { notesRevision: number };
        const update = Array.from({ length: count }, (_, i) => ({
          id: i + 1, channel: 1, tick: i, length: 10, key: 41 + edit, velocity: 0.5, slide: false,
        }));
        await ipc.invoke("notes_edit", { edit: { baseRevision: view.notesRevision, add: [], update, delete: [] } });
      }
      let undone = 0;
      while ((await ipc.invoke("notes_undo")) !== null) undone++;
      const first = [...new Set(m.state.project.pattern.notes.map((n) => (n as { key: number }).key))];
      // Five redone, then a new edit: the five undone ones are forgotten, and with them their
      // notes, so six steps fit.
      for (let i = 0; i < 5; i++) await ipc.invoke("notes_redo");
      const view = (await ipc.invoke("project_view")) as { notesRevision: number };
      const update = Array.from({ length: count }, (_, i) => ({
        id: i + 1, channel: 1, tick: i, length: 10, key: 90, velocity: 0.5, slide: false,
      }));
      await ipc.invoke("notes_edit", { edit: { baseRevision: view.notesRevision, add: [], update, delete: [] } });
      let again = 0;
      while ((await ipc.invoke("notes_undo")) !== null) again++;
      return { undone, first, again };
    });
    // Ten steps back is the first edit's notes.
    expect(steps).toEqual({ undone: 10, first: [41], again: 6 });
    expect(await mock.view().then((v) => [v.canUndo, v.canRedo])).toEqual([false, true]);
  });
});

test.describe("the live fixture's pattern length", () => {
  test.use({ seed: { project: twoChannels([]) } });

  test("follows_the_shells_length_log", async ({ app }) => {
    // undo.rs: the length is the log played through: growth of the steps in effect, and every
    // length set by hand, which undo never takes back.
    const lengths = await app.evaluate(async () => {
      const ipc = (window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string, args?: object) => Promise<unknown> } })
        .__TAURI_INTERNALS__;
      const view = async () => (await ipc.invoke("project_view")) as { notesRevision: number; pattern: { steps: number } };
      const add = async (tick: number) => {
        const v = await view();
        const note = { channel: 1, tick, length: 240, key: 60, velocity: 0.8, slide: false };
        await ipc.invoke("notes_edit", { edit: { baseRevision: v.notesRevision, add: [note], update: [], delete: [] } });
        return (await view()).pattern.steps;
      };
      const out: number[] = [];
      out.push(await add(5_000)); // grows 16 -> 32
      out.push(await add(10_000)); // grows 32 -> 48
      await ipc.invoke("project_edit", { edit: { kind: "setPatternLength", steps: 40 } });
      out.push(await add(12_000)); // past 40 steps: grows to 64
      for (let i = 0; i < 3; i++) {
        await ipc.invoke("notes_undo");
        out.push((await view()).pattern.steps);
      }
      for (let i = 0; i < 3; i++) {
        await ipc.invoke("notes_redo");
        out.push((await view()).pattern.steps);
      }
      return out;
    });
    // Undo: 40 (set by hand) stays to the start; redo plays the growth over it again.
    expect(lengths).toEqual([32, 48, 64, 40, 40, 40, 40, 40, 64]);
  });
});
