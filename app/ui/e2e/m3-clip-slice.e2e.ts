import type { Locator, Page } from "@playwright/test";
import type { ArrangementPage } from "../src/api/arrangement";
import type { PatternsView } from "../src/api/patterns";
import type { ProjectView } from "../src/api/project";
import { expect, project, soundsFor, test } from "./fixture";

const seed = project(["Kick"], { 1: [0, 4, 8, 12] });
const source = seed.pattern;
seed.pattern = { id: 3, name: "Unrelated short pattern", steps: 4, swing: 0, notes: [] };
seed.patterns = [source, seed.pattern];
seed.nextId = 6;
seed.arrangement = {
  tracks: [{ id: 2, name: "Retained track", color: 0, mute: false }],
  clips: [
    { id: 4, track: 2, name: "Original instance", startTick: 960, muted: false,
      content: { type: "pattern", pattern: 0, offsetTick: 480, lengthTicks: 7680 } },
    { id: 5, track: 2, name: "Linked instance", startTick: 9600, muted: false,
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
async function selected(app: Page) {
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await app.keyboard.press("F5");
  const panel = app.getByRole("region", { name: "Playlist", exact: true });
  const canvas = panel.getByRole("application", { name: "Playlist clips" });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await expect(panel.getByRole("status")).toHaveText("2 clips");
  await panel.getByRole("combobox", { name: "Selected playlist clip" }).selectOption("4");
  await expect(panel.getByRole("combobox", { name: "Selected playlist clip" })).toHaveValue("4");
  expect((await state(app)).clips).toEqual(seed.arrangement?.clips);
  expect((await view(app)).pattern.id).toBe(3);
  const patterns = await ipc<PatternsView>(app, "pattern_list");
  expect(patterns.patterns.map(pattern => [pattern.id, pattern.steps])).toEqual([[0, 16], [3, 4]]);
  expect((await view(app)).dirty).toBe(false);
  return { panel, canvas };
}
async function chooseSlice(panel: Locator) {
  const button = panel.getByRole("button", { name: "Slice", exact: true });
  await expect(button).toBeVisible();
  await button.click();
  await expect(button).toHaveAttribute("aria-pressed", "true");
}
async function begin(app: Page, canvas: Locator, pointerTick = 2940) {
  const box = await canvas.boundingBox();
  if (!box || box.width <= pointerTick * 0.05 || box.height <= 40) throw new Error("No positive actual cut area");
  await canvas.evaluate(element => element.addEventListener("pointerdown", event => {
    element.setAttribute("data-qa-slice-pointer", String((event as PointerEvent).pointerId));
  }, { once: true }));
  await app.mouse.move(box.x + pointerTick * 0.05, box.y + 40);
  await app.mouse.down();
  await expect.poll(() => canvas.evaluate(element => element.hasPointerCapture(Number(element.getAttribute("data-qa-slice-pointer"))))).toBe(true);
}
const captured = (canvas: Locator) => canvas.evaluate(element => element.hasPointerCapture(Number(element.getAttribute("data-qa-slice-pointer"))));
const splitAt = (before: ArrangementPage, tick: number, leftLength: number, phase: number, rightLength: number) => [
  { ...before.clips[0], content: { type: "pattern", pattern: 0, offsetTick: 480, lengthTicks: leftLength } },
  before.clips[1],
  { ...before.clips[0], id: 6, startTick: tick,
    content: { type: "pattern", pattern: 0, offsetTick: phase, lengthTicks: rightLength } },
];

test("Slice releases one snapped linked split, retains left selection and global Undo/Redo identities", async ({ app, mock }) => {
  const { panel, canvas } = await selected(app);
  const before = await state(app), patterns = await mock.eval("m => m.state.project.patterns");
  const observe = async () => {
    const arrangement = await state(app), current = await view(app);
    return { clips: arrangement.clips, tracks: arrangement.tracks,
      nextId: await mock.eval<number>("m => m.state.project.nextId"), dirty: current.dirty,
      canUndo: current.projectCanUndo, canRedo: current.projectCanRedo, selectedPattern: current.pattern.id };
  };
  const trace = [{ stage: "initial", value: await observe() }];
  const calls = (await mock.calls("arrangement_edit")).length;
  // Missing feature RED is reached only after the original project/source/selection oracles.
  await chooseSlice(panel);
  await begin(app, canvas);
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls);
  expect((await state(app)).clips).toEqual(before.clips);
  await app.mouse.up();
  const expected = splitAt(before, 2880, 1920, 2400, 5760);
  await expect.poll(async () => (await state(app)).clips).toEqual(expected);
  const sent = (await mock.calls("arrangement_edit")).slice(calls);
  expect(sent).toHaveLength(1);
  expect(sent[0].args).toMatchObject({ generation: before.generation, revision: before.revision });
  expect(JSON.parse(String(sent[0].args.editsJson))).toEqual([{ kind: "slice", clip: 4, tick: 2880, audioFirstFrames: null }]);
  expect((await state(app)).tracks).toEqual(before.tracks);
  expect(await mock.eval("m => m.state.project.patterns")).toEqual(patterns);
  await expect(panel.getByRole("combobox", { name: "Selected playlist clip" })).toHaveValue("4");
  expect(await mock.eval<number>("m => m.state.project.nextId")).toBe(7);
  trace.push({ stage: "sliced", value: await observe() });
  await app.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await state(app)).clips).toEqual(before.clips);
  expect(await mock.eval<number>("m => m.state.project.nextId")).toBe(7);
  expect((await view(app)).projectCanRedo).toBe(true);
  trace.push({ stage: "undone", value: await observe() });
  await app.keyboard.press("ControlOrMeta+Shift+z");
  await expect.poll(async () => (await state(app)).clips).toEqual(expected);
  trace.push({ stage: "redone", value: await observe() });
  await expect(panel.getByRole("combobox", { name: "Selected playlist clip" })).toHaveValue("4");
  expect(await mock.calls("transport_play")).toHaveLength(0);
  await test.info().attach("m3-clip-slice-trace", { body: JSON.stringify(trace), contentType: "application/json" });
  for (const viewport of [{ width: 1280, height: 800 }, { width: 900, height: 560 }]) {
    await app.setViewportSize(viewport);
    await expect(panel.getByRole("button", { name: "Slice", exact: true })).toBeVisible();
    await expect(canvas).toHaveAttribute("aria-disabled", "false");
    await app.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
    await test.info().attach(`slice-candidate-${viewport.width}`, { body: await app.screenshot(), contentType: "image/png" });
  }
});

