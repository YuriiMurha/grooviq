import type { Page } from "@playwright/test";
import { cellPoint, clickCell, colourDistance, expect, grid, project, soundsFor, test } from "./fixture";

type GridProbe = {
  draws: number;
  frames: number;
  waitFrames: (count: number) => Promise<void>;
  countPageCallbacks: () => Promise<number>;
  pixels: (points: { x: number; y: number }[]) => number[][];
};
declare global {
  interface Window {
    stepGridProbe: GridProbe;
  }
}

test.use({ seed: { project: project(["Kick", "Snare"], { 1: [0] }), files: soundsFor(["Kick", "Snare"]) } });

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const requestFrame = window.requestAnimationFrame.bind(window);
    const callbacks = new WeakSet<FrameRequestCallback>();
    let running: FrameRequestCallback | undefined;
    let pageCallbacks: Map<number, number> | undefined;
    const probe: GridProbe = {
      draws: 0,
      frames: 0,
      async waitFrames(count) {
        for (let frame = 0; frame < count; frame++) {
          await new Promise((resolve) => requestFrame(resolve));
        }
      },
      async countPageCallbacks() {
        const counts = new Map<number, number>();
        pageCallbacks = counts;
        try {
          let total = 0;
          let time = await new Promise<number>(requestFrame);
          for (let frame = 0; frame < 32; frame++) {
            const next = await new Promise<number>(requestFrame);
            // Count a frame only once it is over, regardless of our callback's order within it.
            total += counts.get(time) ?? 0;
            time = next;
          }
          return total;
        } finally {
          pageCallbacks = undefined;
        }
      },
      pixels(points) {
        const canvas = document.querySelector<HTMLCanvasElement>(".step-grid-canvas");
        const ctx = canvas?.getContext("2d");
        if (!canvas || !ctx) throw new Error("No step grid to sample");
        const bounds = canvas.getBoundingClientRect();
        return points.map(({ x, y }) => Array.from(ctx.getImageData(
          Math.round(x * canvas.width / bounds.width),
          Math.round(y * canvas.height / bounds.height),
          1, 1,
        ).data));
      },
    };
    window.stepGridProbe = probe;
    window.requestAnimationFrame = (callback) => requestFrame((time) => {
      if (pageCallbacks) pageCallbacks.set(time, (pageCallbacks.get(time) ?? 0) + 1);
      running = callback;
      try {
        callback(time);
      } finally {
        if (callbacks.has(callback)) probe.frames++;
        running = undefined;
      }
    });

    // Exercise the real 2D fallback so each grid draw has one observable clearRect call.
    const getContext = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, kind: string, options?: unknown) {
      if (this.classList.contains("step-grid-canvas")) {
        if (kind === "webgl2") return null;
        if (kind === "2d") return getContext.call(this, kind, { willReadFrequently: true });
      }
      return getContext.call(this, kind, options);
    } as typeof getContext;
    const clear = CanvasRenderingContext2D.prototype.clearRect;
    CanvasRenderingContext2D.prototype.clearRect = function (x, y, width, height) {
      if (this.canvas.classList.contains("step-grid-canvas")) {
        probe.draws++;
        if (running) callbacks.add(running);
      }
      clear.call(this, x, y, width, height);
    };
  });
});

const draws = (page: Page) => page.evaluate(() => window.stepGridProbe.draws);
const waitFrames = (page: Page, count: number) =>
  page.evaluate((n) => window.stepGridProbe.waitFrames(n), count);
const pixels = (page: Page, points: { x: number; y: number }[]) =>
  page.evaluate((points) => window.stepGridProbe.pixels(points), points);

async function settledGrid(page: Page) {
  await expect.poll(() => draws(page)).toBeGreaterThan(0);
  await waitFrames(page, 8);
}

