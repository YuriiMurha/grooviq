import type { Locator, Page } from "@playwright/test";
import type { ArrangementPage } from "../src/api/arrangement";
import type { NotesAck } from "../src/api/notes";
import type { PatternsView } from "../src/api/patterns";
import type { ProjectView } from "../src/api/project";
import { clickCell, expect, project, soundsFor, test, type Mock } from "./fixture";

const seed = project(["Kick"], { 1: [0, 4, 8, 12] });
seed.pattern.name = "Source pattern";
seed.pattern.notes = seed.pattern.notes.map((note, index) => ({ ...note, id: index + 1,
  length: 240, key: 60, velocity: 0.5, slide: false }));
seed.pattern.nextNoteId = 5;
const original = seed.pattern;
seed.pattern = { id: 3, name: "Unrelated short pattern", steps: 4, swing: 0, notes: [], nextNoteId: 1 };
seed.patterns = [original, seed.pattern]; seed.nextId = 6;
seed.arrangement = {
  tracks: [{ id: 2, name: "Retained track", color: 0, mute: false }],
  clips: [
    { id: 4, track: 2, name: "Original instance", startTick: 960, muted: false,
      content: { type: "pattern", pattern: 0, offsetTick: 480, lengthTicks: 3840 } },
    { id: 5, track: 2, name: "Linked instance", startTick: 6720, muted: false,
      content: { type: "pattern", pattern: 0, offsetTick: 480, lengthTicks: 3840 } },
  ], audioAssets: [], loopRegion: null,
};
test.use({ seed: { project: seed, files: soundsFor(["Kick"]) } });
function ipc<Result>(app: Page, command: string, args: Record<string, unknown> = {}): Promise<Result> {
  return app.evaluate(({ command, args }) => (window as unknown as {
    __TAURI_INTERNALS__: { invoke: (command: string, args: Record<string, unknown>) => Promise<unknown> }
  }).__TAURI_INTERNALS__.invoke(command, args), { command, args }) as Promise<Result>;
}
const state = (app: Page) => ipc<ArrangementPage>(app, "arrangement_view", { offset: 0, limit: 256 });
const view = (app: Page) => ipc<ProjectView>(app, "project_view");
const list = (app: Page) => ipc<PatternsView>(app, "pattern_list");
const unique = (panel: Locator) => panel.getByRole("button", { name: "Make selected clip unique", exact: true });
const clonedClips = (before: ArrangementPage) => [
  { ...before.clips[0], content: { type: "pattern", pattern: 6, offsetTick: 480, lengthTicks: 3840 } }, before.clips[1],
];
async function selections(mock: Mock) {
  return (await mock.calls("pattern_edit")).filter(call => {
    const edit = call.args.edit;
    return typeof edit === "object" && edit !== null && "kind" in edit && edit.kind === "select";
  });
}
async function selected(app: Page) {
  const rail = app.getByRole("complementary", { name: "Patterns" });
  await expect(rail).toHaveAttribute("aria-busy", "false");
  await app.keyboard.press("F5");
  const panel = app.getByRole("region", { name: "Playlist", exact: true });
  const canvas = panel.getByRole("application", { name: "Playlist clips" });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await expect(panel.getByRole("status")).toHaveText("2 clips");
  await panel.getByRole("combobox", { name: "Selected playlist clip" }).selectOption("4");
  await expect(panel.getByRole("combobox", { name: "Selected playlist clip" })).toHaveValue("4");
  expect((await state(app)).clips).toEqual(seed.arrangement?.clips);
  expect((await list(app)).patterns.map(pattern => [pattern.id, pattern.steps])).toEqual([[0, 16], [3, 4]]);
  expect((await view(app)).pattern.id).toBe(3); expect((await view(app)).dirty).toBe(false);
  return { panel, canvas, rail };
}
async function currentClone(app: Page, panel: Locator) {
  await expect.poll(async () => (await view(app)).pattern.id).toBe(6);
  await expect(panel.getByRole("application", { name: "Playlist clips" })).toHaveAttribute("aria-disabled", "false");
  await expect(app.getByRole("complementary", { name: "Patterns" }).locator('button[data-pattern="6"]')).toHaveAttribute("aria-current", "true");
  await expect(panel.getByRole("combobox", { name: "Selected playlist clip" })).toHaveValue("4");
}

