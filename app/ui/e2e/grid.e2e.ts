// Owner checklist 1, 2 and 11: the step grid draws, lines up with the rows, and takes keys.
import type { Page } from "@playwright/test";
import { cellPoint, clickCell, expect, grid, project, soundsFor, test } from "./fixture";
import type { MockProject } from "./mockTauri";

const KIT = ["Kick", "Snare", "Hat", "Clap"];

/** The kit with hits at several velocities. */
function kitWithVelocities(): MockProject {
  const p = project(KIT, { 1: [0, 4, 8, 12], 2: [4, 12] });
  const hat = [0, 2, 4, 6, 8, 10, 12, 14].map((step, i) => ({
    channel: 3,
    tick: step * 240,
    velocity: [1, 0.25, 0.8, 0.5, 1, 0.25, 0.8, 0.5][i],
  }));
  p.pattern.notes.push(...hat, { channel: 4, tick: 12 * 240, velocity: 0.6 });
  return p;
}

const status = (page: Page) => page.getByTestId("step-grid-status");

test.describe("drawing (check 1)", () => {
  test.use({ seed: { project: kitWithVelocities(), files: soundsFor(KIT) } });

  test("lit steps, velocity fill, beat shading, the focus ring and the playhead", async ({ app, mock }, info) => {
    const g = grid(app);
    // WebGL2 drew it: the canvas kept its WebGL2 renderer (no fallback to 2D).
    expect(await g.evaluate((c: HTMLCanvasElement) => c.getContext("webgl2") !== null)).toBe(true);
    await g.focus();
    await app.keyboard.press("ArrowDown");
    await app.keyboard.press("ArrowDown");
    await app.keyboard.press("ArrowRight");
    await app.keyboard.press("ArrowRight");
    await expect(status(app)).toHaveText("Hat, step 3, on, velocity 25%");
    await info.attach("rack-aria", { body: await app.getByRole("region", { name: "Channel rack" }).ariaSnapshot(), contentType: "text/yaml" });
    await g.screenshot({ path: info.outputPath("grid-focus.png") });
    await app.getByRole("button", { name: "Play" }).click();
    // Into the second beat, so the playhead stands on a dark (unlit) column next to pink kicks.
    await expect.poll(() => mock.eval<number>("(m) => m.ticks()")).toBeGreaterThan(5 * 240);
    await g.screenshot({ path: info.outputPath("grid-playing.png") });
    await app.getByRole("button", { name: "Stop" }).click();
  });
});

test.describe("rows line up with the grid (check 2)", () => {
  /** Each row's centre against the centre of its grid row, in CSS pixels. */
  async function offsets(page: Page, names: string[]) {
    const box = await grid(page).boundingBox();
    if (!box) throw new Error("no grid");
    const out: number[] = [];
    for (const [row, name] of names.entries()) {
      const r = await page.getByRole("group", { name, exact: true }).boundingBox();
      if (!r) throw new Error(`no row ${name}`);
      out.push(Math.abs(r.y + r.height / 2 - (box.y + cellPoint(row, 0).y)));
    }
    return out;
  }

  for (const count of [1, 4, 12]) {
    const names = Array.from({ length: count }, (_, i) => `Sound ${i + 1}`);
    test.describe(`${count} channel(s)`, () => {
      test.use({ seed: { project: project(names, { 1: [0, 8] }, {}), files: soundsFor(names) } });
      test("every row centre matches its grid row", async ({ app }, info) => {
        for (const d of await offsets(app, names)) expect(d).toBeLessThanOrEqual(1);
        await app.getByRole("region", { name: "Channel rack" }).screenshot({ path: info.outputPath(`rows-${count}.png`) });
      });
    });
  }

  const MISSING = ["Kick", "Lost Snare", "Hat"];
  test.describe("with a missing sample", () => {
    // No file for "Lost Snare": its row says Missing and offers Relink.
    test.use({ seed: { project: project(MISSING), files: soundsFor(["Kick", "Hat"]) } });
    test("the Missing row keeps the rows aligned", async ({ app }, info) => {
      await expect(app.getByRole("button", { name: "Relink Lost Snare" })).toBeVisible();
      for (const d of await offsets(app, MISSING)) expect(d).toBeLessThanOrEqual(1);
      await app.getByRole("region", { name: "Channel rack" }).screenshot({ path: info.outputPath("rows-missing.png") });
    });
  });

  const MANY = Array.from({ length: 24 }, (_, i) => `Sound ${i + 1}`);
  test.describe("after scrolling both ways", () => {
    test.use({
      seed: { project: project(MANY, { 1: [0], 24: [63] }, { pattern: { id: 0, name: "Pattern 1", steps: 64, swing: 0, notes: [] } }), files: soundsFor(MANY) },
      viewport: { width: 900, height: 560 },
    });
    test("rows stay aligned after vertical and horizontal scrolls", async ({ app }, info) => {
      const g = grid(app);
      await g.hover({ position: { x: 40, y: 40 } });
      await app.mouse.wheel(0, 600);
      await app.mouse.wheel(900, 0);
      await app.waitForTimeout(200);
      for (const d of await offsets(app, MANY)) expect(d).toBeLessThanOrEqual(1);
      await app.screenshot({ path: info.outputPath("rows-scrolled.png") });
      await app.mouse.wheel(-900, -600);
      await app.waitForTimeout(200);
      for (const d of await offsets(app, MANY)) expect(d).toBeLessThanOrEqual(1);
    });
  });
});

