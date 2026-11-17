import { readFileSync } from "node:fs";
import { test, expect, SYNTH_PARAMS, SYNTH_PRESETS } from "./fixture";
import { installMockTauri } from "./mockTauri";
import { canvas, openRoll, twoChannels } from "./rollHelpers";

test("openRoll waits for the project before sending panel shortcuts", async ({ page }) => {
  const { sampler } = JSON.parse(readFileSync(new URL("../../../tests/fixtures/device-params.json", import.meta.url), "utf8"));
  await page.addInitScript(installMockTauri, { samplerParams: sampler, project: twoChannels([]), synthParams: SYNTH_PARAMS, synthPresets: SYNTH_PRESETS });
  // Hold the initial view while the toolbar is already usable. No elapsed-time delay.
  await page.addInitScript(() => {
    const w = window as unknown as {
      __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown) => Promise<unknown> };
      releaseView: () => void;
      panelKeys: string[];
    };
    const invoke = w.__TAURI_INTERNALS__.invoke;
    const held = new Promise<void>((resolve) => { w.releaseView = resolve; });
    w.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (cmd === "project_view") await held;
      return invoke(cmd, args);
    };
    w.panelKeys = [];
    window.addEventListener("keydown", (e) => {
      if (e.key === "F6" || e.key === "F7") w.panelKeys.push(e.key);
    });
  });
  await page.goto("/");
  await expect(page.getByRole("banner")).toBeVisible();
  await expect(page.getByRole("region", { name: "Channel rack" })).toBeHidden();
  const opening = openRoll(page);
  // Observe a browser turn before releasing the held view: a premature helper sends keys here.
  await page.evaluate(() => new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r()))));
  const premature = await page.evaluate(() => (window as unknown as { panelKeys: string[] }).panelKeys.slice());
  await page.evaluate(() => (window as unknown as { releaseView: () => void }).releaseView());
  await opening;
  expect(premature).toEqual([]);
  await expect(canvas(page)).toBeVisible();
  await expect(page.getByRole("region", { name: "Channel rack" })).toBeHidden();
});
