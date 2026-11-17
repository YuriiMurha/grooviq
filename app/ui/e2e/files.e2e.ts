// Owner checklist 4, 5, 7, 8, 13 and 14: dropping files, previews, unsaved changes on close,
// recovery, export and a held Cmd+S, through the UI (the shell's own part runs in the real app
// in silent mode).
import { expect, project, soundsFor, test, type Mock } from "./fixture";

const KIT = ["Kick", "Snare", "Hat"];
const kitSeed = { project: project(KIT, { 1: [0, 8] }), files: soundsFor(KIT) };
/** The channel rows the rack shows, top to bottom. */
const rows = (app: import("@playwright/test").Page) =>
  app.getByRole("region", { name: "Channel rack" }).getByRole("group").evaluateAll((els) => els.map((e) => e.getAttribute("aria-label")));
const sounding = (cmd: string, args: Record<string, unknown>) =>
  cmd === "transport_play" ||
  cmd === "synth_note" ||
  (cmd === "channel_note" && args.on === true) ||
  (cmd === "test_tone" && args.on === true) ||
  (cmd === "channel_preview" && args.on === true);

async function makeDirty(mock: Mock, app: import("@playwright/test").Page) {
  await app.getByRole("button", { name: "Mute Kick" }).click();
  await expect.poll(() => mock.view().then((v) => v.dirty)).toBe(true);
}

test.describe("dropping files on the window (check 4)", () => {
  test.use({
    seed: {
      files: {
        "/qa/drop/Boom.wav": { kind: "sample" },
        "/qa/drop/Tick.flac": { kind: "sample" },
        "/qa/drop/notes.txt": { kind: "text" },
        "/qa/drop/Loops": { kind: "folder" },
        "/qa/drop/Shaker.ogg": { kind: "sample" },
      },
    },
  });

  test("the outline shows while hovering; channels come in drop order; others give a message", async ({ app, mock }, info) => {
    const rack = app.getByRole("region", { name: "Channel rack" });
    await mock.eval("(m) => m.drag(['/qa/drop/Boom.wav'], false)");
    await mock.eval("(m) => m.emit('tauri://drag-enter', { paths: ['/qa/drop/Boom.wav'], position: { x: 1, y: 1 } })");
    await expect(rack).toHaveClass(/drop-over/);
    await app.screenshot({ path: info.outputPath("drop-hover.png") });
    await mock.eval("(m) => m.emit('tauri://drag-leave', null)");
    await expect(rack).not.toHaveClass(/drop-over/);
    await mock.eval(
      "(m) => m.drag(['/qa/drop/Boom.wav', '/qa/drop/notes.txt', '/qa/drop/Tick.flac', '/qa/drop/Loops', '/qa/drop/Shaker.ogg'], true)",
    );
    await expect.poll(() => rows(app)).toEqual(["Boom", "Tick", "Shaker"]);
    await expect(rack).not.toHaveClass(/drop-over/);
    const messages = app.getByRole("status", { name: "Messages" });
    await expect(messages).toContainText('"notes.txt": This file is not a sound Grooviq can open');
    await expect(messages).toContainText('"Loops" could not be read: not a regular file.');
    await app.screenshot({ path: info.outputPath("drop-done.png") });
  });

  test("+ Add channel takes several files in the order picked", async ({ app, mock }) => {
    await mock.eval("(m) => m.queueDialog(['/qa/drop/Shaker.ogg', '/qa/drop/Boom.wav'])");
    await app.getByRole("button", { name: "+ Add channel" }).click();
    await app.getByRole("menuitem", { name: "Sample…" }).click();
    await expect.poll(() => rows(app)).toEqual(["Shaker", "Boom"]);
  });
});

