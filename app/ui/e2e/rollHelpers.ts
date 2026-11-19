// Shared by the piano roll's specs that read the roll's own counters and pixels.
import type { Page, TestInfo } from "@playwright/test";
import { expect, project, test, workspaceReady } from "./fixture";
import type { MockNote, MockProject } from "./mockTauri";

export interface RollStats {
  bufferBuilds: number;
  draws: number;
  renderer: string;
  scaleMask: number;
  view: { tickPx: number; keyPx: number; scrollTick: number; topKey: number };
}

/** Synth channels "Bass" (id 1, the fourth ink) and "Lead" (id 2, the second ink) holding these
 * notes (a note's `channel` says whose it is); the roll opens on Bass. */
export function twoChannels(notes: MockNote[], extra: Partial<MockProject> = {}): MockProject {
  const p = project([], {}, extra);
  const synth = { sample: null, volume: 1, pan: 0, mute: false, solo: false, sampler: {}, kind: "synth" as const, preset: "Init" };
  p.channels.push({ id: 1, name: "Bass", color: 3, ...synth }, { id: 2, name: "Lead", color: 1, ...synth });
  p.nextId = 3;
  p.pattern.notes = notes;
  return p;
}

export const stats = (page: Page) => page.evaluate(() => (window as unknown as { __grooviqRoll: RollStats }).__grooviqRoll);
export const canvas = (page: Page) => page.getByLabel("Piano roll notes");
export const lane = (page: Page) => page.getByLabel("Velocity lane");
export const tool = (page: Page, name: string) => page.getByRole("region", { name: "Piano roll" }).getByRole("button", { name, exact: true });
export const nextFrame = (page: Page) => page.evaluate(() => new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r()))));

/** Every note the session holds, as [channel, tick, length, key], sorted. */
export const sessionNotes = (mock: { eval: <T>(fn: string) => Promise<T> }) =>
  mock.eval<Required<MockNote>[]>("(m) => m.notes()").then((ns) =>
    ns.map((n) => [n.channel, n.tick, n.length, n.key]).sort((a, b) => a[0] - b[0] || a[3] - b[3] || a[1] - b[1]),
  );

/** The page point over (tick, key row), in the middle of the row. */
export async function at(page: Page, tick: number, key: number) {
  const v = (await stats(page)).view;
  const box = (await canvas(page).boundingBox())!;
  return { x: box.x + (tick - v.scrollTick) * v.tickPx, y: box.y + (v.topKey - key - 0.5) * v.keyPx };
}

/** The same point relative to the canvas, for pixel reads. */
export async function local(page: Page, tick: number, key: number) {
  const p = await at(page, tick, key);
  const box = (await canvas(page).boundingBox())!;
  return { x: p.x - box.x, y: p.y - box.y };
}

export async function drag(page: Page, from: { x: number; y: number }, to: { x: number; y: number }, button: "left" | "right" = "left") {
  await page.mouse.move(from.x, from.y);
  await page.mouse.down({ button });
  await page.mouse.move((from.x + to.x) / 2, (from.y + to.y) / 2, { steps: 4 });
  await page.mouse.move(to.x, to.y, { steps: 4 });
  await page.mouse.up({ button });
}

/** Waits until the roll's view has taken the canvas's size (it recentres when the roll is resized,
 * so a view read before that is stale). */
export async function settled(page: Page, timeout?: number) {
  // Reserve half the case budget for gestures and assertions. Playwright also clamps
  // this finite correctness wait to the remaining test deadline.
  const budget = timeout ?? Math.max(1, Math.floor(test.info().timeout / 2));
  await expect
    .poll(async () => {
      const box = await canvas(page).boundingBox();
      const view = (await stats(page))?.view as { height?: number; width?: number } | undefined;
      return !!box && !!view && Math.abs((view.height ?? 0) - box.height) < 1 && Math.abs((view.width ?? 0) - box.width) < 1;
    }, { timeout: budget, message: "roll geometry must match the canvas before editing" })
    .toBe(true);
  await nextFrame(page);
  await nextFrame(page);
}

/** F7 opens the roll on the first channel; F6 hides the rack to give it the height. */
export async function openRoll(page: Page) {
  await workspaceReady(page);
  await page.keyboard.press("F7");
  await expect(canvas(page)).toBeVisible();
  await page.keyboard.press("F6");
  await expect(page.getByRole("region", { name: "Channel rack" })).toBeHidden();
  await expect.poll(async () => (await stats(page))?.draws ?? 0).toBeGreaterThan(0);
  await settled(page);
}

export async function shot(page: Page, info: TestInfo, name: string) {
  await info.attach(`${name}-aria`, { body: await page.locator("body").ariaSnapshot(), contentType: "text/yaml" });
  await page.screenshot({ path: info.outputPath(`${name}.png`) });
}

/** A theme colour (`--ink-4`, `--ink-4b`, ...) as RGB. */
export const token = (page: Page, name: string) =>
  page.evaluate((n) => {
    const hex = getComputedStyle(document.documentElement).getPropertyValue(n).trim();
    return [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
  }, name);

/** A box selection of every note between two ticks and two key rows, with the Select tool. */
export async function boxSelect(page: Page, t0: number, k0: number, t1: number, k1: number) {
  await tool(page, "Select").click();
  await drag(page, await at(page, t0, k0), await at(page, t1, k1));
  await nextFrame(page);
}

