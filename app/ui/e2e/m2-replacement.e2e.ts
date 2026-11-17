// Issue #186: a note edit made in one project never lands in the project that replaced it, from
// the piano roll's queue or straight from the shell's commands.
import type { Page } from "@playwright/test";
import { clickCell, expect, project, test, type Mock } from "./fixture";
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

const hold = (mock: Mock, cmd: string) => mock.eval(`(m) => m.holdCommand("${cmd}")`);
const release = (mock: Mock, cmd: string) => mock.eval(`(m) => m.releaseCommand("${cmd}")`);
/** Starts Open B and holds it in the shell, the old project still shown. */
async function openHeld(app: Page, mock: Mock) {
  await hold(mock, "project_open");
  await mock.eval(`(m) => m.queueDialog("${B}")`);
  await app.keyboard.press("ControlOrMeta+o");
  await expect.poll(async () => (await mock.calls("project_open")).length).toBe(1);
}

test.describe("a gesture during New or Open (fix round 1)", () => {
  test.use({
    seed: {
      project: twoChannels([]),
      files: { [B]: { kind: "project", project: twoChannels([]) } },
    },
  });

  test("a_gesture_begun_while_open_is_on_its_way_never_reaches_b", async ({ app, mock }) => {
    await openRoll(app);
    await openHeld(app, mock);
    const p = await at(app, 500, 65);
    await app.mouse.move(p.x, p.y);
    await app.mouse.down();
    await release(mock, "project_open");
    await expect.poll(() => mock.view().then((v) => v.path)).toBe(B);
    await nextFrame(app);
    await app.mouse.up();
    await nextFrame(app);
    await nextFrame(app);
    expect(await mock.calls("notes_edit")).toEqual([]);
    expect(await mock.view().then((v) => v.dirty)).toBe(false);
    expect(await sessionNotes(mock)).toEqual([]);
  });

  test("new_during_a_drag_lets_the_drag_go", async ({ app, mock }) => {
    await openRoll(app);
    const from = await at(app, 500, 65);
    const to = await at(app, 1500, 65);
    await app.mouse.move(from.x, from.y);
    await app.mouse.down();
    await app.mouse.move(to.x, to.y, { steps: 4 });
    await app.keyboard.press("ControlOrMeta+n");
    await expect.poll(async () => (await mock.calls("project_new")).length).toBe(1);
    await app.mouse.move(from.x, from.y, { steps: 2 });
    await app.mouse.up();
    await nextFrame(app);
    await nextFrame(app);
    expect(await mock.calls("notes_edit")).toEqual([]);
    expect(await sessionNotes(mock)).toEqual([]);
    expect(await mock.view().then((v) => v.dirty)).toBe(false);
  });
});

test.describe("a velocity drag across Open (fix round 1)", () => {
  // A clean A with one note on Bass, so Open needs no prompt while the pointer is held; B holds a
  // note under the same id, which A's drag must not reach.
  const note = { id: 1, channel: 1, tick: 960, length: 240, key: 67, velocity: 0.8 };
  test.use({
    seed: {
      project: twoChannels([note]),
      files: { [B]: { kind: "project", project: twoChannels([{ ...note, velocity: 0.3 }]) } },
    },
  });

  for (const when of ["before", "during"] as const) {
    test(`a_velocity_drag_begun_${when}_open_is_not_sent_to_b`, async ({ app, mock }) => {
      await openRoll(app);
      const p = await at(app, 960, 67);
      const lane = (await app.getByLabel("Velocity lane").boundingBox())!;
      if (when === "during") await openHeld(app, mock);
      await app.mouse.move(p.x + 2, lane.y + 4);
      await app.mouse.down();
      await app.mouse.move(p.x + 2, lane.y + 10, { steps: 2 });
      if (when === "before") await openHeld(app, mock);
      await release(mock, "project_open");
      await expect.poll(() => mock.view().then((v) => v.path)).toBe(B);
      await nextFrame(app);
      await app.mouse.up();
      await nextFrame(app);
      await nextFrame(app);
      expect(await mock.calls("notes_edit")).toEqual([]);
      expect(await mock.view().then((v) => v.dirty)).toBe(false);
      expect(await mock.eval<number[]>("(m) => m.notes().map((n) => n.velocity)")).toEqual([0.3]);
    });
  }
});

