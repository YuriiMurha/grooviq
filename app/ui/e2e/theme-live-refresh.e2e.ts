import type { ElementHandle, Locator, Page } from "@playwright/test";
import { cellPoint, colourDistance, expect, grid, pixelsAt, project, soundsFor, test } from "./fixture";
import { at, canvas, lane, local, openRoll, sessionNotes, stats, token } from "./rollHelpers";

const PROBE = {
  ink: [24, 241, 51],
  row: [104, 33, 32],
  outside: [68, 21, 90],
  select: [31, 224, 251],
  wave: [252, 210, 27],
};
const rgb = (colour: number[]) => `rgb(${colour.join(", ")})`;

async function addProbe(page: Page) {
  const original = await page.locator("html").getAttribute("data-theme");
  await page.addStyleTag({ content: `
    :root[data-theme="probe"] {
      --ink-1: ${rgb(PROBE.ink)};
      --roll-row: ${rgb(PROBE.row)};
      --roll-outside: ${rgb(PROBE.outside)};
      --roll-select: ${rgb(PROBE.select)};
    }
    :root[data-theme="probe"] .sample-wave { color: ${rgb(PROBE.wave)}; }
  ` });
  return original;
}

const setTheme = (page: Page, value: string | null) => page.evaluate((theme) => {
  if (theme === null) delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
}, value);

type ContextLog = { __contextCalls: (canvas: HTMLCanvasElement) => string[] };

/** Logs every context request per canvas: each renderer build asks once, a repaint never does
 * (a canvas hands back the same context object, so comparing contexts proves nothing). */
const instrumentContexts = (page: Page, webgl: boolean) => page.addInitScript((webgl) => {
  const get = HTMLCanvasElement.prototype.getContext;
  const calls = new WeakMap<HTMLCanvasElement, string[]>();
  (window as unknown as ContextLog).__contextCalls = (canvas) => [...(calls.get(canvas) ?? [])];
  HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, kind: string, options?: unknown) {
    calls.set(this, [...(calls.get(this) ?? []), kind]);
    return kind === "webgl2" && !webgl ? null : get.call(this, kind, options);
  } as typeof get;
}, webgl);

const contextCalls = (locator: Locator) =>
  locator.evaluate((canvas: HTMLCanvasElement) => (window as unknown as ContextLog).__contextCalls(canvas));

const sameNode = (locator: Locator, original: ElementHandle) => locator.evaluate((node, before) => node === before, original);

const sampleProject = project(["Kick"], { 1: [1] });
const rollProject = project(["Kick"]);
rollProject.pattern.notes = [
  { id: 1, channel: 1, tick: 240, length: 240, key: 60, velocity: 1 },
  { id: 2, channel: 1, tick: 960, length: 240, key: 62, velocity: 1 },
];