test.describe("previews and silence (check 5)", () => {
  test.use({ seed: { ...kitSeed, files: { ...kitSeed.files, "/qa/projects/Other.grooviq": { kind: "project", project: project(KIT, { 2: [4] }) } } } });

  test("nothing sounds on launch or when a project opens", async ({ app, mock }) => {
    await app.waitForTimeout(500);
    await mock.eval("(m) => m.queueDialog('/qa/projects/Other.grooviq')");
    await app.keyboard.press("ControlOrMeta+o");
    await expect.poll(() => mock.view().then((v) => v.path)).toBe("/qa/projects/Other.grooviq");
    await app.waitForTimeout(300);
    const calls = await mock.calls();
    expect(calls.filter((c) => sounding(c.cmd, c.args)).map((c) => c.cmd)).toEqual([]);
  });

  test("holding a channel name previews it; releasing, inside or outside, stops it", async ({ app, mock }) => {
    const kick = app.getByRole("button", { name: /^Kick / });
    const previews = async () => (await mock.calls("channel_preview")).map((c) => c.args.on);
    await kick.hover();
    await app.mouse.down();
    await expect.poll(previews).toEqual([true]);
    await app.mouse.up();
    await expect.poll(previews).toEqual([true, false]);
    // Released outside the name.
    await kick.hover();
    await app.mouse.down();
    await app.mouse.move(600, 700, { steps: 5 });
    await app.mouse.up();
    await expect.poll(previews).toEqual([true, false, true, false]);
    // Held with Enter, then the window loses focus (as with Cmd+Tab): the name blurs.
    await kick.focus();
    await app.keyboard.down("Enter");
    await expect.poll(previews).toEqual([true, false, true, false, true]);
    await app.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
    await expect.poll(previews).toEqual([true, false, true, false, true, false]);
    await app.keyboard.up("Enter");
    expect(await previews()).toEqual([true, false, true, false, true, false]);
  });
});

test.describe("unsaved changes on close (check 7, the UI's part)", () => {
  test.use({ seed: kitSeed });

  test("Cancel keeps the app open", async ({ app, mock }, info) => {
    await makeDirty(mock, app);
    expect(await mock.eval<boolean>("(m) => m.requestClose()")).toBe(true);
    const dialog = app.getByRole("dialog", { name: "Save changes?" });
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText('Save the changes to "QA beat" before closing Grooviq?');
    for (const name of ["Save", "Don't save", "Cancel"]) await expect(dialog.getByRole("button", { name, exact: true })).toBeVisible();
    await info.attach("dialog-aria", { body: await dialog.ariaSnapshot(), contentType: "text/yaml" });
    await app.screenshot({ path: info.outputPath("save-changes.png") });
    await dialog.getByRole("button", { name: "Cancel" }).click();
    await expect(dialog).toBeHidden();
    expect(await mock.eval<number>("(m) => m.state.exitCount")).toBe(0);
  });

  test("Escape is Cancel", async ({ app, mock }) => {
    await makeDirty(mock, app);
    await mock.eval("(m) => m.requestClose()");
    await expect(app.getByRole("dialog", { name: "Save changes?" })).toBeVisible();
    await app.keyboard.press("Escape");
    await expect(app.getByRole("dialog", { name: "Save changes?" })).toBeHidden();
    expect(await mock.eval<number>("(m) => m.state.exitCount")).toBe(0);
  });

  test("Don't save quits without saving", async ({ app, mock }) => {
    await makeDirty(mock, app);
    await mock.eval("(m) => m.requestClose()");
    await app.getByRole("button", { name: "Don't save" }).click();
    await expect.poll(() => mock.eval<number>("(m) => m.state.exitCount")).toBe(1);
    expect(await mock.calls("project_save_as")).toEqual([]);
  });

  test("Save asks where for an untitled project, saves, then quits", async ({ app, mock }) => {
    await makeDirty(mock, app);
    await mock.eval("(m) => m.queueDialog('/qa/projects/Closing')");
    await mock.eval("(m) => m.requestClose()");
    await app.getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(() => mock.eval<number>("(m) => m.state.exitCount")).toBe(1);
    expect((await mock.view()).path).toBe("/qa/projects/Closing.grooviq");
    expect((await mock.view()).dirty).toBe(false);
  });

  test("Save cancelled at the file prompt does not quit", async ({ app, mock }) => {
    await makeDirty(mock, app);
    await mock.eval("(m) => m.queueDialog(null)");
    await mock.eval("(m) => m.requestClose()");
    await app.getByRole("button", { name: "Save", exact: true }).click();
    await app.waitForTimeout(300);
    expect(await mock.eval<number>("(m) => m.state.exitCount")).toBe(0);
  });
});