test("Make unique selects the actual cloned source and preserves the eight-stage global inverse trace", async ({ app, mock }) => {
  const { panel } = await selected(app); const before = await state(app);
  // This feature RED follows original native-shaped state/selection/source premises.
  await expect(unique(panel)).toBeVisible();
  const observe = async () => {
    const arrangement = await state(app), current = await view(app);
    // Pure observation of the existing finite bank, added with the fixture repair AFTER RED.
    // It is not a preset native certificate. Real Session supplies the independent trace.
    return { clips: arrangement.clips, tracks: arrangement.tracks, patterns: await mock.eval("m => m.patterns()"),
      nextId: await mock.eval<number>("m => m.state.project.nextId"), selectedPattern: current.pattern.id,
      dirty: current.dirty, canUndo: current.projectCanUndo, canRedo: current.projectCanRedo };
  };
  const trace = [{ stage: "initial", value: await observe() }];
  await mock.eval("m => m.holdCommand('pattern_edit')"); await unique(panel).click();
  await expect.poll(async () => (await selections(mock)).length).toBe(1);
  await expect.poll(async () => (await state(app)).clips).toEqual(clonedClips(before));
  expect((await view(app)).pattern.id).toBe(3);
  expect((await selections(mock))[0].args.edit).toEqual({ kind: "select", pattern: 6 });
  expect(await mock.calls("arrangement_edit")).toHaveLength(1);
  expect(JSON.parse(String((await mock.calls("arrangement_edit"))[0].args.editsJson))).toEqual([{ kind: "makeUnique", clip: 4 }]);
  trace.push({ stage: "unique", value: await observe() });
  await mock.eval("m => m.releaseCommand('pattern_edit')"); await currentClone(app, panel);
  trace.push({ stage: "selected", value: await observe() });
  const current = await view(app);
  const ack = await ipc<NotesAck>(app, "notes_edit", { edit: { generation: current.generation, patternId: 6,
    baseRevision: current.notesRevision, add: [{ channel: 1, tick: 240, length: 240, key: 60, velocity: 0.5, slide: false }], update: [], delete: [] } });
  expect(ack.addedIds).toEqual([5]); expect(ack.reload).toBe(false);
  trace.push({ stage: "edited", value: await observe() });
  await app.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => mock.eval<number>("m => m.patterns().find(p => p.id === 6).notes.length")).toBe(4);
  trace.push({ stage: "noteUndone", value: await observe() });
  await app.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await state(app)).clips).toEqual(before.clips);
  expect((await list(app)).patterns.map(pattern => pattern.id)).toEqual([0, 3]);
  expect((await view(app)).pattern.id).toBe(0);
  trace.push({ stage: "uniqueUndone", value: await observe() });
  await app.keyboard.press("ControlOrMeta+Shift+z");
  await expect.poll(async () => (await state(app)).clips).toEqual(clonedClips(before));
  expect((await list(app)).patterns.map(pattern => pattern.id)).toEqual([0, 3, 6]);
  expect((await view(app)).pattern.id).toBe(0);
  trace.push({ stage: "uniqueRedone", value: await observe() });
  await app.keyboard.press("ControlOrMeta+Shift+z");
  await expect.poll(() => mock.eval<number>("m => m.patterns().find(p => p.id === 6).notes.length")).toBe(5);
  trace.push({ stage: "noteRedone", value: await observe() });
  expect(await mock.eval<number>("m => m.state.project.nextId")).toBe(7);
  expect(await mock.eval("m => m.patterns().find(p => p.id === 0).notes")).toEqual(original.notes);
  expect((await state(app)).tracks).toEqual(before.tracks);
  expect(await mock.calls("notes_undo")).toHaveLength(0);
  expect(await mock.calls("transport_play")).toHaveLength(0);
  await test.info().attach("m3-clip-unique-trace", { body: JSON.stringify(trace), contentType: "application/json" });
  for (const viewport of [{ width: 1280, height: 800 }, { width: 900, height: 560 }]) {
    await app.setViewportSize(viewport); await expect(unique(panel)).toBeVisible();
    await app.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
    await test.info().attach(`unique-candidate-${viewport.width}`, { body: await app.screenshot(), contentType: "image/png" });
  }
});