for (const renderer of ["webgl2", "canvas2d"] as const) {
  test.describe(`live theme / ${renderer}`, () => {
    test.beforeEach(async ({ page }) => {
      await instrumentContexts(page, renderer === "webgl2");
    });

    test.describe("step grid", () => {
      test.use({ seed: { project: sampleProject, files: soundsFor(["Kick"]) } });

      test("repaints the lit cell in place while stopped", async ({ app, mock }, info) => {
        const g = grid(app);
        const original = await addProbe(app);
        await g.focus();
        const sample = () => pixelsAt(app, g, [cellPoint(0, 1)]).then(([pixel]) => pixel);
        const ink = await token(app, "--ink-1");
        await expect.poll(async () => colourDistance(await sample(), ink)).toBeLessThan(8);
        const before = await sample();
        const element = await g.elementHandle();
        // The app's own requests on this canvas show which renderer drew it and how often it was built.
        const contexts = await contextCalls(g);
        expect(contexts.length).toBeGreaterThan(0);
        expect(new Set(contexts)).toEqual(new Set([renderer === "webgl2" ? "webgl2" : "2d"]));
        await g.screenshot({ path: info.outputPath("grid-original.png") });

        await setTheme(app, "probe");
        await expect.poll(async () => colourDistance(await sample(), PROBE.ink), { message: "lit cell uses the probe ink" }).toBeLessThan(8);
        expect(colourDistance(before, PROBE.ink)).toBeGreaterThan(80);
        expect(await sameNode(g, element)).toBe(true);
        expect(await contextCalls(g), "a theme change repaints without rebuilding the renderer").toEqual(contexts);
        await expect(g).toBeFocused();
        expect(await mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(false);
        await g.screenshot({ path: info.outputPath("grid-probe.png") });

        await setTheme(app, original);
        await expect.poll(async () => colourDistance(await sample(), before)).toBeLessThan(8);
        expect(await sameNode(g, element)).toBe(true);
        expect(await contextCalls(g)).toEqual(contexts);
      });
    });

    test.describe("piano roll", () => {
      test.use({ seed: { project: rollProject, files: soundsFor(["Kick"]) } });

      test("repaints the notes and background and preserves edit state", async ({ app, mock }, info) => {
        await openRoll(app);
        expect((await stats(app)).renderer).toBe(renderer);
        const original = await addProbe(app);
        const roll = canvas(app);
        const velocity = lane(app);

        // Create an undoable edit and then select exactly one of the two original notes.
        const added = await at(app, 1690, 64);
        const seedNotes = await sessionNotes(mock);
        await app.mouse.click(added.x, added.y);
        await expect.poll(async () => (await sessionNotes(mock)).length).toBe(3);
        await expect.poll(async () => (await mock.calls("edit_history_enabled")).at(-1)?.args).toEqual({ undo: true, redo: false });
        const picked = await at(app, 360, 60);
        await app.mouse.click(picked.x, picked.y);

        const rollPoints = await Promise.all([local(app, 360, 60), local(app, 1080, 62), local(app, 600, 65)]);
        const selectedBar = await local(app, 240, 60);
        const unselectedBar = await local(app, 960, 62);
        const lanePoints = [{ x: selectedBar.x + 1, y: 50 }, { x: unselectedBar.x + 1, y: 50 }, { x: rollPoints[2].x, y: 50 }];
        const rollPixels = () => pixelsAt(app, roll, rollPoints);
        const lanePixels = () => pixelsAt(app, velocity, lanePoints);
        const maxDistance = (actual: number[][], expected: number[][]) => Math.max(...actual.map((pixel, i) => colourDistance(pixel, expected[i])));
        const ink = await token(app, "--ink-1");
        const select = await token(app, "--select");
        await expect.poll(async () => maxDistance((await lanePixels()).slice(0, 2), [select, ink]),
          { message: "velocity bars identify the selected and unselected notes" }).toBeLessThan(8);
        const beforeRoll = await rollPixels();
        const beforeLane = await lanePixels();
        expect(maxDistance(beforeRoll.slice(0, 2), [ink, ink])).toBeLessThan(8);
        const elements = await Promise.all([roll.elementHandle(), velocity.elementHandle()]);
        const builds = (await stats(app)).bufferBuilds;
        const originalNotes = await sessionNotes(mock);
        const history = await mock.view();
        const workspace = app.getByRole("region", { name: "Piano roll" });
        await workspace.screenshot({ path: info.outputPath("roll-original.png") });

        await setTheme(app, "probe");
        const probeRoll = [PROBE.ink, PROBE.ink, PROBE.row];
        const probeLane = [PROBE.select, PROBE.ink, PROBE.outside];
        for (const [before, probe] of [[beforeRoll, probeRoll], [beforeLane, probeLane]]) {
          expect(Math.min(...before.map((pixel, i) => colourDistance(pixel, probe[i])))).toBeGreaterThan(40);
        }
        await expect.poll(async () => maxDistance(await rollPixels(), probeRoll),
          { message: "the roll's selected note, unselected note and background use the probe colours" }).toBeLessThan(8);
        await expect.poll(async () => maxDistance(await lanePixels(), probeLane),
          { message: "the velocity bars preserve distinct selected and unselected probe colours" }).toBeLessThan(8);
        await workspace.screenshot({ path: info.outputPath("roll-probe.png") });

        const expectPreserved = async () => {
          expect(await sameNode(roll, elements[0])).toBe(true);
          expect(await sameNode(velocity, elements[1])).toBe(true);
          expect(await stats(app)).toMatchObject({ renderer, bufferBuilds: builds });
          expect(await sessionNotes(mock)).toEqual(originalNotes);
          expect(await mock.view()).toMatchObject({ canUndo: history.canUndo, canRedo: history.canRedo, notesRevision: history.notesRevision });
          expect((await mock.calls("edit_history_enabled")).at(-1)?.args).toEqual({ undo: true, redo: false });
          expect(await mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(false);
        };
        await expectPreserved();
        await setTheme(app, original);
        await expect.poll(async () => maxDistance(await rollPixels(), beforeRoll)).toBeLessThan(8);
        await expect.poll(async () => maxDistance(await lanePixels(), beforeLane)).toBeLessThan(8);
        await expectPreserved();
        await mock.eval("(m) => m.emit('edit-history', false)");
        await expect.poll(() => sessionNotes(mock)).toEqual(seedNotes);
      });
    });
  });
}

test.describe("sampler waveform", () => {
  test.use({ seed: { project: sampleProject, files: soundsFor(["Kick"]) } });

  test("repaints the existing waveform from its loaded peaks", async ({ app, mock }, info) => {
    const original = await addProbe(app);
    await app.getByRole("button", { name: "Edit the Kick sampler" }).click();
    const wave = app.getByRole("img", { name: "Kick waveform" });
    await expect(wave).toBeVisible();
    const sample = () => wave.evaluate((c: HTMLCanvasElement) => {
      // Inspect the canvas itself so the CSS around it cannot satisfy the check.
      const pixel = document.createElement("canvas");
      pixel.width = pixel.height = 1;
      const ctx = pixel.getContext("2d", { willReadFrequently: true });
      if (!ctx) throw new Error("No waveform context");
      ctx.drawImage(c, Math.round(c.width * 0.15), Math.round(c.height * 0.4), 1, 1, 0, 0, 1, 1);
      return Array.from(ctx.getImageData(0, 0, 1, 1).data);
    });
    await expect.poll(async () => (await sample())[3]).toBeGreaterThan(200);
    const before = await sample();
    const element = await wave.elementHandle();
    await wave.screenshot({ path: info.outputPath("waveform-original.png") });

    await setTheme(app, "probe");
    expect(colourDistance(before, PROBE.wave)).toBeGreaterThan(80);
    await expect.poll(async () => colourDistance(await sample(), PROBE.wave), { message: "the waveform uses the probe ink" }).toBeLessThan(8);
    expect(await sameNode(wave, element)).toBe(true);
    await wave.screenshot({ path: info.outputPath("waveform-probe.png") });

    await setTheme(app, original);
    await expect.poll(async () => colourDistance(await sample(), before)).toBeLessThan(8);
    expect(await sameNode(wave, element)).toBe(true);
    expect(await mock.calls("channel_waveform")).toHaveLength(1);
  });
});