for (const cut of [
  { tick: 4800, left: 3840, phase: 480, right: 3840 },
  { tick: 5760, left: 4800, phase: 1440, right: 2880 },
]) test(`Slice at ${cut.tick} wraps the actual unselected source period`, async ({ app, mock }) => {
  const { panel, canvas } = await selected(app); const before = await state(app);
  await chooseSlice(panel); await begin(app, canvas, cut.tick); await app.mouse.up();
  await expect.poll(async () => (await state(app)).clips).toEqual(splitAt(before, cut.tick, cut.left, cut.phase, cut.right));
  expect((await view(app)).pattern.id).toBe(3);
  expect(await mock.calls("arrangement_edit")).toHaveLength(1);
  await expect(panel.getByRole("combobox", { name: "Selected playlist clip" })).toHaveValue("4");
});

test("Slice endpoints and empty space neither resize nor draw, including Alt at an edge", async ({ app, mock }) => {
  const { panel, canvas } = await selected(app); const before = await state(app), initial = await view(app);
  await chooseSlice(panel);
  const box = await canvas.boundingBox(); if (!box || box.width <= 440) throw new Error("No positive endpoint area");
  await app.keyboard.down("Alt");
  for (const tick of [960, 8640, 240]) await app.mouse.click(box.x + tick * 0.05, box.y + 40);
  await app.keyboard.up("Alt");
  expect((await state(app)).clips).toEqual(before.clips);
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
  expect(await mock.eval<number>("m => m.state.project.nextId")).toBe(6);
  expect(await view(app)).toMatchObject({ generation: initial.generation, dirty: false,
    projectCanUndo: initial.projectCanUndo, projectCanRedo: initial.projectCanRedo });
});

test("Slice takes precedence over Alt-copy without changing the linked instance", async ({ app, mock }) => {
  const { panel, canvas } = await selected(app); const before = await state(app);
  await chooseSlice(panel); await app.keyboard.down("Alt");
  await begin(app, canvas); await app.mouse.up(); await app.keyboard.up("Alt");
  await expect.poll(async () => (await state(app)).clips).toEqual(splitAt(before, 2880, 1920, 2400, 5760));
  const calls = await mock.calls("arrangement_edit"); expect(calls).toHaveLength(1);
  expect(JSON.parse(String(calls[0].args.editsJson))).toEqual([{ kind: "slice", clip: 4, tick: 2880, audioFirstFrames: null }]);
});

for (const cancel of ["Escape", "capture loss", "F5"]) test(`${cancel} cancels a captured cut before release`, async ({ app, mock }) => {
  const { panel, canvas } = await selected(app); const before = await state(app);
  await chooseSlice(panel); await begin(app, canvas);
  if (cancel === "capture loss") await canvas.evaluate(element => element.releasePointerCapture(Number(element.getAttribute("data-qa-slice-pointer"))));
  else await app.keyboard.press(cancel);
  if (cancel !== "F5") await expect.poll(() => captured(canvas)).toBe(false);
  else await expect(panel).toHaveCount(0);
  await app.mouse.up();
  if (cancel === "F5") await app.keyboard.press("F5");
  expect((await state(app)).clips).toEqual(before.clips);
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
  expect(await mock.eval<number>("m => m.state.project.nextId")).toBe(6);
  expect((await view(app)).dirty).toBe(false);
});

