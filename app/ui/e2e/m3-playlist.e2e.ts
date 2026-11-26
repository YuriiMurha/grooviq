import type { Page } from "@playwright/test";
import type { ArrangementPage } from "../src/api/arrangement";
import type { ProjectView } from "../src/api/project";
import { expect, project, soundsFor, test, type Mock } from "./fixture";

test.use({ seed: { project: project(["Kick"], { 1: [0, 4, 8, 12] }), files: soundsFor(["Kick"]) } });
const file = "/qa/projects/playlist.grooviq";
function ipc<T>(page: Page, command: string, args: Record<string, unknown> = {}): Promise<T> {
  return page.evaluate(({ command, args }) => (window as unknown as {
    __TAURI_INTERNALS__: { invoke: (command: string, args: Record<string, unknown>) => Promise<unknown> }
  }).__TAURI_INTERNALS__.invoke(command, args), { command, args }) as Promise<T>;
}
const state = (page: Page) => ipc<ArrangementPage>(page, "arrangement_view", { offset: 0, limit: 256 });
async function openPlaylist(page: Page) {
  await expect(page.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await page.keyboard.press("F5");
  const playlist = page.getByRole("region", { name: "Playlist", exact: true });
  await expect(playlist).toBeVisible(); await expect(playlist).toHaveAttribute("aria-busy", "false");
  const canvas = playlist.getByRole("application", { name: "Playlist clips" });
  await expect(canvas).toHaveAttribute("data-renderer", /webgl2|canvas2d/);
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  const box = await canvas.boundingBox(); if (!box || box.width < 300 || box.height < 64) throw new Error("Playlist has no actual input area");
  return { playlist, canvas, box };
}
async function place(page: Page) {
  const view = await openPlaylist(page);
  await page.mouse.click(view.box.x + 48, view.box.y + 32);
  await expect.poll(async () => (await state(page)).total).toBe(1);
  const value = await state(page), clip = value.clips[0];
  expect(clip.startTick).toBe(960);
  expect(clip.content).toEqual({ type: "pattern", pattern: 0, offsetTick: 0, lengthTicks: 3840 });
  expect(value.tracks).toHaveLength(1);
  expect(clip.track).toBe(value.tracks[0].id);
  return { ...view, value, clip };
}
async function observation(app: Page, mock: Mock) {
  const arrangement = await state(app);
  const view = await ipc<ProjectView>(app, "project_view");
  return { clips: arrangement.clips, tracks: arrangement.tracks, nextId: await mock.eval<number>("(m) => m.state.project.nextId"),
    selected: view.pattern.id, playMode: arrangement.playMode, dirty: view.dirty, canUndo: view.projectCanUndo, canRedo: view.projectCanRedo };
}

test("places, moves, resizes and duplicates a linked pattern with actual global Undo/Redo", async ({ app, mock }) => {
  const trace = [{ stage: "initial", value: await observation(app, mock) }];
  const { box, playlist, clip } = await place(app);
  trace.push({ stage: "placed", value: await observation(app, mock) });
  const beforeMove = await mock.calls("arrangement_edit");
  await app.mouse.move(box.x + 144, box.y + 40); await app.mouse.down();
  await app.mouse.move(box.x + 240, box.y + 40, { steps: 6 });
  expect(await mock.calls("arrangement_edit")).toHaveLength(beforeMove.length);
  expect((await state(app)).clips[0].startTick).toBe(960);
  await app.screenshot({ path: test.info().outputPath("playlist-move-preview.png") });
  await app.mouse.up();
  await expect.poll(async () => (await state(app)).clips[0].startTick).toBe(2880);
  expect(await mock.calls("arrangement_edit")).toHaveLength(beforeMove.length + 1);
  trace.push({ stage: "moved", value: await observation(app, mock) });
  await app.mouse.move(box.x + 335, box.y + 40); await app.mouse.down();
  await app.mouse.move(box.x + 383, box.y + 40, { steps: 4 }); await app.mouse.up();
  await expect.poll(async () => (await state(app)).clips[0].content).toEqual({ type: "pattern", pattern: 0, offsetTick: 0, lengthTicks: 4800 });
  trace.push({ stage: "resized", value: await observation(app, mock) });
  await app.mouse.move(box.x + 145, box.y + 40); await app.mouse.down();
  await app.mouse.move(box.x + 193, box.y + 40, { steps: 4 }); await app.mouse.up();
  await expect.poll(async () => (await state(app)).clips[0].content).toEqual({ type: "pattern", pattern: 0, offsetTick: 960, lengthTicks: 3840 });
  expect((await state(app)).clips[0].startTick).toBe(3840);
  trace.push({ stage: "trimmed", value: await observation(app, mock) });
  await playlist.getByRole("button", { name: "Duplicate clip", exact: true }).click();
  await expect.poll(async () => (await state(app)).total).toBe(2);
  const copied = await state(app); expect(copied.clips[1].id).not.toBe(clip.id);
  expect(copied.clips[0].content).toEqual(copied.clips[1].content);
  expect(copied.clips[1].startTick).toBe(7680);
  trace.push({ stage: "copied", value: await observation(app, mock) });
  await app.screenshot({ path: test.info().outputPath("playlist-normal.png") });
  const allocation = await mock.eval<number>("(m) => m.state.project.nextId");
  await app.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await state(app)).total).toBe(1);
  expect(await mock.eval<number>("(m) => m.state.project.nextId")).toBe(allocation);
  trace.push({ stage: "undone", value: await observation(app, mock) });
  await app.keyboard.press("ControlOrMeta+Shift+z");
  await expect.poll(async () => (await state(app)).clips).toEqual(copied.clips);
  trace.push({ stage: "redone", value: await observation(app, mock) });
  expect(await mock.calls("transport_play")).toHaveLength(0);
  await test.info().attach("m3-playlist-ui-trace", { body: JSON.stringify(trace), contentType: "application/json" });
});