test("actual rack editing changes only the unique source and global Undo/Redo retains linked originals", async ({ app, mock }) => {
  const { panel } = await selected(app); const before = await state(app);
  await unique(panel).click(); await currentClone(app, panel);
  await clickCell(app, 0, 1);
  await expect.poll(() => mock.eval<number>("m => m.notes().length")).toBe(5);
  expect(await mock.eval("m => m.patterns().find(p => p.id === 0).notes")).toEqual(original.notes);
  expect((await state(app)).clips).toEqual(clonedClips(before));
  await app.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => mock.eval<number>("m => m.notes().length")).toBe(4);
  await app.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await state(app)).clips).toEqual(before.clips);
  await app.keyboard.press("ControlOrMeta+Shift+z");
  await expect.poll(async () => (await state(app)).clips).toEqual(clonedClips(before));
  await app.keyboard.press("ControlOrMeta+Shift+z");
  await expect.poll(() => mock.eval<number>("m => m.patterns().find(p => p.id === 6).notes.length")).toBe(5);
  expect(await mock.eval("m => m.patterns().find(p => p.id === 0).notes")).toEqual(original.notes);
  expect(await mock.eval<number>("m => m.state.project.nextId")).toBe(7);
});

test("Save waits through accepted cloning and held source selection before writing the final project", async ({ app, mock }) => {
  const { panel } = await selected(app); const before = await state(app);
  const path = "/qa/projects/unique-selected.grooviq";
  await mock.eval("m => m.holdCommand('pattern_edit')"); await unique(panel).click();
  await expect.poll(async () => (await selections(mock)).length).toBe(1);
  expect((await state(app)).clips).toEqual(clonedClips(before));
  expect((await view(app)).pattern.id).toBe(3);
  await mock.eval(`m => m.queueDialog('${path}')`); await app.keyboard.press("ControlOrMeta+s");
  await expect.poll(async () => (await mock.calls("plugin:dialog|save")).length).toBe(1);
  expect(await mock.calls("project_save_as")).toHaveLength(0);
  await mock.eval("m => m.releaseCommand('pattern_edit')");
  await expect.poll(async () => (await mock.calls("project_save_as")).length).toBe(1);
  await expect.poll(async () => (await view(app)).dirty).toBe(false);
  const saved = await mock.eval<{ project: { pattern: { id: number }; patterns: { id: number; notes: unknown[] }[]; arrangement: { clips: unknown[] }; nextId: number } }>(`m => m.file('${path}')`);
  expect(saved.project.pattern.id).toBe(6); expect(saved.project.arrangement.clips).toEqual(clonedClips(before));
  expect(saved.project.patterns.map(pattern => pattern.id)).toEqual([0, 3, 6]); expect(saved.project.nextId).toBe(7);
  expect(saved.project.patterns.find(pattern => pattern.id === 6)?.notes).toEqual(original.notes);
  await currentClone(app, panel);
  await expect(app.getByText(/^Saved /)).toBeVisible();
});

test("refused cloning never selects or allocates a source and prevents the waiting Save", async ({ app, mock }) => {
  const { panel } = await selected(app); const before = await state(app), initial = await view(app);
  await mock.eval("m => { m.holdCommand('arrangement_edit'); m.refuseWorkspaceNext('generated unique refusal'); }");
  await unique(panel).click(); await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(1);
  await mock.eval("m => m.queueDialog('/qa/projects/refused-unique.grooviq')");
  await app.keyboard.press("ControlOrMeta+s");
  await expect.poll(async () => (await mock.calls("plugin:dialog|save")).length).toBe(1);
  await mock.eval("m => m.releaseCommand('arrangement_edit')");
  await expect(panel.getByRole("alert")).toContainText("generated unique refusal");
  await expect(app.getByRole("status", { name: "Messages" })).toContainText("not saved");
  expect((await state(app)).clips).toEqual(before.clips); expect((await list(app)).patterns.map(pattern => pattern.id)).toEqual([0, 3]);
  expect(await mock.eval<number>("m => m.state.project.nextId")).toBe(6);
  expect(await view(app)).toMatchObject({ pattern: { id: 3 }, dirty: false, generation: initial.generation,
    projectCanUndo: initial.projectCanUndo, projectCanRedo: initial.projectCanRedo });
  expect(await selections(mock)).toHaveLength(0); expect(await mock.calls("project_save_as")).toHaveLength(0);
});

