import type { Page } from "@playwright/test";
import type { ArrangementPage } from "../src/api/arrangement";
import { expect, project, soundsFor, test } from "./fixture";
const groupStartTicks = [960, 5760];
test.use({ seed: { project: project(["Kick"], { 1: [0, 4, 8, 12] }, { nextId: 5, arrangement: {
  tracks: [{ id: 2, name: "Track 1", color: 0, mute: false }],
  clips: groupStartTicks.map((startTick, index) => ({
    id: 3 + index, track: 2, name: "Pattern 1", startTick, muted: false,
    content: { type: "pattern", pattern: 0, offsetTick: 0, lengthTicks: 3840 },
  })),
  audioAssets: [], loopRegion: null,
} }), files: soundsFor(["Kick"]) } });
const state = (page: Page): Promise<ArrangementPage> => page.evaluate(() => (window as unknown as { __TAURI_INTERNALS__: { invoke: (command: string, args: unknown) => Promise<ArrangementPage> } }).__TAURI_INTERNALS__.invoke("arrangement_view", { offset: 0, limit: 256 }));
async function setup(page: Page) {
  await expect(page.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await page.keyboard.press("F5");
  const playlist = page.getByRole("region", { name: "Playlist", exact: true });
  const canvas = playlist.getByRole("application", { name: "Playlist clips" });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await expect(playlist.getByRole("status")).toHaveText("2 clips");
  expect((await state(page)).clips.map(clip => clip.startTick)).toEqual(groupStartTicks);
  const box = await canvas.boundingBox(); if (!box) throw new Error("Missing playlist canvas");
  await playlist.getByRole("button", { name: "Select", exact: true }).click();
  await page.mouse.move(box.x + 10, box.y + 8); await page.mouse.down();
  await page.mouse.move(box.x + 490, box.y + 62, { steps: 6 }); await page.mouse.up();
  await expect(playlist.getByText("2 selected", { exact: true })).toBeVisible();
  return { playlist, canvas, box };
}

test.describe("existing clip order", () => {
  const content = { type: "pattern" as const, pattern: 0, offsetTick: 0, lengthTicks: 3840 };
  test.use({ seed: { project: project(["Kick"], {}, { nextId: 5, arrangement: {
    tracks: [{ id: 2, name: "Track 1", color: 0, mute: false }],
    clips: [
      { id: 3, track: 2, name: "Lower clip", startTick: 960, muted: false, content },
      { id: 4, track: 2, name: "Upper clip", startTick: 2880, muted: false, content },
    ],
    audioAssets: [], loopRegion: null,
  } }), files: soundsFor(["Kick"]) } });

  test("reverse selection order preserves the overlapping copy stack", async ({ app, mock }) => {
    await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
    await app.keyboard.press("F5");
    const playlist = app.getByRole("region", { name: "Playlist", exact: true });
    const canvas = playlist.getByRole("application", { name: "Playlist clips" });
    await expect(canvas).toHaveAttribute("aria-disabled", "false");
    const box = await canvas.boundingBox();
    if (!box) throw new Error("Missing playlist canvas");
    await playlist.getByRole("button", { name: "Select", exact: true }).click();
    await app.mouse.click(box.x + 275, box.y + 40);
    await app.keyboard.down("Shift");
    await app.mouse.click(box.x + 100, box.y + 40);
    await app.keyboard.up("Shift");
    await expect(playlist.getByText("2 selected", { exact: true })).toBeVisible();
    await app.keyboard.down("Alt");
    await app.mouse.move(box.x + 175, box.y + 40);
    await app.mouse.down();
    await app.mouse.move(box.x + 271, box.y + 40);
    await app.mouse.up();
    await app.keyboard.up("Alt");
    await expect.poll(async () => (await state(app)).clips.slice(2).map(clip => clip.name)).toEqual(["Lower clip", "Upper clip"]);
    expect(await mock.calls("arrangement_edit")).toHaveLength(1);
    await expect(canvas).toHaveAttribute("aria-disabled", "false");
    await app.mouse.click(box.x + 300, box.y + 40);
    await expect(playlist.getByRole("button", { name: /^Selected playlist clip:/ })).toHaveAccessibleName("Selected playlist clip: Upper clip · Track 1 · 4800 ticks");
  });
});

test("a loaded automation clip joins the pattern group preview and release batch", async ({ app, mock }) => {
  const { box, playlist, canvas } = await setup(app);
  await mock.eval(`m => { m.state.project.nextId = 102; m.state.project.arrangement.clips.push({
    id: 100, track: m.state.project.arrangement.tracks[0].id, name: "Automation",
    startTick: 10560, muted: false, content: { type: "automation",
      target: { type: "channelVolume", channel: 1 }, priority: 101,
      offsetTick: 0, lengthTicks: 1920, points: [] }
  }); }`);
  await playlist.getByRole("button", { name: "Refresh playlist" }).click();
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await expect(playlist.getByRole("status")).toHaveText("3 clips");
  const original = (await state(app)).clips.find(clip => clip.id === 100);
  const label = playlist.locator(".playlist-labels").getByText("Automation · automation", { exact: true });
  await expect(label).toHaveCSS("left", "534px");
  await app.mouse.click(box.x + 560, box.y + 40);
  await expect(playlist.getByRole("button", { name: /^Selected playlist clip:/ })).toHaveAccessibleName("Selected playlist clip: Automation · Track 1 · 10560 ticks");
  await app.keyboard.down("Shift");
  await app.mouse.click(box.x + 140, box.y + 40);
  await app.mouse.click(box.x + 380, box.y + 40);
  await app.keyboard.up("Shift");
  await expect(playlist.getByText("3 selected", { exact: true })).toBeVisible();
  const before = await state(app), calls = await mock.calls("arrangement_edit");
  await app.mouse.move(box.x + 140, box.y + 40);
  await app.mouse.down();
  await app.mouse.move(box.x + 188, box.y + 40);
  await expect(label).toHaveCSS("left", "582px");
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls.length);
  expect((await state(app)).clips).toEqual(before.clips);
  await app.mouse.up();
  await expect.poll(async () => (await state(app)).clips.map(clip => clip.startTick)).toEqual([1920, 6720, 11520]);
  const sent = (await mock.calls("arrangement_edit")).slice(calls.length);
  expect(sent).toHaveLength(1);
  expect(JSON.parse(String(sent[0].args.editsJson))).toEqual([
    { kind: "move", clip: 3, track: 2, startTick: 1920 },
    { kind: "move", clip: 4, track: 2, startTick: 6720 },
    { kind: "move", clip: 100, track: 2, startTick: 11520 },
  ]);
  expect((await state(app)).clips.find(clip => clip.id === 100)).toEqual({ ...original, startTick: 11520 });
});
test("marquee group move and Alt-copy send one release batch and Undo restores both", async ({ app, mock }) => {
  const { box, playlist } = await setup(app);
  const before = await state(app), calls = await mock.calls("arrangement_edit");
  await app.mouse.move(box.x + 140, box.y + 40); await app.mouse.down();
  await app.mouse.move(box.x + 188, box.y + 40, { steps: 5 });
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls.length);
  expect((await state(app)).clips).toEqual(before.clips);
  await app.mouse.up();
  await expect.poll(async () => (await state(app)).clips.map(c => c.startTick)).toEqual(before.clips.map(c => c.startTick + 960));
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls.length + 1);
  await app.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await state(app)).clips).toEqual(before.clips);
  await app.keyboard.down("Alt"); await app.mouse.move(box.x + 140, box.y + 40); await app.mouse.down();
  await app.mouse.move(box.x + 188, box.y + 40); await app.mouse.up(); await app.keyboard.up("Alt");
  await expect.poll(async () => (await state(app)).total).toBe(4);
  await expect(playlist.getByText("2 selected", { exact: true })).toBeVisible();
  await app.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await state(app)).clips).toEqual(before.clips);
});
for (const cancellation of ["Escape", "capture"] as const) test(`held group cancels on ${cancellation}`, async ({ app, mock }) => {
  const { box, canvas } = await setup(app);
  const before = await state(app), calls = await mock.calls("arrangement_edit");
  await canvas.evaluate(element => element.addEventListener("gotpointercapture", event => element.setAttribute("data-pointer", String((event as PointerEvent).pointerId)), { once: true }));
  await app.mouse.move(box.x + 140, box.y + 40); await app.mouse.down(); await app.mouse.move(box.x + 188, box.y + 40);
  if (cancellation === "Escape") await app.keyboard.press("Escape");
  else await canvas.evaluate(element => element.releasePointerCapture(Number(element.getAttribute("data-pointer"))));
  await app.mouse.up();
  expect((await state(app)).clips).toEqual(before.clips);
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls.length);
});
test("modifier toggles and sub-threshold clicks do not edit; Make unique stays single-only", async ({ app, mock }) => {
  const { box, playlist } = await setup(app), calls = await mock.calls("arrangement_edit");
  await expect(playlist.getByRole("button", { name: "Make selected clip unique" })).toBeDisabled();
  for (const modifier of ["Shift", "ControlOrMeta"]) {
    await app.keyboard.down(modifier); await app.mouse.click(box.x + 140, box.y + 40); await app.keyboard.up(modifier);
    await expect(playlist.getByText(modifier === "Shift" ? "1 selected" : "2 selected", { exact: true })).toBeVisible();
  }
  await app.mouse.move(box.x + 140, box.y + 40); await app.mouse.down(); await app.mouse.move(box.x + 143, box.y + 40); await app.mouse.up();
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls.length);
});
test("Save cancels held group input before saving canonical clips", async ({ app, mock }) => {
  const { box } = await setup(app), before = await state(app), calls = await mock.calls("arrangement_edit");
  await mock.eval("m => m.queueDialog('/qa/projects/group.grooviq')");
  await app.mouse.move(box.x + 140, box.y + 40); await app.mouse.down(); await app.mouse.move(box.x + 188, box.y + 40);
  await app.keyboard.press("ControlOrMeta+s");
  await expect.poll(async () => (await mock.calls("project_save_as")).length).toBe(1);
  await app.mouse.up();
  expect((await state(app)).clips).toEqual(before.clips);
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls.length);
});
test("replacement retires group capture and selection", async ({ app, mock }) => {
  const bpm = app.getByRole("textbox", { name: "BPM", exact: true });
  await bpm.fill("121");
  await bpm.press("Enter");
  await expect.poll(async () => (await mock.view()).dirty).toBe(true);
  const { box, playlist } = await setup(app), before = await state(app), calls = await mock.calls("arrangement_edit");
  await app.mouse.move(box.x + 140, box.y + 40); await app.mouse.down(); await app.mouse.move(box.x + 188, box.y + 40);
  await app.keyboard.press("ControlOrMeta+n");
  await expect(app.getByRole("dialog", { name: "Save changes?" })).toBeVisible();
  await app.mouse.up();
  await app.getByRole("button", { name: "Don't save", exact: true }).click();
  await expect.poll(async () => (await state(app)).generation).toBe(before.generation + 1);
  await expect(playlist.getByText("0 selected", { exact: true })).toBeVisible();
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls.length);
});
test("native refusal preserves all group members", async ({ app, mock }) => {
  const { box, playlist } = await setup(app), before = await state(app);
  await mock.eval("m => m.refuseWorkspaceNext('group refusal')");
  await app.mouse.move(box.x + 140, box.y + 40); await app.mouse.down(); await app.mouse.move(box.x + 188, box.y + 40); await app.mouse.up();
  await expect(playlist.getByRole("alert")).toContainText("group refusal");
  expect((await state(app)).clips).toEqual(before.clips);
});