for (const cancellation of ["Escape", "capture loss", "F5"] as const) test(`a real held playlist move cancels on ${cancellation} without a native edit`, async ({ app, mock }) => {
  const { box, canvas } = await place(app);
  const before = await state(app), calls = await mock.calls("arrangement_edit");
  await canvas.evaluate(element => {
    element.addEventListener("gotpointercapture", event => { element.setAttribute("data-qa-held-pointer", String((event as PointerEvent).pointerId)); }, { once: true });
  });
  await app.mouse.move(box.x + 144, box.y + 40); await app.mouse.down();
  await app.mouse.move(box.x + 240, box.y + 40, { steps: 4 });
  if (cancellation === "capture loss") await canvas.evaluate(element => {
    const id = Number(element.getAttribute("data-qa-held-pointer"));
    if (!Number.isSafeInteger(id) || !element.hasPointerCapture(id)) throw new Error("No actual original pointer capture");
    element.releasePointerCapture(id);
  });
  else await app.keyboard.press(cancellation);
  await app.mouse.up();
  expect((await state(app)).clips).toEqual(before.clips);
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls.length);
  if (cancellation === "F5") await expect(canvas).toHaveCount(0);
});

test("Save waits for a held accepted clip edit and snapshots the accepted arrangement", async ({ app, mock }) => {
  const { box, canvas } = await openPlaylist(app);
  await mock.eval("(m) => m.holdCommand('arrangement_edit')");
  await app.mouse.click(box.x + 48, box.y + 32);
  await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(1);
  await mock.eval(`(m) => m.queueDialog('${file}')`);
  await app.keyboard.press("ControlOrMeta+s");
  expect(await mock.calls("project_save")).toHaveLength(0);
  await mock.eval("(m) => m.releaseCommand('arrangement_edit')");
  await expect.poll(async () => (await state(app)).total).toBe(1);
  await expect.poll(async () => (await mock.view()).dirty).toBe(false);
  const generation = (await state(app)).generation;
  await mock.eval(`(m) => m.queueDialog('${file}')`);
  await canvas.focus(); await app.keyboard.press("ControlOrMeta+o");
  await expect.poll(async () => (await state(app)).generation).toBe(generation + 1);
  expect((await state(app)).clips).toHaveLength(1);
  expect((await state(app)).clips[0].startTick).toBe(960);
  expect(await mock.calls("transport_play")).toHaveLength(0);
});