test("failed selection preserves an accepted unique edit and blocks blind retry until explicit recovery", async ({ app, mock }) => {
  const { panel, rail } = await selected(app); const before = await state(app);
  await mock.eval("m => { m.holdCommand('pattern_edit'); m.refuseNext('pattern_edit', 'generated selection refusal'); }");
  await unique(panel).click(); await expect.poll(async () => (await selections(mock)).length).toBe(1);
  expect((await state(app)).clips).toEqual(clonedClips(before));
  await mock.eval("m => m.queueDialog('/qa/projects/refused-selection.grooviq')");
  await app.keyboard.press("ControlOrMeta+s");
  await expect.poll(async () => (await mock.calls("plugin:dialog|save")).length).toBe(1);
  await mock.eval("m => m.releaseCommand('pattern_edit')");
  await expect(panel.getByRole("alert")).toContainText("generated selection refusal");
  await expect(app.getByRole("status", { name: "Messages" })).toContainText("not saved");
  expect((await state(app)).clips).toEqual(clonedClips(before)); expect((await view(app)).pattern.id).toBe(3);
  expect((await view(app)).dirty).toBe(true); expect(await mock.eval<number>("m => m.state.project.nextId")).toBe(7);
  await expect(unique(panel)).toBeDisabled(); expect(await mock.calls("project_save_as")).toHaveLength(0);
  expect(await mock.calls("arrangement_edit")).toHaveLength(1);
  await panel.getByRole("button", { name: "Refresh playlist", exact: true }).click();
  await expect(panel.getByRole("application", { name: "Playlist clips" })).toHaveAttribute("aria-disabled", "false");
  expect((await state(app)).clips).toEqual(clonedClips(before)); expect(await mock.calls("arrangement_edit")).toHaveLength(1);
  await rail.locator('button[data-pattern="6"]').click(); await currentClone(app, panel);
  const path = "/qa/projects/unique-recovered.grooviq";
  await mock.eval(`m => m.queueDialog('${path}')`); await app.keyboard.press("ControlOrMeta+s");
  await expect.poll(async () => (await view(app)).dirty).toBe(false);
  const saved = await mock.eval<{ project: { nextId: number; arrangement: { clips: unknown[] } } }>(`m => m.file('${path}')`);
  expect(saved.project.nextId).toBe(7); expect(saved.project.arrangement.clips).toEqual(clonedClips(before));
});

test("post-clone source-observation failure remains recoverable without another clone", async ({ app, mock }) => {
  const { panel } = await selected(app); const before = await state(app);
  await mock.eval("m => m.refuseNext('pattern_list', 'generated source observation failure')");
  await unique(panel).click();
  await expect(panel.getByRole("alert")).toContainText("generated source observation failure");
  expect((await state(app)).clips).toEqual(clonedClips(before)); expect(await selections(mock)).toHaveLength(0);
  expect(await mock.eval<number>("m => m.state.project.nextId")).toBe(7); await expect(unique(panel)).toBeDisabled();
  await panel.getByRole("button", { name: "Refresh playlist", exact: true }).click();
  await expect(panel.getByRole("application", { name: "Playlist clips" })).toHaveAttribute("aria-disabled", "false");
  expect((await state(app)).clips).toEqual(clonedClips(before)); expect(await mock.calls("arrangement_edit")).toHaveLength(1);
});