test("the stopped grid runs zero callbacks over 32 quiet frames", async ({ app, mock }) => {
  await settledGrid(app);
  expect(await mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(false);
  const counts = await app.evaluate(async () => {
    const probe = window.stepGridProbe;
    // Confirm that the probe has already identified the callback that paints the grid.
    const identified = probe.frames > 0;
    const before = probe.frames;
    await probe.waitFrames(32);
    return { identified, quietCallbacks: probe.frames - before };
  });
  expect(counts).toEqual({ identified: true, quietCallbacks: 0 });
});

test("the stopped rack adds no page callbacks over 32 quiet frames", async ({ app, mock }) => {
  await settledGrid(app);
  expect(await mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(false);
  const countCallbacks = () => app.evaluate(() => window.stepGridProbe.countPageCallbacks());
  const withRack = await countCallbacks();

  await app.keyboard.press("F6");
  await expect(app.getByRole("region", { name: "Channel rack" })).toBeHidden();
  await waitFrames(app, 8);
  // The position readout and master meter run in both configurations.
  await expect.poll(countCallbacks, { message: "page callbacks with the rack hidden match the count with it shown" })
    .toBe(withRack);
});

test("live playback wakes the grid, follows the playhead and clears it in one draw", async ({ app, mock }) => {
  await settledGrid(app);
  const steps = [2, 9];
  const points = steps.map((step) => cellPoint(1, step));
  const stopped = await pixels(app, points);
  const columns = async () => {
    const now = await pixels(app, points);
    return steps.filter((_, i) => colourDistance(now[i], stopped[i]) > 20);
  };

  // Simulate a transport started elsewhere, with no grid focus or project changes to wake it.
  await mock.eval("(m) => { m.hold(490); m.state.transport.playing = true; }");
  await expect.poll(columns, { message: "playback wakes the stopped grid" }).toEqual([2]);
  await mock.eval("(m) => m.hold(2170)");
  await expect.poll(columns, { message: "the playhead advances to step 10" }).toEqual([9]);
  const beforeStop = await draws(app);
  await mock.eval("(m) => { m.state.transport.playing = false; }");
  await expect.poll(columns, { message: "the stopped frame clears the playhead" }).toEqual([]);
  await waitFrames(app, 8);
  expect(await draws(app)).toBe(beforeStop + 1);
});

test("toggling a step repaints the stopped grid", async ({ app, mock }) => {
  await settledGrid(app);
  const point = [cellPoint(0, 2)];
  const [off] = await pixels(app, point);
  await clickCell(app, 0, 2);
  await expect.poll(() => mock.view().then((view) => view.pattern.cells["1"][2])).toBe(0.8);
  await expect.poll(async () => colourDistance((await pixels(app, point))[0], off)).toBeGreaterThan(100);
});

async function dragVelocityPreview(app: Page) {
  await settledGrid(app);
  const box = await grid(app).boundingBox();
  if (!box) throw new Error("No step grid bounds");
  const cell = cellPoint(0, 0);
  const point = [{ x: cell.x, y: cell.y - 11 }];
  const readPixel = async () => (await pixels(app, point))[0];
  const before = await readPixel();
  await app.mouse.move(box.x + cell.x, box.y + cell.y);
  await app.mouse.down();
  // Let the focus and press paint before moving, so they cannot mask a missing drag wake.
  await waitFrames(app, 8);
  await app.mouse.move(box.x + cell.x, box.y + cell.y - 30);
  await expect.poll(async () => colourDistance(await readPixel(), before), { message: "velocity preview changes the step pixel" })
    .toBeGreaterThan(100);
  return { before, readPixel };
}

test("a velocity drag repaints before committing a stopped step edit", async ({ app, mock }) => {
  try {
    await dragVelocityPreview(app);
    expect(await mock.calls("project_edit")).toHaveLength(0);
  } finally {
    await app.mouse.up();
  }
});

for (const cancellation of ["pointercancel", "cancelled Save As"] as const) {
  test(`${cancellation} repaints the stopped step without committing the drag`, async ({ app, mock }) => {
    expect(await mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(false);
    const velocity = (await mock.view()).pattern.cells["1"][0];
    try {
      const { before, readPixel } = await dragVelocityPreview(app);
      if (cancellation === "pointercancel") {
        await grid(app).dispatchEvent("pointercancel", { pointerId: 1, bubbles: true });
      } else {
        await mock.eval("(m) => m.queueDialog(null)");
        await app.keyboard.press("ControlOrMeta+Shift+s");
        await expect.poll(async () => (await mock.calls("plugin:dialog|save")).length).toBe(1);
        expect(await mock.calls("project_save_as")).toHaveLength(0);
      }
      await expect.poll(readPixel, { message: "cancelling restores the pre-drag pixel" }).toEqual(before);
    } finally {
      await app.mouse.up();
    }
    await waitFrames(app, 8);
    expect(await mock.calls("project_edit")).toHaveLength(0);
    expect((await mock.view()).pattern.cells["1"][0]).toBe(velocity);
  });
}

test("a probe theme repaints the stopped grid", async ({ app }) => {
  await settledGrid(app);
  const ink = [24, 241, 51];
  const point = [cellPoint(0, 0)];
  expect(colourDistance((await pixels(app, point))[0], ink)).toBeGreaterThan(80);
  await app.addStyleTag({ content: `:root[data-theme="probe"] { --ink-1: rgb(${ink.join(", ")}); }` });
  await app.evaluate(() => { document.documentElement.dataset.theme = "probe"; });
  await expect.poll(async () => colourDistance((await pixels(app, point))[0], ink)).toBeLessThan(8);
});