test.describe("channels added during New or Open (fix round 1)", () => {
  test.use({
    seed: {
      project: project(["A"]),
      files: { [B]: { kind: "project", project: project(["B"]) }, "/qa/late.wav": { kind: "sample" }, "/qa/later.wav": { kind: "sample" } },
    },
  });
  const names = async (mock: Mock) => (await mock.view()).channels.map((c) => c.name);

  for (const kind of ["synth", "sample"] as const) {
    test(`a_${kind}_added_while_open_is_on_its_way_stays_out_of_b`, async ({ app, mock }) => {
      await openHeld(app, mock);
      if (kind === "synth") {
        await app.getByRole("button", { name: "+ Add channel", exact: true }).click();
        await app.getByRole("menuitem", { name: "Init", exact: true }).click();
      } else {
        await mock.eval("(m) => m.drag(['/qa/late.wav'], true)");
      }
      await release(mock, "project_open");
      await expect.poll(() => mock.view().then((v) => v.path)).toBe(B);
      await nextFrame(app);
      expect(await names(mock)).toEqual(["B"]);
      expect(await mock.view().then((v) => v.dirty)).toBe(false);
      expect(await mock.calls(kind === "synth" ? "channel_add_synth" : "channel_add")).toEqual([]);
    });
  }

  test("a_dropped_batch_stops_at_a_replacement", async ({ app, mock }) => {
    // The first file is on its way when New starts: it lands in A, the second never goes.
    await hold(mock, "channel_add");
    await mock.eval("(m) => m.drag(['/qa/late.wav', '/qa/later.wav'], true)");
    await expect.poll(async () => (await mock.calls("channel_add")).length).toBe(1);
    await app.keyboard.press("ControlOrMeta+n");
    await app.getByRole("dialog", { name: "Save changes?" }).getByRole("button", { name: "Don't save" }).click();
    await release(mock, "channel_add");
    await expect.poll(async () => (await mock.calls("project_new")).length).toBe(1);
    await nextFrame(app);
    await nextFrame(app);
    expect((await mock.calls("channel_add")).length).toBe(1);
    expect(await names(mock)).toEqual([]);
  });

  test("a_file_picked_after_new_started_is_not_added", async ({ app, mock }) => {
    // The file dialog answers only after the project was replaced.
    await hold(mock, "plugin:dialog|open");
    await mock.eval("(m) => m.queueDialog(['/qa/late.wav'])");
    await app.getByRole("button", { name: "+ Add channel", exact: true }).click();
    await app.getByRole("menuitem", { name: "Sample…", exact: true }).click();
    await expect.poll(async () => (await mock.calls("plugin:dialog|open")).length).toBe(1);
    await app.keyboard.press("ControlOrMeta+n");
    await expect.poll(async () => (await mock.calls("project_new")).length).toBe(1);
    await release(mock, "plugin:dialog|open");
    await nextFrame(app);
    await nextFrame(app);
    expect(await mock.calls("channel_add")).toEqual([]);
    expect(await names(mock)).toEqual([]);
  });

  test("the_shell_refuses_channel_requests_made_for_a_replaced_project", async ({ app, mock }) => {
    const a = (await mock.view()).generation;
    const errors = await app.evaluate(
      async ({ a, B }) => {
        const ipc = (window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown) => Promise<unknown> } })
          .__TAURI_INTERNALS__;
        await ipc.invoke("project_open", { path: B });
        const out: string[] = [];
        for (const [cmd, args] of [
          ["channel_add", { path: "/qa/late.wav", generation: a }],
          ["channel_add_synth", { preset: "Init", generation: a }],
          ["channel_set_sample", { channel: 1, path: "/qa/late.wav", generation: a }],
        ] as const) {
          await ipc.invoke(cmd, args).then(
            () => out.push(`${cmd}: applied`),
            (e: unknown) => out.push(`${cmd}: ${String(e)}`),
          );
        }
        return out;
      },
      { a, B },
    );
    const refused = "The project changed before this finished, so it was not applied.";
    expect(errors).toEqual([`channel_add: ${refused}`, `channel_add_synth: ${refused}`, `channel_set_sample: ${refused}`]);
    expect(await names(mock)).toEqual(["B"]);
    expect(await mock.view().then((v) => v.dirty)).toBe(false);
  });
});