test("refused clip admission preserves project/history and blocks its Save snapshot", async ({ app, mock }) => {
  const { box } = await openPlaylist(app); const initial = await state(app);
  await mock.eval("(m) => m.holdCommand('arrangement_edit')");
  await mock.eval("(m) => m.refuseWorkspaceNext('generated clip admission refusal')");
  await app.mouse.click(box.x + 48, box.y + 32);
  await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(1);
  await app.keyboard.press("ControlOrMeta+s");
  await mock.eval("(m) => m.releaseCommand('arrangement_edit')");
  await expect(app.getByRole("region", { name: "Playlist", exact: true }).getByRole("alert")).toContainText("generated clip admission refusal");
  expect((await state(app)).clips).toEqual(initial.clips);
  expect((await state(app)).tracks).toEqual(initial.tracks);
  expect(await mock.calls("project_save")).toHaveLength(0);
});

test("a clean project with a pending clip prompts before New and Cancel retains its admitted edit", async ({ app, mock }) => {
  const { box } = await openPlaylist(app);
  expect((await mock.view()).dirty).toBe(false);
  const generation = (await state(app)).generation;
  await mock.eval("(m) => m.holdCommand('arrangement_edit')");
  await app.mouse.click(box.x + 48, box.y + 32);
  await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(1);
  expect((await mock.view()).dirty).toBe(false);
  await app.keyboard.press("ControlOrMeta+n");
  await expect(app.getByRole("button", { name: "Cancel", exact: true })).toBeVisible();
  expect(await mock.calls("project_new")).toHaveLength(0);
  await app.getByRole("button", { name: "Cancel", exact: true }).click();
  await mock.eval("(m) => m.releaseCommand('arrangement_edit')");
  await expect.poll(async () => (await state(app)).total).toBe(1);
  expect((await state(app)).generation).toBe(generation);
  expect((await mock.view()).dirty).toBe(true);
  expect(await mock.calls("project_new")).toHaveLength(0);
});

test("project replacement retires a held drag and reused clip identities cannot inherit it", async ({ app, mock }) => {
  const { box, canvas } = await place(app);
  const generation = (await state(app)).generation;
  const edits = await mock.calls("arrangement_edit");
  await canvas.evaluate(element => {
    element.addEventListener("gotpointercapture", event => element.setAttribute("data-qa-held-pointer", String((event as PointerEvent).pointerId)), { once: true });
  });
  await app.mouse.move(box.x + 144, box.y + 40); await app.mouse.down();
  await app.mouse.move(box.x + 240, box.y + 40, { steps: 4 });
  await app.keyboard.press("ControlOrMeta+n");
  await expect(app.getByRole("dialog", { name: "Save changes?" })).toBeVisible();
  await expect.poll(() => canvas.evaluate(element => element.hasPointerCapture(Number(element.getAttribute("data-qa-held-pointer"))))).toBe(false);
  await app.mouse.up();
  expect(await mock.calls("arrangement_edit")).toHaveLength(edits.length);
  expect((await state(app)).clips[0].startTick).toBe(960);
  await app.getByRole("button", { name: "Don't save", exact: true }).click();
  await expect.poll(async () => (await state(app)).generation).toBe(generation + 1);
  await app.mouse.up();
  expect((await state(app)).clips).toEqual([]);
  expect(await mock.calls("arrangement_edit")).toHaveLength(edits.length);
  await expect(canvas).toBeVisible();
  expect(await mock.calls("transport_play")).toHaveLength(0);
});

