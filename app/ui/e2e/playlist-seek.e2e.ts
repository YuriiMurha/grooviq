import { readFileSync } from "node:fs";
import type { ArrangementStamp } from "../src/api/arrangement";
import { test, expect, project, workspaceReady, SYNTH_PARAMS, SYNTH_PRESETS } from "./fixture";
import { installMockTauri, type MockSeed } from "./mockTauri";

const song = project([], {}, { playMode: "song", arrangement: {
  tracks: [{ id: 1, name: "Track", color: 0, mute: false }], audioAssets: [], loopRegion: null,
  clips: [{ id: 2, track: 1, name: "Pattern", startTick: 0, muted: false, content: { type: "pattern", pattern: 0, offsetTick: 0, lengthTicks: 15360 } }],
}, nextId: 3 });
test.use({ seed: { project: song, files: { "/qa/projects/Replacement.grooviq": { kind: "project", project: song } } } });

test("playlist loads after discarding recovery shown before the initial project view", async ({ page }) => {
  const { sampler } = JSON.parse(readFileSync(new URL("../../../tests/fixtures/device-params.json", import.meta.url), "utf8")) as { sampler: MockSeed["samplerParams"] };
  await page.addInitScript(installMockTauri, {
    samplerParams: sampler, synthParams: SYNTH_PARAMS, synthPresets: SYNTH_PRESETS,
    project: song, holdProjectView: true, recovery: { savedAt: 1_794_860_000, original: null, project: song },
  });
  await page.goto("/");
  const recovery = page.getByRole("dialog", { name: "Restore your project?" });
  await expect(recovery).toBeVisible();
  await expect(page.getByRole("region", { name: "Channel rack" })).toHaveCount(0);
  await page.evaluate("window.__grooviqMock.releaseCommand('project_view')");
  await workspaceReady(page);
  await expect(recovery).toBeVisible();
  await recovery.getByRole("button", { name: "Discard" }).click();
  await page.keyboard.press("F5");
  await expect(page.getByRole("region", { name: "Playlist", exact: true }).getByRole("status")).toHaveText("1 clips");
});

for (const boundary of ["disconnect", "replacement"] as const) test(`relative seek waits for a live frame after ${boundary}`, async ({ app, mock }) => {
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await app.keyboard.press("Space");
  await expect.poll(() => mock.eval("m => m.state.transport.playing")).toBe(true);
  await app.keyboard.press("F5");
  const ruler = app.getByRole("group", { name: "Playlist seek ruler" });
  await expect(ruler).toHaveAttribute("aria-disabled", "false");
  const seekTicks = async () => (await mock.calls("arrangement_seek")).map(call => call.args.tick);
  await mock.eval("m => { m.pauseLive(true); m.hold(1700); m.injectLive(true); }");
  await ruler.press("ArrowRight");
  await expect.poll(seekTicks).toEqual([2660]);
  await mock.eval("m => { m.hold(1700); m.injectLive(true); }");

  if (boundary === "disconnect") {
    expect(await mock.eval("m => m.disconnectLive()")).toBe(1);
    const box = await ruler.boundingBox();
    if (!box) throw new Error("No ruler");
    await app.mouse.click(box.x + 480, box.y + 10);
    await expect.poll(() => mock.eval("m => m.ticks()")).toBe(9600);
  } else {
    await mock.eval("m => m.queueDialog('/qa/projects/Replacement.grooviq')");
    await app.keyboard.press("ControlOrMeta+o");
    await expect(app).toHaveTitle("Replacement — Grooviq");
    await expect(ruler).toHaveAttribute("aria-disabled", "false");
  }

  await ruler.press("ArrowRight");
  await ruler.press("Home");
  const absoluteSeeks = boundary === "disconnect" ? [2660, 9600, 0] : [2660, 0];
  await expect.poll(seekTicks).toEqual(absoluteSeeks);
  if (boundary === "replacement") {
    await app.keyboard.press("Space");
    await expect.poll(() => mock.eval("m => m.state.transport.playing")).toBe(true);
  }
  await expect.poll(() => mock.eval("m => { m.hold(3200); return m.injectLive(true); }")).toBe(1);
  await ruler.press("ArrowRight");
  await expect.poll(seekTicks).toEqual([...absoluteSeeks, 4160]);
});