test("marquee release inside the threshold uses the returned rectangle", async ({ app, mock }) => {
  const { box, playlist } = await setup(app);
  const before = await state(app), calls = await mock.calls("arrangement_edit");
  await app.mouse.move(box.x + 10, box.y + 8);
  await app.mouse.down();
  await app.mouse.move(box.x + 490, box.y + 62, { steps: 6 });
  await app.mouse.move(box.x + 12, box.y + 9);
  await expect(playlist.locator(".playlist-marquee")).toHaveCSS("width", "2px");
  await app.mouse.up();
  await expect(playlist.getByText("0 selected", { exact: true })).toBeVisible();
  await app.keyboard.press("ArrowRight");
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls.length);
  expect((await state(app)).clips).toEqual(before.clips);
});

test.describe("dense group preview", () => {
  const denseCount = 1600;
  const content = { type: "pattern" as const, pattern: 0, offsetTick: 0, lengthTicks: 3840 };
  test.use({ seed: { project: project(["Kick"], {}, { nextId: 2001, arrangement: {
    tracks: [{ id: 2, name: "Track 1", color: 0, mute: false }],
    clips: [...Array.from({ length: denseCount }, (_, index) => ({
      id: 3 + index, track: 2, name: `Beat ${index + 1}`, startTick: 960, muted: false, content,
    })), { id: 2000, track: 2, name: "Later clip", startTick: 120960, muted: false, content }],
    audioAssets: [], loopRegion: null,
  } }), files: soundsFor(["Kick"]) } });

  test("keeps dense group labels and canonical uploads bounded while previewing and scrolling", async ({ app, mock }) => {
    await app.evaluate(() => {
      const scope = window as unknown as { playlistCanonicalUploads: number };
      scope.playlistCanonicalUploads = 0;
      if (typeof WebGL2RenderingContext === "undefined") return;
      const prototype = WebGL2RenderingContext.prototype;
      prototype.bufferData = new Proxy(prototype.bufferData, {
        apply(original, context: WebGL2RenderingContext, args) {
          if (args[2] === context.STATIC_DRAW && args[1] instanceof Float32Array && args[1].length >= 1600 * 8) {
            scope.playlistCanonicalUploads++;
          }
          return Reflect.apply(original, context, args);
        },
      });
    });
    const uploads = () => app.evaluate(() => (window as unknown as { playlistCanonicalUploads: number }).playlistCanonicalUploads);
    await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
    await app.keyboard.press("F5");
    const playlist = app.getByRole("region", { name: "Playlist", exact: true });
    const canvas = playlist.getByRole("application", { name: "Playlist clips" });
    await expect(canvas).toHaveAttribute("aria-disabled", "false");
    await expect(playlist.getByRole("status")).toHaveText("1601 clips");
    const box = await canvas.boundingBox();
    if (!box) throw new Error("Missing playlist canvas");
    await playlist.getByRole("button", { name: "Select", exact: true }).click();
    await app.mouse.move(box.x + 15, box.y + 9);
    await app.mouse.down();
    await app.mouse.move(box.x + 245, box.y + 63, { steps: 5 });
    await app.mouse.up();
    await expect(playlist.getByText("1600 selected", { exact: true })).toBeVisible();
    const canonicalUploads = await uploads();
    if (await canvas.getAttribute("data-renderer") === "webgl2") expect(canonicalUploads).toBeGreaterThan(0);
    const calls = await mock.calls("arrangement_edit");
    await app.mouse.move(box.x + 140, box.y + 39);
    await app.mouse.down();
    await app.mouse.move(box.x + 188, box.y + 39, { steps: 5 });
    await expect(playlist.locator(".playlist-labels").getByText("Beat 1", { exact: true })).toHaveCSS("left", "102px");
    expect(await uploads()).toBe(canonicalUploads);
    expect(await playlist.locator(".playlist-labels span").count()).toBeLessThanOrEqual(1025);
    expect(await mock.calls("arrangement_edit")).toHaveLength(calls.length);
    await app.keyboard.press("Escape");
    await app.mouse.up();
    await playlist.locator(".playlist-scroll").evaluate(element => { element.scrollLeft = 6000; });
    await expect(playlist.locator(".playlist-labels").getByText("Later clip", { exact: true })).toBeVisible();
    await expect(playlist.locator(".playlist-labels").getByText(/^Beat /)).toHaveCount(0);
    await expect(playlist.getByText("1600 selected", { exact: true })).toBeVisible();
    expect(await uploads()).toBe(canonicalUploads);
  });
});