test("F5 playlist preserves the same inner workspace, channel and requested split", async ({ app }) => {
  await app.setViewportSize({ width: 900, height: 560 });
  await app.keyboard.press("F7");
  const workspace = app.locator(".workspace");
  await workspace.evaluate(element => { element.setAttribute("data-qa-playlist-stable", "original"); });
  const splitter = app.getByRole("separator", { name: "Resize the channel rack and the piano roll" });
  await splitter.focus(); await app.keyboard.press("ArrowDown");
  const requested = await workspace.evaluate(element => (element as HTMLElement).style.getPropertyValue("--split"));
  await app.keyboard.press("F5");
  await expect(app.getByRole("application", { name: "Playlist clips" })).toBeVisible();
  const viewport = app.locator(".workspace-viewport");
  const body = app.locator(".panel-roll .roll-body"), notes = app.locator(".panel-roll .roll-canvas");
  await expect.poll(async () => (await body.boundingBox())?.height ?? 0).toBeGreaterThanOrEqual(112);
  await expect.poll(async () => (await notes.boundingBox())?.height ?? 0).toBeGreaterThanOrEqual(112);
  const scrollable = await viewport.evaluate(element => element.scrollHeight > element.clientHeight);
  const area = await viewport.boundingBox(); if (!area) throw new Error("No actual contained viewport");
  await app.mouse.move(area.x + area.width - 2, area.y + area.height / 2);
  await app.mouse.wheel(0, 2000);
  await expect.poll(async () => notes.evaluate(element => {
    const area = element.closest(".workspace-viewport"); if (!area) throw new Error("No contained workspace viewport");
    const inner = element.getBoundingClientRect(), outer = area.getBoundingClientRect();
    return inner.top >= outer.top - 1 && inner.bottom <= outer.bottom + 1;
  })).toBe(true);
  if (scrollable) expect(await viewport.evaluate(element => element.scrollTop)).toBeGreaterThan(0);
  expect(await app.evaluate(() => window.scrollY)).toBe(0);
  await app.screenshot({ path: test.info().outputPath("playlist-900x560.png") });
  await app.keyboard.press("F5");
  expect(await workspace.getAttribute("data-qa-playlist-stable")).toBe("original");
  expect(await workspace.evaluate(element => (element as HTMLElement).style.getPropertyValue("--split"))).toBe(requested);
  await expect(app.getByRole("region", { name: "Piano roll", exact: true })).toBeVisible();
});

test.describe("source phase at repeated pattern boundaries", () => {
  const scene = project(["Kick"], { 1: [0, 4, 8, 12] });
  const source = scene.pattern;
  const other = { ...source, id: 3, name: "Short selected pattern", steps: 4, notes: [] };
  scene.patterns = [source, other]; scene.pattern = other; scene.nextId = 6;
  scene.arrangement = { tracks: [{ id: 4, name: "Original track", color: 0, mute: false }], clips: [
    { id: 5, track: 4, name: "Repeated source", startTick: 960, muted: false,
      content: { type: "pattern", pattern: source.id, offsetTick: 0, lengthTicks: 11520 } },
  ], audioAssets: [], loopRegion: null };
  test.use({ seed: { project: scene, files: soundsFor(["Kick"]) } });
  for (const [shift, offset] of [[2880, 2880], [3840, 0], [4800, 960]]) {
    test(`left trim ${shift} uses the unselected source cycle and preserves global history`, async ({ app, mock }) => {
      const { canvas, box, playlist } = await openPlaylist(app);
      const before = await state(app), allocation = await mock.eval<number>("m => m.state.project.nextId");
      expect((await ipc<ProjectView>(app, "project_view")).pattern).toMatchObject({ id: 3, steps: 4 });
      expect(before.clips[0].content).toEqual({ type: "pattern", pattern: source.id, offsetTick: 0, lengthTicks: 11520 });
      await app.mouse.click(box.x + 144, box.y + 40);
      const calls = (await mock.calls("arrangement_edit")).length;
      await app.mouse.move(box.x + 49, box.y + 40); await app.mouse.down();
      await app.mouse.move(box.x + 49 + shift / 20, box.y + 40, { steps: 6 });
      expect(await mock.calls("arrangement_edit")).toHaveLength(calls);
      await app.mouse.up();
      const expected = { ...before.clips[0], startTick: 960 + shift,
        content: { type: "pattern", pattern: source.id, offsetTick: offset, lengthTicks: 11520 - shift } };
      await expect.poll(async () => (await state(app)).clips[0]).toEqual(expected);
      expect(await mock.calls("arrangement_edit")).toHaveLength(calls + 1);
      expect((await ipc<ProjectView>(app, "project_view")).projectCanUndo).toBe(true);
      await app.keyboard.press("ControlOrMeta+z");
      await expect.poll(async () => (await state(app)).clips).toEqual(before.clips);
      await app.keyboard.press("ControlOrMeta+Shift+z");
      await expect.poll(async () => (await state(app)).clips).toEqual([expected]);
      expect(await mock.eval<number>("m => m.state.project.nextId")).toBe(allocation);
      expect((await ipc<ProjectView>(app, "project_view")).pattern.id).toBe(3);
      await expect(playlist.getByRole("button", { name: /^Selected playlist clip:/ })).toHaveAccessibleName(`Selected playlist clip: Repeated source · Track 1 · ${expected.startTick} ticks`);
      await expect(canvas).toHaveAttribute("aria-disabled", "false");
      expect(await mock.calls("transport_play")).toHaveLength(0);
    });
  }
});

