// Issue #186: a note edit made in one project never lands in the project that replaced it, from
// the piano roll's queue or straight from the shell's commands.
import type { Page } from "@playwright/test";
import { clickCell, expect, test } from "./fixture";
import { at, nextFrame, openRoll, sessionNotes, twoChannels } from "./rollHelpers";

const B = "/qa/B.grooviq";

/** Holds back the reply to the next `notes_edit` (the engine has applied it) until the page
 * calls `releaseAck()`. */
const delayFirstAck = (app: Page) =>
  app.evaluate(() => {
    const w = window as unknown as {
      __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown) => Promise<unknown> };
      releaseAck?: () => void;
    };
    const original = w.__TAURI_INTERNALS__.invoke;
    let first = true;
    w.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      const result = await original(cmd, args);
      if (cmd === "notes_edit" && first) {
        first = false;
        await new Promise<void>((r) => (w.releaseAck = r));
      }
      return result;
    };
  });

test.describe("an edit made before Open", () => {
  // A: Bass holds one note. B (same channel ids) holds none.
  test.use({
    seed: {
      project: twoChannels([{ id: 1, channel: 1, tick: 0, length: 240, key: 64, velocity: 0.8 }]),
      files: { [B]: { kind: "project", project: twoChannels([]) } },
    },
  });

  test("queued_roll_edits_cannot_cross_open", async ({ app, mock }) => {
    await openRoll(app);
    // The first edit lands in A, but its reply is held back: the second waits in the roll's queue.
    await delayFirstAck(app);
    for (const tick of [500, 1000]) {
      const p = await at(app, tick, 65);
      await app.mouse.click(p.x, p.y);
    }
    await expect.poll(async () => (await mock.calls("notes_edit")).length).toBe(1);

    // Open B. The UI has not heard that A changed, yet it asks: an edit is on its way.
    await mock.eval(`(m) => m.queueDialog("${B}")`);
    await app.keyboard.press("ControlOrMeta+o");
    const dialog = app.getByRole("dialog", { name: "Save changes?" });
    await expect(dialog).toBeVisible();
    await dialog.getByRole("button", { name: "Don't save" }).click();
    await expect.poll(() => mock.view().then((v) => v.path)).toBe(B);

    // A's late reply arrives: nothing of A's reaches B, and A's view is not drawn.
    await app.evaluate(() => (window as unknown as { releaseAck: () => void }).releaseAck());
    await nextFrame(app);
    await nextFrame(app);
    expect(await sessionNotes(mock)).toEqual([]);
    expect((await mock.calls("notes_edit")).length).toBe(1);
    expect(await mock.view().then((v) => v.dirty)).toBe(false);
    await expect(app.getByText("Unsaved changes")).toBeHidden();

    // B's own edits go through.
    const p = await at(app, 1500, 67);
    await app.mouse.click(p.x, p.y);
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 1440, 240, 67]]);
  });

  test("nothing_of_a_goes_out_while_open_is_on_its_way", async ({ app, mock }) => {
    await openRoll(app);
    await delayFirstAck(app);
    for (const tick of [500, 1000]) {
      const p = await at(app, tick, 65);
      await app.mouse.click(p.x, p.y);
    }
    await expect.poll(async () => (await mock.calls("notes_edit")).length).toBe(1);
    // The shell holds the Open; A's reply comes while it waits.
    await mock.eval(`(m) => m.holdCommand("project_open")`);
    await mock.eval(`(m) => m.queueDialog("${B}")`);
    await app.keyboard.press("ControlOrMeta+o");
    await app.getByRole("dialog", { name: "Save changes?" }).getByRole("button", { name: "Don't save" }).click();
    await expect.poll(async () => (await mock.calls("project_open")).length).toBe(1);
    await app.evaluate(() => (window as unknown as { releaseAck: () => void }).releaseAck());
    await nextFrame(app);
    await nextFrame(app);
    expect((await mock.calls("notes_edit")).length).toBe(1);
    await mock.eval(`(m) => m.releaseCommand("project_open")`);
    await expect.poll(() => mock.view().then((v) => v.path)).toBe(B);
    await nextFrame(app);
    expect((await mock.calls("notes_edit")).length).toBe(1);
    expect(await sessionNotes(mock)).toEqual([]);
  });

  test("a_closed_roll_still_holds_open_back_and_its_late_reply_is_not_drawn", async ({ app, mock }) => {
    await openRoll(app);
    await delayFirstAck(app);
    const p = await at(app, 500, 65);
    await app.mouse.click(p.x, p.y);
    await expect.poll(async () => (await mock.calls("notes_edit")).length).toBe(1);
    // The roll closes while its edit waits for its reply; the UI still shows A as saved.
    await app.keyboard.press("F7");
    await expect(app.getByLabel("Piano roll notes")).toBeHidden();
    await mock.eval(`(m) => m.queueDialog("${B}")`);
    await app.keyboard.press("ControlOrMeta+o");
    const dialog = app.getByRole("dialog", { name: "Save changes?" });
    await expect(dialog).toBeVisible();
    await dialog.getByRole("button", { name: "Don't save" }).click();
    await expect.poll(() => mock.view().then((v) => v.path)).toBe(B);
    await expect(app.getByText("Unsaved changes")).toBeHidden();
    // A's reply (A dirty, with its note) comes after B is shown: B stays shown.
    await app.evaluate(() => (window as unknown as { releaseAck: () => void }).releaseAck());
    await nextFrame(app);
    await nextFrame(app);
    await expect(app.getByText("Unsaved changes")).toBeHidden();
    expect(await sessionNotes(mock)).toEqual([]);
    // B's step edits are B's: the grid's edit goes through.
    await app.keyboard.press("F6");
    await clickCell(app, 0, 2);
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 480, 240, 60]]);
  });

  test("the_shell_refuses_an_edit_made_for_the_project_it_replaced", async ({ app, mock }) => {
    const a = (await mock.view()).generation;
    const notesChanged = await app.evaluate(async ({ a, B }) => {
      const w = window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (cmd: string, args?: unknown) => Promise<unknown>;
          transformCallback: (cb: (e: { payload: unknown }) => void) => number;
        };
        heard: unknown[];
      };
      const ipc = w.__TAURI_INTERNALS__;
      await ipc.invoke("project_open", { path: B });
      // Listen once B is open: only what the late requests announce is heard.
      w.heard = [];
      const handler = ipc.transformCallback((e) => w.heard.push(e.payload));
      await ipc.invoke("plugin:event|listen", { event: "notes-changed", target: { kind: "Any" }, handler });
      const note = { channel: 1, tick: 0, length: 240, key: 64, velocity: 0.8, slide: false };
      const acks = [
        await ipc.invoke("notes_edit", { edit: { generation: a, baseRevision: 1, add: [note], update: [], delete: [1] } }),
        await ipc.invoke("project_edit", { edit: { kind: "toggleStep", channel: 1, step: 2 }, generation: a }),
        await ipc.invoke("notes_undo", { generation: a }),
        await ipc.invoke("notes_redo", { generation: a }),
      ];
      return { acks, heard: w.heard };
    }, { a, B });
    expect(await sessionNotes(mock)).toEqual([]);
    const view = await mock.view();
    expect(view.dirty).toBe(false);
    expect(view.generation).toBe(a + 1);
    expect(notesChanged.heard).toEqual([]);
    const [edit, step, undo, redo] = notesChanged.acks as { reload?: boolean; addedIds?: number[]; generation?: number }[];
    expect(edit).toMatchObject({ reload: true, addedIds: [] });
    expect(step).toMatchObject({ generation: a + 1, dirty: false });
    expect(undo).toMatchObject({ reload: true });
    expect(redo).toMatchObject({ reload: true });
  });
});