test.describe("recovery (check 8, the UI's part)", () => {
  test.use({
    seed: {
      ...kitSeed,
      recovery: { savedAt: 1_794_860_000, original: "/qa/projects/Beat.grooviq", project: project(KIT, { 1: [0, 4], 2: [8] }) },
    },
  });

  test("Restore opens the autosave unsaved; Save writes the original file", async ({ app, mock }, info) => {
    const dialog = app.getByRole("dialog", { name: "Restore your project?" });
    await expect(dialog).toBeVisible();
    await app.screenshot({ path: info.outputPath("restore.png") });
    // Keyboard path: Restore has focus, and Space presses it rather than starting playback.
    await expect(dialog.getByRole("button", { name: "Restore" })).toBeFocused();
    await app.keyboard.press("Space");
    await expect(dialog).toBeHidden();
    expect(await mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(false);
    await expect.poll(() => mock.view().then((v) => [v.path, v.dirty])).toEqual(["/qa/projects/Beat.grooviq", true]);
    await expect(app).toHaveTitle("• Beat — Grooviq");
    await app.keyboard.press("ControlOrMeta+s");
    await expect.poll(() => mock.view().then((v) => v.dirty)).toBe(false);
    expect(await mock.calls("project_save")).toHaveLength(1);
    expect(await mock.calls("plugin:dialog|save")).toEqual([]);
    expect(await mock.eval<boolean>("(m) => m.files.get('/qa/projects/Beat.grooviq')?.kind === 'project'")).toBe(true);
  });

  test("Discard throws the autosave away", async ({ app, mock }) => {
    await app.getByRole("button", { name: "Discard" }).click();
    await expect.poll(() => mock.calls("recovery_discard").then((c) => c.length)).toBe(1);
    expect((await mock.view()).path).toBeNull();
  });
});

test.describe("export (check 13)", () => {
  test.use({ seed: { ...kitSeed, exportMs: 1200 } });

  for (const [format, label] of [
    ["pcm24", "24-bit"],
    ["float32", "32-bit float"],
  ] as const) {
    test(`${label}: the bar moves and the result shows the peak`, async ({ app, mock }, info) => {
      await app.keyboard.press("ControlOrMeta+r");
      const dialog = app.getByRole("dialog", { name: "Export WAV" });
      await dialog.getByLabel(label).check();
      await mock.eval(`(m) => m.queueDialog('/qa/out/${format}')`);
      await dialog.getByRole("button", { name: "Export…" }).click();
      const bar = dialog.getByRole("progressbar", { name: "Export progress" });
      await expect.poll(async () => Number(await bar.getAttribute("aria-valuenow"))).toBeGreaterThan(0);
      const first = Number(await bar.getAttribute("aria-valuenow"));
      await app.screenshot({ path: info.outputPath(`export-running-${format}.png`) });
      await expect.poll(async () => Number((await bar.getAttribute("aria-valuenow")) ?? 100)).toBeGreaterThan(first);
      await expect(dialog.getByRole("status")).toHaveText("Exported. Peak -3.2 dBFS.");
      await app.screenshot({ path: info.outputPath(`export-done-${format}.png`) });
      const sent = await mock.calls("project_export");
      expect(sent.map((c) => c.args.options)).toEqual([{ sampleRate: 48000, format, loops: 1 }]);
      expect(await mock.eval<boolean>(`(m) => m.files.has('/qa/out/${format}.wav')`)).toBe(true);
    });
  }

  test.describe("held export cancellation", () => {
    test.use({ seed: { ...kitSeed, holdExport: true } });

    test("Cancel over an existing file keeps that file", async ({ app, mock }) => {
      await app.keyboard.press("ControlOrMeta+r");
      const dialog = app.getByRole("dialog", { name: "Export WAV" });
      await mock.eval("(m) => m.files.set('/qa/out/beat.wav', { kind: 'sample', seconds: 9 })");
      await mock.eval("(m) => m.queueDialog('/qa/out/beat.wav')");
      await dialog.getByRole("button", { name: "Export…" }).click();
      await expect.poll(async () => Number(await dialog.getByRole("progressbar").getAttribute("aria-valuenow"))).toBeGreaterThan(0);
      await dialog.getByRole("button", { name: "Cancel export" }).click();
      await expect.poll(() => mock.calls("project_export_cancel")).toHaveLength(1);
      await mock.eval("(m) => m.releaseExport()");
      await expect(dialog.getByRole("button", { name: "Export…" })).toBeVisible();
      expect(await mock.eval<number>("(m) => m.files.get('/qa/out/beat.wav')?.seconds")).toBe(9);
      expect(await mock.eval<string[]>("(m) => [...m.files.keys()].filter((k) => k.endsWith('.part'))")).toEqual([]);
    });

    test("Cancel leaves no file behind and goes back to the options", async ({ app, mock }) => {
      await app.keyboard.press("ControlOrMeta+r");
      const dialog = app.getByRole("dialog", { name: "Export WAV" });
      await mock.eval("(m) => m.queueDialog('/qa/out/cancelled.wav')");
      await dialog.getByRole("button", { name: "Export…" }).click();
      await expect.poll(async () => Number(await dialog.getByRole("progressbar").getAttribute("aria-valuenow"))).toBeGreaterThan(0);
      await dialog.getByRole("button", { name: "Cancel export" }).click();
      await expect.poll(() => mock.calls("project_export_cancel")).toHaveLength(1);
      await mock.eval("(m) => m.releaseExport()");
      await expect(dialog.getByRole("button", { name: "Export…" })).toBeVisible();
      await expect(dialog.getByRole("alert")).toHaveCount(0);
      expect(await mock.eval<boolean>("(m) => m.files.has('/qa/out/cancelled.wav')")).toBe(false);
    });
  });
});

test.describe("a held Cmd+S (check 14, the UI's part)", () => {
  test.use({ seed: { ...kitSeed, files: { ...kitSeed.files, "/qa/projects/Held.grooviq": { kind: "project", project: project(KIT) } } } });

  test("Save works as soon as the first project view commits", async ({ app, mock }) => {
    await app.addInitScript(() => {
      const observer = new MutationObserver(() => {
        if (![...document.querySelectorAll("button[aria-haspopup='menu']")].some((button) => button.textContent?.includes("QA beat"))) return;
        observer.disconnect();
        const shell = (window as unknown as { __grooviqMock: { queueDialog: (path: string) => void } }).__grooviqMock;
        shell.queueDialog("/qa/projects/Early.grooviq");
        const mac = /^mac/i.test(navigator.platform) || /iphone|ipad/i.test(navigator.platform);
        // Mutation observers run after the commit's layout effects, before passive effects.
        window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", metaKey: mac, ctrlKey: !mac, bubbles: true }));
      });
      observer.observe(document, { childList: true, subtree: true });
    });
    await app.reload();
    await expect.poll(() => mock.calls("project_save_as")).toHaveLength(1);
    await expect.poll(() => mock.view().then((view) => view.path)).toBe("/qa/projects/Early.grooviq");
  });

  test("saves once per press, not once per key repeat", async ({ app, mock }) => {
    await mock.eval("(m) => m.queueDialog('/qa/projects/Held.grooviq')");
    await app.keyboard.press("ControlOrMeta+o");
    await expect.poll(() => mock.view().then((v) => v.path)).toBe("/qa/projects/Held.grooviq");
    await makeDirty(mock, app);
    await app.keyboard.down("ControlOrMeta");
    for (let i = 0; i < 20; i++) {
      await app.keyboard.down("s");
      await app.waitForTimeout(100);
    }
    await app.keyboard.up("s");
    await app.keyboard.up("ControlOrMeta");
    await expect.poll(() => mock.view().then((v) => v.dirty)).toBe(false);
    expect(await mock.calls("project_save")).toHaveLength(1);
    expect(await mock.eval<number>("(m) => m.state.backups.get('/qa/projects/Held.grooviq')")).toBe(1);
  });
});
