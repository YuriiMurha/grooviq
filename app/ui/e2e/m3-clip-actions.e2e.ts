import type { Page } from "@playwright/test";
import type { ArrangementPage } from "../src/api/arrangement";
import type { ProjectView } from "../src/api/project";
import { expect, project, soundsFor, test } from "./fixture";

const seed = project(["Kick"], { 1: [0, 4, 8, 12] });
seed.nextId = 5;
seed.arrangement = {
  tracks: [{ id: 2, name: "Retained track", color: 0, mute: false }],
  clips: [
    { id: 3, track: 2, name: "Original instance", startTick: 960, muted: false,
      content: { type: "pattern", pattern: 0, offsetTick: 480, lengthTicks: 3840 } },
    { id: 4, track: 2, name: "Linked instance", startTick: 6720, muted: false,
      content: { type: "pattern", pattern: 0, offsetTick: 480, lengthTicks: 3840 } },
  ], audioAssets: [], loopRegion: null,
};
test.use({ seed: { project: seed, files: soundsFor(["Kick"]) } });
function ipc<T>(page: Page, command: string, args: Record<string, unknown> = {}): Promise<T> {
  return page.evaluate(({ command, args }) => (window as unknown as {
    __TAURI_INTERNALS__: { invoke: (command: string, args: Record<string, unknown>) => Promise<unknown> }
  }).__TAURI_INTERNALS__.invoke(command, args), { command, args }) as Promise<T>;
}
const state = (app: Page) => ipc<ArrangementPage>(app, "arrangement_view", { offset: 0, limit: 256 });
const view = (app: Page) => ipc<ProjectView>(app, "project_view");
async function observation(app: Page) {
  const arrangement = await state(app), current = await view(app);
  return { clips: arrangement.clips, tracks: arrangement.tracks, dirty: current.dirty,
    canUndo: current.projectCanUndo, canRedo: current.projectCanRedo };
}
async function selected(app: Page) {
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await app.keyboard.press("F5");
  const panel = app.getByRole("region", { name: "Playlist", exact: true });
  const canvas = panel.getByRole("application", { name: "Playlist clips" });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await expect(panel.getByRole("status")).toHaveText("2 clips");
  await panel.getByRole("combobox", { name: "Selected playlist clip" }).selectOption("3");
  await expect(panel.getByRole("combobox", { name: "Selected playlist clip" })).toHaveValue("3");
  expect((await state(app)).clips.map(clip => clip.id)).toEqual([3, 4]);
  return { panel, canvas };
}

test("mute affects only the selected linked instance and global Undo/Redo restores exact content", async ({ app, mock }) => {
  const { panel } = await selected(app);
  const before = await state(app), source = await mock.eval("m => m.state.project.pattern");
  const allocation = await mock.eval<number>("m => m.state.project.nextId");
  const observe = async () => ({ ...(await observation(app)), nextId: await mock.eval<number>("m => m.state.project.nextId") });
  const trace = [{ stage: "initial", value: await observe() }];
  const calls = (await mock.calls("arrangement_edit")).length;
  await expect(panel.getByRole("button", { name: "Mute selected clip", exact: true })).toBeVisible();
  await panel.getByRole("button", { name: "Mute selected clip", exact: true }).click();
  await expect.poll(async () => (await state(app)).clips).toEqual([{ ...before.clips[0], muted: true }, before.clips[1]]);
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls + 1);
  expect((await state(app)).tracks).toEqual(before.tracks);
  expect(await mock.eval("m => m.state.project.pattern")).toEqual(source);
  expect(await mock.eval<number>("m => m.state.project.nextId")).toBe(allocation);
  trace.push({ stage: "muted", value: await observe() });
  await app.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await state(app)).clips).toEqual(before.clips);
  trace.push({ stage: "undone", value: await observe() });
  await app.keyboard.press("ControlOrMeta+Shift+z");
  await expect.poll(async () => (await state(app)).clips[0].muted).toBe(true);
  trace.push({ stage: "redone", value: await observe() });
  await panel.getByRole("button", { name: "Unmute selected clip", exact: true }).click();
  await expect.poll(async () => (await state(app)).clips).toEqual(before.clips);
  trace.push({ stage: "unmuted", value: await observe() });
  expect(await mock.calls("transport_play")).toHaveLength(0);
  await test.info().attach("m3-clip-actions-trace", { body: JSON.stringify(trace), contentType: "application/json" });
});