test.describe("grid keys and the right button (check 11)", () => {
  test.use({ seed: { project: project(KIT, { 1: [0] }), files: soundsFor(KIT) } });

  test("arrows, Enter, Delete, Alt+Up/Down, Space and Cmd+S", async ({ app, mock }) => {
    const g = grid(app);
    await g.focus();
    await expect(status(app)).toHaveText("Kick, step 1, on, velocity 80%");
    await app.keyboard.press("ArrowRight");
    await app.keyboard.press("ArrowDown");
    await expect(status(app)).toHaveText("Snare, step 2, off");
    await app.keyboard.press("Enter");
    await expect(status(app)).toHaveText("Snare, step 2, on, velocity 80%");
    await app.keyboard.press("Alt+ArrowUp");
    await expect(status(app)).toHaveText("Snare, step 2, on, velocity 90%");
    await app.keyboard.press("Alt+ArrowDown");
    await app.keyboard.press("Alt+ArrowDown");
    await expect(status(app)).toHaveText("Snare, step 2, on, velocity 70%");
    await app.keyboard.press("Delete");
    await expect(status(app)).toHaveText("Snare, step 2, off");
    await app.keyboard.press("End");
    await app.keyboard.press("Home");
    await expect(status(app)).toHaveText("Snare, step 1, off");
    // Space is play/stop while the grid has focus.
    await app.keyboard.press("Space");
    await expect.poll(() => mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(true);
    await app.keyboard.press("Space");
    await expect.poll(() => mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(false);
    // Cmd+S on an untitled project asks where, then saves there.
    await mock.eval("(m) => m.queueDialog('/qa/projects/Keys test.grooviq')");
    await app.keyboard.press("ControlOrMeta+s");
    await expect.poll(() => mock.view().then((v) => v.path)).toBe("/qa/projects/Keys test.grooviq");
    expect((await mock.view()).dirty).toBe(false);
    await expect(g).toBeFocused();
  });

  test("a right click clears a step and opens no context menu", async ({ app, mock }) => {
    await app.evaluate(() => {
      const w = window as unknown as { contextDefault: boolean[] };
      w.contextDefault = [];
      window.addEventListener("contextmenu", (e) => w.contextDefault.push(e.defaultPrevented));
    });
    await clickCell(app, 0, 0, "right");
    await expect.poll(() => mock.view().then((v) => v.pattern.cells["1"][0])).toBeNull();
    // Right-clicking an empty step changes nothing.
    await clickCell(app, 1, 3, "right");
    expect(await app.evaluate(() => (window as unknown as { contextDefault: boolean[] }).contextDefault)).toEqual([true, true]);
    expect((await mock.calls("project_edit")).map((c) => c.args.edit)).toEqual([{ kind: "clearStep", channel: 1, step: 0 }]);
  });

  test("a click toggles and a vertical drag sets the velocity", async ({ app, mock }) => {
    await clickCell(app, 2, 5);
    await expect.poll(() => mock.view().then((v) => v.pattern.cells["3"][5])).toBe(0.8);
    const box = await grid(app).boundingBox();
    if (!box) throw new Error("no grid");
    const p = cellPoint(2, 5);
    await app.mouse.move(box.x + p.x, box.y + p.y);
    await app.mouse.down();
    await app.mouse.move(box.x + p.x, box.y + p.y + 40, { steps: 8 });
    await app.mouse.up();
    await expect.poll(() => mock.view().then((v) => v.pattern.cells["3"][5])).toBeLessThan(0.8);
  });
});