test.describe("Save while roll edits are on their way (#191)", () => {
  test.use({
    seed: {
      project: twoChannels([]),
      files: { [B]: { kind: "project", project: twoChannels([]) } },
    },
  });

  test("save_in_the_prompt_holds_every_queued_edit_before_the_replacement", async ({ app, mock }) => {
    await openRoll(app);
    await delayFirstAck(app);
    for (const tick of [480, 960]) {
      const p = await at(app, tick, 65);
      await app.mouse.click(p.x, p.y);
    }
    await expect.poll(async () => (await mock.calls("notes_edit")).length).toBe(1);
    // Save as A (untitled), then open B; B's Open is held to look at A's file before it runs.
    await hold(mock, "project_open");
    await mock.eval(`(m) => m.queueDialog("/qa/A.grooviq", "${B}")`);
    await app.keyboard.press("ControlOrMeta+o");
    await app.getByRole("dialog", { name: "Save changes?" }).getByRole("button", { name: "Save", exact: true }).click();
    // The save waits for the reply still out and the edit behind it.
    await nextFrame(app);
    expect(await mock.calls("project_save_as")).toEqual([]);
    await app.evaluate(() => (window as unknown as { releaseAck: () => void }).releaseAck());
    await expect.poll(async () => (await mock.calls("project_open")).length).toBe(1);
    expect((await mock.calls("notes_edit")).length).toBe(2);
    const saved = await mock.eval<{ project: { pattern: { notes: { tick: number; key: number }[] } } }>(
      "(m) => m.file('/qa/A.grooviq')",
    );
    expect(saved.project.pattern.notes.map((n) => [n.tick, n.key]).sort((x, y) => x[0] - y[0])).toEqual([
      [480, 65],
      [960, 65],
    ]);
    await release(mock, "project_open");
    await expect.poll(() => mock.view().then((v) => v.path)).toBe(B);
    expect(await sessionNotes(mock)).toEqual([]);
  });
});

test.describe("Save while a queued roll edit is refused (#191)", () => {
  // A pattern one id from used up: the first drawn note takes the last id, the second is refused.
  test.use({
    seed: {
      project: (() => {
        const p = twoChannels([]);
        p.pattern.nextNoteId = 0xfffffffe;
        return p;
      })(),
      files: { [B]: { kind: "project", project: twoChannels([]) } },
    },
  });

  for (const titled of [false, true]) {
    test(`a_refused_edit_stops_the_${titled ? "save" : "save_as"}_and_the_replacement`, async ({ app, mock }) => {
      if (titled) {
        // Saved once, empty: the prompt's Save then saves in place.
        await mock.eval(`(m) => m.queueDialog("/qa/A.grooviq")`);
        await app.keyboard.press("ControlOrMeta+s");
        await expect.poll(() => mock.view().then((v) => [v.path, v.dirty])).toEqual(["/qa/A.grooviq", false]);
      }
      await openRoll(app);
      await delayFirstAck(app);
      for (const tick of [480, 960]) {
        const p = await at(app, tick, 65);
        await app.mouse.click(p.x, p.y);
      }
      await expect.poll(async () => (await mock.calls("notes_edit")).length).toBe(1);
      await mock.eval(titled ? `(m) => m.queueDialog("${B}")` : `(m) => m.queueDialog("/qa/A.grooviq", "${B}")`);
      await app.keyboard.press("ControlOrMeta+o");
      await app.getByRole("dialog", { name: "Save changes?" }).getByRole("button", { name: "Save", exact: true }).click();
      await nextFrame(app);
      await app.evaluate(() => (window as unknown as { releaseAck: () => void }).releaseAck());
      const messages = app.getByRole("status", { name: "Messages" });
      await expect(messages).toContainText("This pattern has used up its note ids.");
      await expect(messages).toContainText("Your latest note edits could not be applied, so the project was not saved.");
      await nextFrame(app);
      await nextFrame(app);
      // Nothing saved, nothing opened: A is still open, unsaved, with the note that was applied.
      expect(await mock.calls("project_save")).toEqual([]);
      expect((await mock.calls("project_save_as")).length).toBe(titled ? 1 : 0);
      expect(await mock.calls("project_open")).toEqual([]);
      const file = await mock.eval<{ project: { pattern: { notes: unknown[] } } } | null>("(m) => m.file('/qa/A.grooviq')");
      expect(file?.project.pattern.notes ?? null).toEqual(titled ? [] : null);
      expect(await mock.view().then((v) => [v.path, v.dirty])).toEqual([titled ? "/qa/A.grooviq" : null, true]);
      expect(await sessionNotes(mock)).toEqual([[1, 480, 240, 65]]);
    });
  }
});