test("deleting a selected clip leaves its track, linked copy and source and clears selection only after acceptance", async ({ app, mock }) => {
  const { panel } = await selected(app);
  const before = await state(app), source = await mock.eval("m => m.state.project.pattern");
  const allocation = await mock.eval<number>("m => m.state.project.nextId");
  await panel.getByRole("button", { name: "Delete selected clip", exact: true }).click();
  await expect.poll(async () => (await state(app)).clips).toEqual([before.clips[1]]);
  await expect(panel.getByRole("combobox", { name: "Selected playlist clip" })).toHaveValue("");
  await expect(panel.getByRole("button", { name: "Delete selected clip", exact: true })).toBeDisabled();
  expect((await state(app)).tracks).toEqual(before.tracks);
  expect(await mock.eval("m => m.state.project.pattern")).toEqual(source);
  await app.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await state(app)).clips).toEqual(before.clips);
  await expect(panel.getByRole("combobox", { name: "Selected playlist clip" })).toHaveValue("");
  await app.keyboard.press("ControlOrMeta+Shift+z");
  await expect.poll(async () => (await state(app)).clips).toEqual([before.clips[1]]);
  expect(await mock.eval<number>("m => m.state.project.nextId")).toBe(allocation);
  expect(await mock.calls("transport_play")).toHaveLength(0);
});

for (const key of ["Delete", "Backspace"]) test(`focused ${key} removes once and ignores repeat and modified input`, async ({ app, mock }) => {
  const { canvas } = await selected(app); const before = await state(app);
  const calls = (await mock.calls("arrangement_edit")).length;
  const delivered: unknown[] = [];
  app.on("console", message => {
    const text = message.text();
    if (text.startsWith("M3_CLIP_KEYS:")) delivered.push(JSON.parse(text.slice("M3_CLIP_KEYS:".length)));
  });
  await canvas.evaluate(element => {
    element.addEventListener("keydown", event => {
      const key = event as KeyboardEvent;
      queueMicrotask(() => console.debug("M3_CLIP_KEYS:" + JSON.stringify({ key: key.key, repeat: key.repeat,
        ctrl: key.ctrlKey, alt: key.altKey, meta: key.metaKey, shift: key.shiftKey })));
    });
  });
  await canvas.focus();
  for (const modifier of ["Shift", "Alt", "Control", "Meta"]) await app.keyboard.press(`${modifier}+${key}`);
  expect((await state(app)).clips).toEqual(before.clips);
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls);
  const focusedAfterModifiedKeys = await canvas.evaluate(element => document.activeElement === element);
  // Modified keys retain their browser defaults; establish the focused-action premise again.
  await canvas.focus(); await expect(canvas).toBeFocused();
  const url = app.url();
  await mock.eval("m => m.holdCommand('arrangement_edit')");
  await app.keyboard.down(key);
  await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(calls + 1);
  expect((await state(app)).clips).toEqual(before.clips);
  await app.keyboard.down(key); await app.keyboard.up(key);
  await test.info().attach("actual-key-input", { body: JSON.stringify({ focusedAfterModifiedKeys, delivered, originalURL: url, afterURL: app.url() }), contentType: "application/json" });
  expect(app.url()).toBe(url);
  expect((await state(app)).clips).toEqual(before.clips);
  await mock.eval("m => m.releaseCommand('arrangement_edit')");
  await expect.poll(async () => (await state(app)).clips).toEqual([before.clips[1]]);
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls + 1);
  expect(await mock.calls("transport_play")).toHaveLength(0);
});

test("typing and non-playlist Delete input do not remove the selected clip", async ({ app, mock }) => {
  const { panel } = await selected(app); const before = await state(app);
  const calls = (await mock.calls("arrangement_edit")).length;
  const bpm = app.getByRole("textbox", { name: "BPM", exact: true });
  await bpm.focus(); await bpm.press("End"); await bpm.press("Backspace"); await bpm.press("Escape");
  await panel.getByRole("combobox", { name: "Selected playlist clip" }).focus();
  await app.keyboard.press("Delete");
  expect((await state(app)).clips).toEqual(before.clips);
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls);
});