test("clean pending Make unique prompts before New and Cancel retains clone and selected source", async ({ app, mock }) => {
  const { panel } = await selected(app); const before = await state(app);
  await mock.eval("m => m.holdCommand('arrangement_edit')"); await unique(panel).click();
  await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(1);
  expect((await view(app)).dirty).toBe(false); expect((await state(app)).clips).toEqual(before.clips);
  await app.keyboard.press("ControlOrMeta+n");
  await expect(app.getByRole("dialog", { name: "Save changes?" })).toBeVisible(); expect(await mock.calls("project_new")).toHaveLength(0);
  await app.getByRole("button", { name: "Cancel", exact: true }).click(); await mock.eval("m => m.releaseCommand('arrangement_edit')");
  await currentClone(app, panel); expect((await state(app)).clips).toEqual(clonedClips(before));
  expect((await state(app)).generation).toBe(before.generation); expect(await mock.calls("project_new")).toHaveLength(0);
});

test("keyboard Make unique cancels original pointer capture before a later release can move the clip", async ({ app, mock }) => {
  const { panel, canvas } = await selected(app); const before = await state(app);
  await canvas.evaluate(element => element.addEventListener("pointerdown", event => {
    element.setAttribute("data-qa-unique-pointer", String((event as PointerEvent).pointerId));
  }, { once: true }));
  const box = await canvas.boundingBox(); if (!box || box.width <= 240 || box.height <= 40) throw new Error("No positive actual gesture area");
  await app.mouse.move(box.x + 144, box.y + 40); await app.mouse.down();
  await app.mouse.move(box.x + 192, box.y + 40, { steps: 3 });
  await expect.poll(() => canvas.evaluate(element => element.hasPointerCapture(Number(element.getAttribute("data-qa-unique-pointer"))))).toBe(true);
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
  await unique(panel).focus(); await unique(panel).press("Enter");
  await expect.poll(() => canvas.evaluate(element => element.hasPointerCapture(Number(element.getAttribute("data-qa-unique-pointer"))))).toBe(false);
  await app.mouse.up(); await currentClone(app, panel);
  expect((await state(app)).clips).toEqual(clonedClips(before));
  const calls = await mock.calls("arrangement_edit"); expect(calls).toHaveLength(1);
  expect(JSON.parse(String(calls[0].args.editsJson))).toEqual([{ kind: "makeUnique", clip: 4 }]);
});

test("F5 hides the view without releasing an admitted clone or the waiting Save ticket", async ({ app, mock }) => {
  const { panel } = await selected(app); const before = await state(app);
  await mock.eval("m => m.holdCommand('pattern_edit')"); await unique(panel).click();
  await expect.poll(async () => (await selections(mock)).length).toBe(1);
  await app.keyboard.press("F5"); await expect(panel).toHaveCount(0);
  const path = "/qa/projects/unique-hidden.grooviq";
  await mock.eval(`m => m.queueDialog('${path}')`); await app.keyboard.press("ControlOrMeta+s");
  expect(await mock.calls("project_save_as")).toHaveLength(0);
  await mock.eval("m => m.releaseCommand('pattern_edit')");
  await expect.poll(async () => (await mock.calls("project_save_as")).length).toBe(1);
  await expect.poll(async () => (await view(app)).dirty).toBe(false);
  expect((await view(app)).pattern.id).toBe(6); expect((await state(app)).clips).toEqual(clonedClips(before));
  await app.keyboard.press("F5"); await expect(panel.getByRole("status")).toHaveText("2 clips");
  expect(await mock.calls("arrangement_edit")).toHaveLength(1);
});

test("true New removes the unique source and cannot revive its previous clip selection", async ({ app, mock }) => {
  const { panel } = await selected(app); const before = await state(app);
  await unique(panel).click(); await currentClone(app, panel);
  await app.keyboard.press("ControlOrMeta+n");
  await expect(app.getByRole("dialog", { name: "Save changes?" })).toBeVisible();
  await app.getByRole("button", { name: "Don't save", exact: true }).click();
  await expect.poll(async () => (await state(app)).generation).toBeGreaterThan(before.generation);
  await expect(panel.getByRole("status")).toHaveText("0 clips");
  await expect(panel.getByRole("combobox", { name: "Selected playlist clip" })).toHaveValue("");
  expect((await list(app)).patterns.map(pattern => pattern.id)).toEqual([0]); expect((await view(app)).pattern.id).toBe(0);
  expect(await mock.calls("arrangement_edit")).toHaveLength(1); expect(await mock.calls("transport_play")).toHaveLength(0);
});