test("Save waits for an admitted Slice and persists both exact fragments", async ({ app, mock }) => {
  const { panel, canvas } = await selected(app); const before = await state(app);
  const path = "/qa/projects/clip-slice.grooviq";
  await chooseSlice(panel); await mock.eval("m => m.holdCommand('arrangement_edit')");
  await begin(app, canvas); await app.mouse.up();
  await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(1);
  expect((await state(app)).clips).toEqual(before.clips);
  await mock.eval(`m => m.queueDialog('${path}')`); await app.keyboard.press("ControlOrMeta+s");
  expect(await mock.calls("project_save_as")).toHaveLength(0);
  await mock.eval("m => m.releaseCommand('arrangement_edit')");
  await expect.poll(async () => (await mock.calls("project_save_as")).length).toBe(1);
  await expect.poll(async () => (await view(app)).dirty).toBe(false);
  const saved = await mock.eval<{ project: { arrangement: { clips: unknown[]; tracks: unknown[] }; nextId: number } }>(`m => m.file('${path}')`);
  expect(saved.project.arrangement.clips).toEqual(splitAt(before, 2880, 1920, 2400, 5760));
  expect(saved.project.arrangement.tracks).toEqual(before.tracks); expect(saved.project.nextId).toBe(7);
  await expect(panel.getByRole("combobox", { name: "Selected playlist clip" })).toHaveValue("4");
});

test("refused Slice preserves identity, allocator and history and blocks the waiting Save", async ({ app, mock }) => {
  const { panel, canvas } = await selected(app); const before = await state(app), initial = await view(app);
  await chooseSlice(panel);
  await mock.eval("m => { m.holdCommand('arrangement_edit'); m.refuseWorkspaceNext('generated slice refusal'); }");
  await begin(app, canvas); await app.mouse.up();
  await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(1);
  await app.keyboard.press("ControlOrMeta+s");
  await mock.eval("m => m.releaseCommand('arrangement_edit')");
  await expect(panel.getByRole("alert")).toContainText("generated slice refusal");
  expect((await state(app)).clips).toEqual(before.clips);
  expect((await state(app)).tracks).toEqual(before.tracks);
  expect(await mock.eval<number>("m => m.state.project.nextId")).toBe(6);
  expect(await view(app)).toMatchObject({ generation: initial.generation, dirty: false,
    projectCanUndo: initial.projectCanUndo, projectCanRedo: initial.projectCanRedo });
  await expect(panel.getByRole("combobox", { name: "Selected playlist clip" })).toHaveValue("4");
  expect(await mock.calls("project_save_as")).toHaveLength(0);
});

test("clean pending Slice prompts before New and Cancel preserves the accepted edit", async ({ app, mock }) => {
  const { panel, canvas } = await selected(app); const before = await state(app);
  await chooseSlice(panel); await mock.eval("m => m.holdCommand('arrangement_edit')");
  await begin(app, canvas); await app.mouse.up();
  await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(1);
  expect((await view(app)).dirty).toBe(false);
  await app.keyboard.press("ControlOrMeta+n");
  await expect(app.getByRole("dialog", { name: "Save changes?" })).toBeVisible();
  expect(await mock.calls("project_new")).toHaveLength(0);
  await app.getByRole("button", { name: "Cancel", exact: true }).click();
  await mock.eval("m => m.releaseCommand('arrangement_edit')");
  await expect.poll(async () => (await state(app)).clips).toEqual(splitAt(before, 2880, 1920, 2400, 5760));
  expect((await state(app)).generation).toBe(before.generation);
  expect((await view(app)).dirty).toBe(true);
  expect(await mock.calls("project_new")).toHaveLength(0);
});

test("opening a project modal cancels capture before its buttons and cannot release a hidden cut", async ({ app, mock }) => {
  const { panel, canvas } = await selected(app);
  const bpm = app.getByRole("textbox", { name: "BPM", exact: true });
  await bpm.fill("121"); await bpm.press("Enter");
  await expect.poll(async () => (await view(app)).bpm).toBe(121);
  expect((await view(app)).dirty).toBe(true);
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  const before = await state(app);
  await chooseSlice(panel); await begin(app, canvas);
  await app.keyboard.press("ControlOrMeta+n");
  await expect(app.getByRole("dialog", { name: "Save changes?" })).toBeVisible();
  await expect.poll(() => captured(canvas)).toBe(false);
  await app.mouse.up();
  await app.getByRole("button", { name: "Cancel", exact: true }).click();
  expect((await state(app)).clips).toEqual(before.clips);
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
  expect(await mock.calls("project_new")).toHaveLength(0);
});

test("true New during an unsubmitted cut retires the original pointer and selection", async ({ app, mock }) => {
  const { panel, canvas } = await selected(app); const before = await state(app);
  await chooseSlice(panel); await begin(app, canvas);
  await app.keyboard.press("ControlOrMeta+n");
  await expect.poll(async () => (await state(app)).generation).toBeGreaterThan(before.generation);
  await app.mouse.up();
  await expect(panel.getByRole("status")).toHaveText("0 clips");
  await expect(panel.getByRole("combobox", { name: "Selected playlist clip" })).toHaveValue("");
  expect((await state(app)).clips).toEqual([]);
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
  expect(await mock.calls("project_new")).toHaveLength(1);
  expect(await mock.calls("transport_play")).toHaveLength(0);
});