test("compact seek row moves the playhead while the loop row only edits loops", async ({ app, mock }, info) => {
  await app.setViewportSize({ width: 900, height: 560 });
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await app.keyboard.press("Space");
  await expect.poll(() => mock.eval("m => m.state.transport.playing")).toBe(true);
  await mock.eval("m => m.hold(0)");
  await app.keyboard.press("F5");
  const seekRuler = app.getByRole("group", { name: "Playlist seek ruler" });
  const loopRuler = app.getByRole("group", { name: "Playlist loop ruler" });
  await expect(seekRuler).toHaveAttribute("aria-disabled", "false");
  await expect(loopRuler).toHaveAttribute("aria-disabled", "false");
  const seekBox = await seekRuler.boundingBox(), loopBox = await loopRuler.boundingBox();
  if (!seekBox || !loopBox) throw new Error("Both ruler rows must be visible");
  expect(seekBox.y).toBeGreaterThanOrEqual(0);
  expect(seekBox.y + seekBox.height).toBeLessThanOrEqual(loopBox.y + 0.5);
  expect(loopBox.y + loopBox.height).toBeLessThan(560);
  const seekY = seekBox.y + seekBox.height / 2, loopY = loopBox.y + loopBox.height / 2;
  const playhead = seekRuler.locator(".playlist-seek-playhead");
  const playheadX = async () => (await playhead.boundingBox())?.x;
  const seekTicks = async () => (await mock.calls("arrangement_seek")).map(call => call.args.tick);
  const before = await mock.view();
  const edits = (await mock.calls("arrangement_edit")).length;
  const reads = (await mock.calls("project_view")).length;
  await app.mouse.click(loopBox.x + 96, loopY);
  expect(await seekTicks()).toEqual([]);
  expect(await mock.calls("arrangement_edit")).toHaveLength(edits);
  await app.mouse.click(seekBox.x + 48, seekY);
  await expect.poll(seekTicks).toEqual([960]);
  await expect.poll(playheadX).toBeCloseTo(seekBox.x + 48, 0);
  await app.mouse.move(seekBox.x + 48, seekY); await app.mouse.down();
  await app.mouse.move(seekBox.x + 240, seekY, { steps: 4 });
  await expect.poll(playheadX).toBeCloseTo(seekBox.x + 240, 0);
  expect(await seekTicks()).toEqual([960]);
  await app.mouse.up();
  await expect.poll(seekTicks).toEqual([960, 4800]);
  await expect.poll(() => mock.eval("m => m.ticks()")).toBe(4800);
  expect(await mock.calls("arrangement_edit")).toHaveLength(edits);
  expect(await mock.calls("project_view")).toHaveLength(reads);
  expect(await mock.view()).toMatchObject({ dirty: before.dirty, canUndo: before.canUndo, canRedo: before.canRedo });
  await app.mouse.move(loopBox.x + 48, loopY); await app.mouse.down();
  await app.mouse.move(loopBox.x + 52, loopY); await app.mouse.move(loopBox.x + 48, loopY); await app.mouse.up();
  await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(edits + 1);
  expect(await seekTicks()).toEqual([960, 4800]);
  await expect(loopRuler.locator(".playlist-loop")).toBeVisible();
  await expect.poll(playheadX).toBeCloseTo(seekBox.x + 240, 0);
  await seekRuler.focus();
  await info.attach("playlist-seek-900x560", { body: await app.screenshot({ scale: "css" }), contentType: "image/png" });
});

test("keyboard uses live ticks; native failures do not dirty or create history", async ({ app, mock }) => {
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  const stamp = () => mock.eval<ArrangementStamp>("m => { const { generation, revision } = m.workspaceSnapshot(); return { generation, revision }; }");
  await app.keyboard.press("Space");
  await mock.eval("m => m.hold(1700)");
  await app.keyboard.press("F5");
  const ruler = app.getByRole("group", { name: "Playlist seek ruler" });
  await expect(ruler).toHaveAttribute("aria-disabled", "false");
  await expect.poll(() => mock.eval("m => m.ticks()")).toBe(1700);
  const seekStamp = await stamp();
  const seekArgs = async () => (await mock.calls("arrangement_seek")).map(call => call.args);
  await ruler.focus(); await app.keyboard.press("ArrowRight");
  await expect.poll(async () => (await seekArgs())[0]?.tick).toBe(2660);
  await app.keyboard.press("Home");
  await expect.poll(seekArgs).toEqual([{ ...seekStamp, tick: 2660 }, { ...seekStamp, tick: 0 }]);
  const before = await mock.view();
  await mock.eval("m => m.refuseWorkspaceNext('seek admission refused')");
  await app.keyboard.press("ArrowRight");
  await expect(app.getByRole("region", { name: "Playlist", exact: true }).getByRole("alert")).toContainText("seek admission refused");
  await mock.eval("m => { m.state.project.playMode = 'pattern'; }");
  await app.keyboard.press("Home");
  await expect(app.getByRole("region", { name: "Playlist", exact: true }).getByRole("alert")).toContainText("Song mode");
  expect(await mock.view()).toMatchObject({ dirty: before.dirty, canUndo: before.canUndo, canRedo: before.canRedo });
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
});

