import type { Page } from "@playwright/test";
import type { ArrangementPage } from "../src/api/arrangement";
import { expect, project, soundsFor, test } from "./fixture";
test.use({ seed: { project: project(["Kick"], { 1: [0, 4, 8, 12] }), files: soundsFor(["Kick"]) } });
const state = (page: Page): Promise<ArrangementPage> => page.evaluate(() => (window as unknown as { __TAURI_INTERNALS__: { invoke: (command: string, args: unknown) => Promise<ArrangementPage> } }).__TAURI_INTERNALS__.invoke("arrangement_view", { offset: 0, limit: 256 }));
async function setup(page: Page) {
  await expect(page.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await page.keyboard.press("F5");
  const playlist = page.getByRole("region", { name: "Playlist", exact: true });
  const canvas = playlist.getByRole("application", { name: "Playlist clips" });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  const box = await canvas.boundingBox(); if (!box) throw new Error("Missing playlist canvas");
  await page.mouse.click(box.x + 48, box.y + 32);
  await expect.poll(async () => (await state(page)).total).toBe(1);
  await page.mouse.click(box.x + 288, box.y + 32);
  await expect.poll(async () => (await state(page)).total).toBe(2);
  await playlist.getByRole("button", { name: "Select", exact: true }).click();
  await page.mouse.move(box.x + 10, box.y + 8); await page.mouse.down();
  await page.mouse.move(box.x + 490, box.y + 62, { steps: 6 }); await page.mouse.up();
  await expect(playlist.getByRole("status")).toContainText("2 selected");
  return { playlist, canvas, box };
}
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
  await expect(playlist.getByRole("status")).toContainText("2 selected");
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
test("modifier toggles and sub-threshold clicks do not edit; group destructive controls stay single-only", async ({ app, mock }) => {
  const { box, playlist } = await setup(app), calls = await mock.calls("arrangement_edit");
  await expect(playlist.getByRole("button", { name: "Make selected clip unique" })).toBeDisabled();
  await expect(playlist.getByRole("button", { name: "Delete selected clip" })).toBeDisabled();
  for (const modifier of ["Shift", "ControlOrMeta"]) {
    await app.keyboard.down(modifier); await app.mouse.click(box.x + 140, box.y + 40); await app.keyboard.up(modifier);
    await expect(playlist.getByRole("status")).toContainText(modifier === "Shift" ? "1 selected" : "2 selected");
  }
  await app.mouse.move(box.x + 140, box.y + 40); await app.mouse.down(); await app.mouse.move(box.x + 143, box.y + 40); await app.mouse.up();
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls.length);
});
test("Save cancels held group input before saving canonical clips", async ({ app, mock }) => {
  const { box } = await setup(app), before = await state(app), calls = await mock.calls("arrangement_edit");
  await mock.eval("m => m.queueDialog('/qa/projects/group.grooviq')");
  await app.mouse.move(box.x + 140, box.y + 40); await app.mouse.down(); await app.mouse.move(box.x + 188, box.y + 40);
  await app.keyboard.press("ControlOrMeta+s");
  await expect.poll(async () => (await mock.calls("project_save")).length).toBe(1);
  await app.mouse.up();
  expect((await state(app)).clips).toEqual(before.clips);
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls.length);
});
test("replacement retires group capture and selection", async ({ app, mock }) => {
  const { box, playlist } = await setup(app), before = await state(app), calls = await mock.calls("arrangement_edit");
  await app.mouse.move(box.x + 140, box.y + 40); await app.mouse.down(); await app.mouse.move(box.x + 188, box.y + 40);
  await app.keyboard.press("ControlOrMeta+n");
  await expect(app.getByRole("dialog", { name: "Save changes?" })).toBeVisible();
  await app.mouse.up();
  await app.getByRole("button", { name: "Don't save", exact: true }).click();
  await expect.poll(async () => (await state(app)).generation).toBe(before.generation + 1);
  await expect(playlist.getByRole("status")).toContainText("0 selected");
  expect(await mock.calls("arrangement_edit")).toHaveLength(calls.length);
});
test("native refusal preserves all group members", async ({ app, mock }) => {
  const { box, playlist } = await setup(app), before = await state(app);
  await mock.eval("m => m.refuseWorkspaceNext('group refusal')");
  await app.mouse.move(box.x + 140, box.y + 40); await app.mouse.down(); await app.mouse.move(box.x + 188, box.y + 40); await app.mouse.up();
  await expect(playlist.getByRole("alert")).toContainText("group refusal");
  expect((await state(app)).clips).toEqual(before.clips);
});
