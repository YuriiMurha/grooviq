// Owner checklist 1, 2 and 11: the step grid draws, lines up with the rows, and takes keys.
import type { Page } from "@playwright/test";
import { brightness, cellPoint, clickCell, colourDistance, expect, grid, pixelsAt, project, soundsFor, test } from "./fixture";
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
    // A point in cell (row, step), offset from its centre.
    const at = (row: number, step: number, dx = 0, dy = 0) => ({ x: cellPoint(row, step).x + dx, y: cellPoint(row, step).y + dy });
    await g.focus();
    for (const key of ["ArrowDown", "ArrowDown", "ArrowRight", "ArrowRight"]) await app.keyboard.press(key);
    await expect(status(app)).toHaveText("Hat, step 3, on, velocity 25%");
    await info.attach("rack-aria", { body: await app.getByRole("region", { name: "Channel rack" }).ariaSnapshot(), contentType: "text/yaml" });
    await g.screenshot({ path: info.outputPath("grid-focus.png") });
    const [empty1, empty5, lit, shadow, gap, v25top, v25bottom, v100top, ring, plainGap] = await pixelsAt(app, g, [
      at(0, 1), // an empty step in the first beat
      at(0, 5), // an empty step in the second beat
      at(0, 0), // a kick
      at(0, 0, 15, 0), // just right of the kick: its offset second ink
      at(0, 1, 15, 0), // the gap right of an empty step
      at(2, 2, 0, -10), // a hat at 25 %: the top stays dark
      at(2, 2, 0, 10), // ... the bottom is lit
      at(2, 0, 0, -10), // a hat at 100 %: lit to the top
      at(2, 2, -16, 0), // left of the focused hat: the focus ring
      at(3, 6, -16, 0), // left of an unfocused empty step
    ]);
    expect(colourDistance(lit, empty1), "a lit step stands out").toBeGreaterThan(150);
    expect(colourDistance(shadow, gap), "the lit step's offset shadow").toBeGreaterThan(100);
    expect(colourDistance(shadow, lit), "the shadow is a second ink").toBeGreaterThan(100);
    expect(colourDistance(v25bottom, v25top), "velocity 25 % fills only the bottom").toBeGreaterThan(150);
    expect(colourDistance(v100top, v25bottom), "velocity 100 % fills to the top").toBeLessThan(10);
    expect(colourDistance(empty1, empty5), "beats are shaded apart").toBeGreaterThan(5);
    expect(brightness(ring) - brightness(plainGap), "the focus ring").toBeGreaterThan(150);

    // The playhead: with the transport pinned at a step, exactly that column of empty snare
    // steps changes colour; it follows the position; Stop clears it.
    const snareRow = Array.from({ length: 16 }, (_, step) => step).filter((step) => step !== 4 && step !== 12);
    const before = await pixelsAt(app, g, snareRow.map((step) => at(1, step)));
    const playheadColumns = async () => {
      const now = await pixelsAt(app, g, snareRow.map((step) => at(1, step)));
      return snareRow.filter((_, i) => colourDistance(now[i], before[i]) > 20);
    };
    expect(await playheadColumns(), "no playhead while stopped").toEqual([]);
    await app.getByRole("button", { name: "Play" }).click();
    await expect.poll(() => mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(true);
    for (const step of [2, 9]) {
      await mock.eval(`(m) => m.hold(${step * 240 + 10})`);
      await expect.poll(playheadColumns, { message: `the playhead on step ${step + 1}` }).toEqual([step]);
    }
    await g.screenshot({ path: info.outputPath("grid-playing.png") });
    await app.getByRole("button", { name: "Stop" }).click();
    await expect.poll(playheadColumns, { message: "Stop clears the playhead" }).toEqual([]);
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
    // "Lost Snare" has no sample in the project file: its row says Missing and offers Relink.
    const lost = project(MISSING);
    lost.channels[1].embedded = false;
    test.use({ seed: { project: lost, files: soundsFor(["Kick", "Hat"]) } });
    test("the Missing row keeps the rows aligned", async ({ app }, info) => {
      await expect(app.getByRole("button", { name: "Relink Lost Snare" })).toBeVisible();
      for (const d of await offsets(app, MISSING)) expect(d).toBeLessThanOrEqual(1);
      await app.getByRole("region", { name: "Channel rack" }).screenshot({ path: info.outputPath("rows-missing.png") });
    });
  });

  const MANY = Array.from({ length: 24 }, (_, i) => `Sound ${i + 1}`);
  test.describe("after scrolling both ways", () => {
    const long = project(MANY, { 1: [0], 24: [63] });
    long.pattern.steps = 64;
    test.use({ seed: { project: long, files: soundsFor(MANY) }, viewport: { width: 900, height: 560 } });

    /** How far the grid's scrolling ancestors are scrolled, summed: [left, top]. */
    const scrolled = (page: Page) =>
      grid(page).evaluate((el) => {
        let [left, top] = [0, 0];
        for (let n: HTMLElement | null = el.parentElement; n; n = n.parentElement) {
          left += n.scrollLeft;
          top += n.scrollTop;
        }
        return [left, top];
      });

    test("rows stay aligned after vertical and horizontal scrolls", async ({ app }, info) => {
      const g = grid(app);
      await g.hover({ position: { x: 40, y: 40 } });
      await app.mouse.wheel(0, 600);
      await app.mouse.wheel(900, 0);
      // It really scrolled both ways, and the last channel and the last step came into view.
      await expect.poll(() => scrolled(app)).toEqual([expect.any(Number), expect.any(Number)]);
      await expect.poll(async () => (await scrolled(app)).every((d) => d > 0), { message: "scrolled both ways" }).toBe(true);
      await expect(app.getByRole("group", { name: "Sound 24", exact: true })).toBeInViewport();
      for (const d of await offsets(app, MANY)) expect(d).toBeLessThanOrEqual(1);
      await app.screenshot({ path: info.outputPath("rows-scrolled.png") });
      await g.hover({ position: { x: 40, y: 40 } });
      await app.mouse.wheel(-900, 0);
      await app.mouse.wheel(0, -600);
      await expect.poll(() => scrolled(app)).toEqual([0, 0]);
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

test.describe("a new pixel ratio (check 3, moving between screens)", () => {
  test.use({ seed: { project: project(KIT, { 1: [0] }), files: soundsFor(KIT) } });

  test("the grid redraws at the new ratio without a window resize", async ({ app, browserName }, info) => {
    test.skip(browserName !== "chromium", "changes the ratio through the Chrome DevTools Protocol");
    const backing = () => grid(app).evaluate((c: HTMLCanvasElement) => c.width / c.clientWidth);
    expect(await backing()).toBeCloseTo(1);
    const cdp = await app.context().newCDPSession(app);
    await cdp.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 800, deviceScaleFactor: 2, mobile: false });
    await expect.poll(() => app.evaluate(() => window.devicePixelRatio)).toBe(2);
    await app.waitForTimeout(500);
    // Known defect #117: the grid re-reads the ratio only on `resize`, so it stays at 1x. This
    // pins it; when #117 is fixed this fails: expect 2 here instead.
    info.annotations.push({ type: "issue", description: "#117" });
    expect(await backing(), "#117 fixed? expect a 2x backing store now").toBeCloseTo(1);
  });
});