test("playlist seek uses the current arrangement revision after a playback mode switch", async ({ app, mock }) => {
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  const nativeMode = app.getByRole("combobox", { name: "Playback mode", exact: true });
  const pickerMode = app.getByRole("button", { name: /^Playback mode:/ });
  const mode = nativeMode.or(pickerMode);
  const changeMode = async (name: "Pattern" | "Song") => {
    if (await nativeMode.count()) {
      await nativeMode.selectOption({ label: name });
    } else {
      await pickerMode.click();
      await app.getByRole("listbox", { name: "Playback mode", exact: true }).getByRole("option", { name, exact: true }).click();
    }
  };
  await expect(mode).toBeEnabled();
  await app.keyboard.press("F5");
  const ruler = app.getByRole("group", { name: "Playlist seek ruler" });
  await expect(ruler).toHaveAttribute("aria-disabled", "false");

  const stamp = () => mock.eval<ArrangementStamp>("m => { const { generation, revision } = m.workspaceSnapshot(); return { generation, revision }; }");
  const initial = await stamp();
  await changeMode("Pattern");
  await expect(mode).toBeEnabled();
  await expect.poll(stamp).toEqual({ ...initial, revision: initial.revision + 1 });

  const views = (await mock.calls("project_view")).length;
  await mock.eval("m => m.holdCommand('project_view')");
  try {
    await changeMode("Song");
    await expect(mode).toBeDisabled();
    await expect.poll(async () => (await mock.calls("project_view")).length).toBeGreaterThan(views);
    // Let project-view effects run in the input event that releases the held native readback.
    await mock.eval("m => window.addEventListener('click', () => m.releaseCommand('project_view'), { once: true })");
    await app.getByRole("heading", { name: "Patterns", exact: true }).click();
  } finally {
    await mock.eval("m => m.releaseCommand('project_view')");
  }
  await expect(mode).toBeEnabled();
  const current = { ...initial, revision: initial.revision + 2 };
  await expect.poll(stamp).toEqual(current);
  await expect(ruler).toHaveAttribute("aria-disabled", "false");
  await ruler.press("Home");
  await expect.poll(async () => (await mock.calls("arrangement_seek")).map((call) => call.args))
    .toEqual([{ ...current, tick: 0 }]);
});

for (const reason of ["capture loss", "replacement"] as const) test(`held click cancels on ${reason}`, async ({ app, mock }) => {
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await app.keyboard.press("F5");
  const ruler = app.getByRole("group", { name: "Playlist seek ruler" });
  await expect(ruler).toHaveAttribute("aria-disabled", "false");
  await ruler.evaluate(element => element.addEventListener("gotpointercapture", event => element.setAttribute("data-pointer", String((event as PointerEvent).pointerId)), { once: true }));
  const box = await ruler.boundingBox(); if (!box) throw new Error("No ruler");
  await app.mouse.move(box.x + 48, box.y + 10); await app.mouse.down();
  await app.mouse.move(box.x + 49, box.y + 10);
  await expect.poll(() => ruler.evaluate(element => element.hasPointerCapture(Number(element.getAttribute("data-pointer"))))).toBe(true);
  if (reason === "capture loss") await ruler.evaluate(element => element.releasePointerCapture(Number(element.getAttribute("data-pointer"))));
  else await app.keyboard.press("ControlOrMeta+n");
  await app.mouse.up();
  expect(await mock.calls("arrangement_seek")).toHaveLength(0);
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
});