test("Delete during a captured preview cancels the preview and cannot release a hidden move", async ({ app, mock }) => {
  const { canvas } = await selected(app); const before = await state(app);
  const box = await canvas.boundingBox(); if (!box) throw new Error("No positive actual canvas");
  const calls = (await mock.calls("arrangement_edit")).length;
  await canvas.evaluate(element => element.addEventListener("gotpointercapture", event => {
    element.setAttribute("data-qa-original-pointer", String((event as PointerEvent).pointerId));
  }, { once: true }));
  await app.mouse.move(box.x + 144, box.y + 40); await app.mouse.down();
  await app.mouse.move(box.x + 240, box.y + 40, { steps: 4 });
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls);
  await app.keyboard.press("Delete");
  await expect.poll(() => canvas.evaluate(element => element.hasPointerCapture(Number(element.getAttribute("data-qa-original-pointer"))))).toBe(false);
  await app.mouse.up();
  await expect.poll(async () => (await state(app)).clips).toEqual([before.clips[1]]);
  const sent = (await mock.calls("arrangement_edit")).slice(calls);
  expect(sent).toHaveLength(1);
  expect(JSON.parse(String(sent[0].args.editsJson))).toEqual([{ kind: "deleteClip", clip: 3 }]);
  expect((await state(app)).tracks).toEqual(before.tracks);
});

test("Save waits for an admitted delete and persists the accepted arrangement", async ({ app, mock }) => {
  const { panel } = await selected(app); const before = await state(app);
  const path = "/qa/projects/clip-actions.grooviq";
  await mock.eval("m => m.holdCommand('arrangement_edit')");
  await panel.getByRole("button", { name: "Delete selected clip", exact: true }).click();
  await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(1);
  await expect(panel.getByRole("combobox", { name: "Selected playlist clip" })).toHaveValue("3");
  expect((await state(app)).clips).toEqual(before.clips);
  await mock.eval(`m => m.queueDialog('${path}')`); await app.keyboard.press("ControlOrMeta+s");
  expect(await mock.calls("project_save_as")).toHaveLength(0);
  await mock.eval("m => m.releaseCommand('arrangement_edit')");
  await expect.poll(async () => (await view(app)).dirty).toBe(false);
  const saved = await mock.eval<{ project: { arrangement: { clips: unknown[]; tracks: unknown[] } } }>(`m => m.file('${path}')`);
  expect(saved.project.arrangement.clips).toEqual([before.clips[1]]);
  expect(saved.project.arrangement.tracks).toEqual(before.tracks);
});

test("refused delete keeps clip selection and history and prevents the waiting Save snapshot", async ({ app, mock }) => {
  const { panel } = await selected(app); const before = await state(app), initial = await view(app);
  await mock.eval("m => { m.holdCommand('arrangement_edit'); m.refuseWorkspaceNext('generated delete refusal'); }");
  await panel.getByRole("button", { name: "Delete selected clip", exact: true }).click();
  await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(1);
  await app.keyboard.press("ControlOrMeta+s");
  await mock.eval("m => m.releaseCommand('arrangement_edit')");
  await expect(panel.getByRole("alert")).toContainText("generated delete refusal");
  expect((await state(app)).clips).toEqual(before.clips);
  expect((await state(app)).tracks).toEqual(before.tracks);
  expect(await view(app)).toMatchObject({ generation: initial.generation, dirty: initial.dirty,
    projectCanUndo: initial.projectCanUndo, projectCanRedo: initial.projectCanRedo });
  await expect(panel.getByRole("combobox", { name: "Selected playlist clip" })).toHaveValue("3");
  expect(await mock.calls("project_save_as")).toHaveLength(0);
});

test("clean pending delete prompts before New and Cancel preserves its eventual accepted edit", async ({ app, mock }) => {
  const { panel } = await selected(app); const before = await state(app);
  expect((await view(app)).dirty).toBe(false);
  await mock.eval("m => m.holdCommand('arrangement_edit')");
  await panel.getByRole("button", { name: "Delete selected clip", exact: true }).click();
  await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(1);
  await app.keyboard.press("ControlOrMeta+n");
  await expect(app.getByRole("dialog", { name: "Save changes?" })).toBeVisible();
  expect(await mock.calls("project_new")).toHaveLength(0);
  await app.getByRole("button", { name: "Cancel", exact: true }).click();
  await mock.eval("m => m.releaseCommand('arrangement_edit')");
  await expect.poll(async () => (await state(app)).clips).toEqual([before.clips[1]]);
  expect((await state(app)).generation).toBe(before.generation);
  expect(await mock.calls("project_new")).toHaveLength(0);
  expect((await view(app)).dirty).toBe(true);
});