for (const flow of ["Open", "New", "Restore"] as const) test.describe(`failed ${flow} playlist recovery`, () => {
  const scene = project(["Kick"], { 1: [0, 4, 8, 12] });
  scene.nextId = 4;
  scene.arrangement = { tracks: [{ id: 2, name: "Original track", color: 0, mute: false }], clips: [
    { id: 3, track: 2, name: "Retained clip", startTick: 960, muted: false,
      content: { type: "pattern", pattern: scene.pattern.id, offsetTick: 0, lengthTicks: 3840 } },
  ], audioAssets: [], loopRegion: null };
  test.use({ seed: { project: scene, files: soundsFor(["Kick"]),
    recovery: flow === "Restore" ? { savedAt: 1000, original: file, project: scene } : null } });
  test("delayed refusal restores the original editable playlist without Refresh", async ({ app, mock }) => {
    const command = flow === "Open" ? "project_open" : flow === "New" ? "project_new" : "recovery_restore";
    if (flow !== "Restore") {
      const { box } = await openPlaylist(app);
      await app.mouse.click(box.x + 144, box.y + 40);
    } else {
      await expect(app.getByRole("dialog", { name: "Restore your project?" })).toBeVisible();
      await expect.poll(async () => (await mock.calls("arrangement_view")).length).toBeGreaterThan(0);
    }
    const before = await state(app), view = await ipc<ProjectView>(app, "project_view");
    expect(before.clips).toHaveLength(1); expect(view.dirty).toBe(false);
    const calls = (await mock.calls(command)).length;
    await mock.eval(`m => { m.holdCommand('${command}'); m.refuseNext('${command}', 'generated delayed ${flow} refusal'); }`);
    if (flow === "Open") await mock.eval("m => m.queueDialog('/qa/missing.grooviq')");
    if (flow === "Restore") await app.getByRole("button", { name: "Restore", exact: true }).click();
    else await app.keyboard.press(flow === "Open" ? "ControlOrMeta+o" : "ControlOrMeta+n");
    await expect.poll(async () => (await mock.calls(command)).length).toBe(calls + 1);
    // Flush the pre-command settled() adoption/effects while native replacement is held.
    await app.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
    expect((await state(app)).clips).toEqual(before.clips);
    await mock.eval(`m => m.releaseCommand('${command}')`);
    await expect(app.getByRole("status", { name: "Messages" })).toContainText(`generated delayed ${flow} refusal`);
    expect(await state(app)).toMatchObject({ generation: before.generation, revision: before.revision, clips: before.clips, tracks: before.tracks });
    expect(await ipc<ProjectView>(app, "project_view")).toMatchObject({ generation: view.generation, dirty: false, projectCanUndo: view.projectCanUndo, projectCanRedo: view.projectCanRedo });
    const ui = flow === "Restore" ? await openPlaylist(app) : {
      playlist: app.getByRole("region", { name: "Playlist", exact: true }),
      canvas: app.getByRole("application", { name: "Playlist clips" }),
    };
    await expect(ui.playlist.getByRole("status")).toHaveText("1 clips");
    await expect(ui.canvas).toHaveAttribute("aria-disabled", "false");
    if (flow !== "Restore") await expect(ui.playlist.getByRole("button", { name: /^Selected playlist clip:/ })).toHaveAccessibleName("Selected playlist clip: Retained clip · Track 1 · 960 ticks");
    const box = await ui.canvas.boundingBox(); if (!box) throw new Error("No retained actual canvas");
    const edits = (await mock.calls("arrangement_edit")).length;
    await app.mouse.move(box.x + 144, box.y + 40); await app.mouse.down();
    await app.mouse.move(box.x + 192, box.y + 40, { steps: 4 }); await app.mouse.up();
    await expect.poll(async () => (await state(app)).clips[0].startTick).toBe(1920);
    expect(await mock.calls("arrangement_edit")).toHaveLength(edits + 1);
    expect((await ipc<ProjectView>(app, "project_view")).projectCanUndo).toBe(true);
    expect(await mock.calls("transport_play")).toHaveLength(0);
  });
});

// The fixture's clip/history implementation must be independently compared to production
// Session traces before these browser outcomes are used as native integration evidence.
