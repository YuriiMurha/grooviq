// M2 Task 11: the piano roll. Assertions read the mock session's command log and notes, and the
// roll's own counters (`window.__grooviqRoll`, outside production builds).
import type { Page, TestInfo } from "@playwright/test";
import { colourDistance, expect, pixelsAt, project, test, type Mock } from "./fixture";
import type { MockNote, MockProject } from "./mockTauri";
import { PROD_URL } from "./ports";
import { settled } from "./rollHelpers";

interface RollStats {
  bufferBuilds: number;
  draws: number;
  renderer: string;
  view: { tickPx: number; keyPx: number; scrollTick: number; topKey: number };
}

/** One synth channel, "Bass" (id 1, the fourth ink), holding these notes. */
function bass(notes: MockNote[]): MockProject {
  const p = project([]);
  p.channels.push({ id: 1, name: "Bass", color: 3, sample: null, volume: 1, pan: 0, mute: false, solo: false, sampler: {}, kind: "synth", preset: "Init" });
  p.nextId = 2;
  p.pattern.notes = notes;
  return p;
}

const stats = (page: Page) => page.evaluate(() => (window as unknown as { __grooviqRoll: RollStats }).__grooviqRoll);
const canvas = (page: Page) => page.getByLabel("Piano roll notes");
const keys = (page: Page) => page.getByLabel("Piano keys");
const tool = (page: Page, name: string) => page.getByRole("region", { name: "Piano roll" }).getByRole("button", { name, exact: true });
const notes = (mock: Mock) =>
  mock.eval<Required<MockNote>[]>("(m) => m.notes()").then((ns) =>
    ns.map((n) => [n.tick, n.length, n.key]).sort((a, b) => a[2] - b[2] || a[0] - b[0]),
  );
const nextFrame = (page: Page) => page.evaluate(() => new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r()))));

/** The page point over (tick, key row), in the middle of the row. */
async function at(page: Page, tick: number, key: number) {
  const v = (await stats(page)).view;
  const box = (await canvas(page).boundingBox())!;
  return { x: box.x + (tick - v.scrollTick) * v.tickPx, y: box.y + (v.topKey - key - 0.5) * v.keyPx };
}

/** The point relative to the canvas, for pixel reads. */
async function local(page: Page, tick: number, key: number) {
  const p = await at(page, tick, key);
  const box = (await canvas(page).boundingBox())!;
  return { x: p.x - box.x, y: p.y - box.y };
}

async function drag(page: Page, from: { x: number; y: number }, to: { x: number; y: number }, button: "left" | "right" = "left") {
  await page.mouse.move(from.x, from.y);
  await page.mouse.down({ button });
  await page.mouse.move((from.x + to.x) / 2, (from.y + to.y) / 2, { steps: 4 });
  await page.mouse.move(to.x, to.y, { steps: 4 });
  await page.mouse.up({ button });
}

async function waitForInitialDraw(page: Page, timeout?: number) {
  const info = test.info();
  // Readiness is correctness, not a five-second performance gate. Reserve half the
  // case budget for editing and its assertions; a stalled draw fails here.
  const budget = timeout ?? Math.max(1, Math.floor(info.timeout / 2));
  await expect.poll(async () => (await stats(page))?.draws ?? 0, {
    timeout: budget,
    message: "piano roll must draw before editing",
  }).toBeGreaterThan(0);
}

/** F7 opens the roll on the selected channel; F6 hides the rack to give it the height. */
async function openRoll(page: Page, drawTimeout?: number) {
  await page.keyboard.press("F7");
  await page.keyboard.press("F6");
  await expect(canvas(page)).toBeVisible();
  await waitForInitialDraw(page, drawTimeout);
  await settled(page);
}

async function shot(page: Page, info: TestInfo, name: string) {
  await info.attach(`${name}-aria`, { body: await page.locator("body").ariaSnapshot(), contentType: "text/yaml" });
  await page.screenshot({ path: info.outputPath(`${name}.png`) });
}

/** The ink of the fourth channel colour, as RGB. */
const ink = (page: Page) =>
  page.evaluate(() => {
    const hex = getComputedStyle(document.documentElement).getPropertyValue("--ink-4").trim();
    return [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
  });

test.describe("initial draw readiness", () => {
  test.use({ seed: { project: bass([]) } });

  for (const state of ["missing", "zero"] as const) {
    test(`a_${state}_draw_observation_blocks_editing`, async ({ app, mock }) => {
      await openRoll(app);
      await app.keyboard.press("F7");
      await app.keyboard.press("F6");
      const restore = await app.evaluate((state) => {
        const w = window as unknown as { __grooviqRoll?: RollStats; __savedRoll?: RollStats };
        w.__savedRoll = w.__grooviqRoll;
        w.__grooviqRoll = state === "missing" ? undefined : { ...w.__grooviqRoll!, draws: 0 };
        return !!w.__savedRoll;
      }, state);
      expect(restore).toBe(true);
      try {
        await expect(openRoll(app, 300)).rejects.toThrow(/piano roll must draw before editing/);
        expect(await mock.calls("notes_edit")).toHaveLength(0);
      } finally {
        await app.evaluate(() => {
          const w = window as unknown as { __grooviqRoll?: RollStats; __savedRoll?: RollStats };
          w.__grooviqRoll = w.__savedRoll;
          delete w.__savedRoll;
        });
      }
      await waitForInitialDraw(app);
    });
  }

  test("a_ready_browser_result_can_arrive_after_the_default_poll_deadline", async ({ app }) => {
    const evaluate = app.evaluate.bind(app);
    let delayed = false;
    const controlled = new Proxy(app, {
      get(target, property) {
        if (property !== "evaluate") return Reflect.get(target, property, target);
        return async (...args: Parameters<Page["evaluate"]>) => {
          const result = await evaluate(...args);
          if (!delayed && String(args[0]).includes("__grooviqRoll") && ((result as RollStats | undefined)?.draws ?? 0) > 0) {
            delayed = true;
            await new Promise((resolve) => setTimeout(resolve, 5500));
          }
          return result;
        };
      },
    });
    await openRoll(controlled);
    expect(delayed).toBe(true);
    expect((await stats(app)).draws).toBeGreaterThan(0);
  });
});

test.describe("ten thousand notes", () => {
  // A bounded correctness deadline, including fixture setup; not a performance gate.
  test.describe.configure({ timeout: 120_000 });
  // Ten keys, one bar, ten thousand notes: every one of them inside one box on screen.
  const many: MockNote[] = Array.from({ length: 10_000 }, (_, i) => ({
    id: i + 1,
    channel: 1,
    tick: (i * 37) % 3600,
    length: 240,
    key: 60 + (i % 10),
    velocity: 0.8,
  }));
  test.use({ seed: { project: bass(many) } });

  test("moving_ten_thousand_selected_notes_commits_once", async ({ app, mock }, info) => {
    const before = await test.step("setup and select all 10,000 notes", async () => {
      await openRoll(app);
      await tool(app, "Select").click();
      // A box from above the notes (an empty row) to below and past them selects all of them.
      await drag(app, await at(app, 30, 72), await at(app, 3960, 58));
      await expect.poll(async () => (await stats(app)).bufferBuilds).toBeGreaterThan(0);
      return mock.eval<Required<MockNote>[]>("(m) => m.notes()");
    });

    const atEnd = await test.step("UI move without committing or rebuilding", async () => {
      // Grab a note and drag the whole selection two rows up and two beats on.
      const from = await at(app, 1000, 65);
      await app.mouse.move(from.x, from.y);
      await app.mouse.down();
      await nextFrame(app);
      const atDown = await stats(app);
      for (let i = 1; i <= 8; i++) {
        await app.mouse.move(from.x + i * 6, from.y - (i * 28) / 8);
        await nextFrame(app);
      }
      const atEnd = await stats(app);
      await shot(app, info, "dragging-ten-thousand");
      // The drag is drawn every frame from the same instance buffer: nothing was rebuilt.
      expect(atEnd.bufferBuilds).toBe(atDown.bufferBuilds);
      expect(atEnd.draws).toBeGreaterThan(atDown.draws);
      expect(await mock.eval<number>('(m) => m.calls.filter((c) => c.cmd === "notes_edit").length')).toBe(0);
      return atEnd;
    });
    await test.step("release and settle one commit", async () => {
      await app.mouse.up();

      // One edit at release, with every note in it.
      await expect.poll(() => mock.eval<number>('(m) => m.calls.filter((c) => c.cmd === "notes_edit").length')).toBe(1);
    });
    await test.step("assert complete moved data and rebuilt buffer", async () => {
      const [edit] = await mock.calls("notes_edit");
      const sent = edit.args.edit as { update: unknown[]; add: unknown[]; delete: unknown[] };
      expect([sent.update.length, sent.add.length, sent.delete.length]).toEqual([10_000, 0, 0]);
      // Every note moved by the same delta: the grabbed note's start snapped to the step grid.
      const after = new Map((await mock.eval<Required<MockNote>[]>("(m) => m.notes()")).map((n) => [n.id, n]));
      expect(before).toHaveLength(10_000);
      expect(after.size).toBe(10_000);
      const dTick = after.get(before[0].id)!.tick - before[0].tick;
      expect(dTick).toBeGreaterThan(0);
      const off = before.filter((n) => {
        const m = after.get(n.id)!;
        return m.tick - n.tick !== dTick || m.key - n.key !== 2 || m.length !== 240 || m.channel !== n.channel || m.velocity !== Math.fround(n.velocity) || m.slide !== n.slide;
      });
      expect(off.length, "every note retains its fields and moves by the same delta").toBe(0);
      await expect.poll(async () => (await stats(app)).bufferBuilds).toBeGreaterThan(atEnd.bufferBuilds);
    });
  });
});

test.describe("editing", () => {
  test.use({ seed: { project: bass([]) } });

  test("draw_paint_select_resize_slice_and_delete_reach_the_session", async ({ app, mock }, info) => {
    await openRoll(app);
    await shot(app, info, "empty-roll");
    // Draw (the default tool): a click puts a step-long note on the grid.
    await expect(tool(app, "Draw")).toHaveAttribute("aria-pressed", "true");
    const p = await at(app, 970, 60);
    await app.mouse.click(p.x, p.y);
    await expect.poll(() => notes(mock)).toEqual([[960, 240, 60]]);

    // Resize from its right edge to a step past 1600: the end snaps to 1680.
    await drag(app, await at(app, 1197, 60), await at(app, 1650, 60));
    await expect.poll(() => notes(mock)).toEqual([[960, 720, 60]]);

    // Slice it at 1440.
    await tool(app, "Slice").click();
    const cut = await at(app, 1445, 60);
    await app.mouse.click(cut.x, cut.y);
    await expect.poll(() => notes(mock)).toEqual([
      [960, 480, 60],
      [1440, 240, 60],
    ]);

    // Paint four steps along row 64.
    await tool(app, "Paint").click();
    await drag(app, await at(app, 10, 64), await at(app, 3 * 240 + 10, 64));
    await expect.poll(() => notes(mock)).toEqual([
      [960, 480, 60],
      [1440, 240, 60],
      [0, 240, 64],
      [240, 240, 64],
      [480, 240, 64],
      [720, 240, 64],
    ]);
    await shot(app, info, "painted");

    // Select them with a box, then move them two rows up and two steps on.
    await tool(app, "Select").click();
    await drag(app, await at(app, 10, 66), await at(app, 900, 63));
    await drag(app, await at(app, 300, 64), await at(app, 300 + 480, 66));
    await expect.poll(() => notes(mock)).toEqual([
      [960, 480, 60],
      [1440, 240, 60],
      [480, 240, 66],
      [720, 240, 66],
      [960, 240, 66],
      [1200, 240, 66],
    ]);

    // Right-click deletes in any tool; the delete tool deletes with a click.
    const r = await at(app, 1000, 60);
    await app.mouse.click(r.x, r.y, { button: "right" });
    await expect.poll(() => notes(mock).then((n) => n.length)).toBe(5);
    await tool(app, "Delete").click();
    const d = await at(app, 1500, 60);
    await app.mouse.click(d.x, d.y);
    await expect.poll(() => notes(mock)).toEqual([
      [480, 240, 66],
      [720, 240, 66],
      [960, 240, 66],
      [1200, 240, 66],
    ]);
    await shot(app, info, "edited");
    // Each gesture was one edit: draw, resize, slice, the paint stroke's cells, the move and two deletes.
    const edits = (await mock.calls("notes_edit")).map((c) => c.args.edit as { add: unknown[]; update: unknown[]; delete: unknown[] });
    expect(edits.filter((e) => e.update.length === 4)).toHaveLength(1);
    expect(edits.filter((e) => e.delete.length === 1)).toHaveLength(2);
  });

  test("the_key_column_plays_and_releases_the_channel", async ({ app, mock }) => {
    await openRoll(app);
    const v = (await stats(app)).view;
    const box = (await keys(app).boundingBox())!;
    const y = (key: number) => box.y + (v.topKey - key - 0.5) * v.keyPx;
    const x = box.x + box.width * 0.8;
    const live = () => mock.eval<string[]>("(m) => m.live().sort()");
    await app.mouse.move(x, y(60));
    await app.mouse.down();
    await expect.poll(live).toEqual(["1:60"]);
    // Sliding down the keys plays each one and lets the last one go.
    await app.mouse.move(x, y(57), { steps: 3 });
    await expect.poll(live).toEqual(["1:57"]);
    await app.mouse.up();
    await expect.poll(live).toEqual([]);
    const calls = (await mock.calls("channel_note")).map((c) => [c.args.key, c.args.on]);
    expect(calls[0]).toEqual([60, true]);
    expect(calls.at(-1)).toEqual([57, false]);
    // Every note-on has its note-off.
    const on = calls.filter((c) => c[1]).map((c) => c[0]);
    const off = calls.filter((c) => !c[1]).map((c) => c[0]);
    expect(off.sort()).toEqual(on.sort());
  });
});

test.describe("held keys", () => {
  test.use({ seed: { project: bass([]) } });

  test("the_key_column_lets_go_on_blur_and_on_stop", async ({ app, mock }) => {
    await openRoll(app);
    const v = (await stats(app)).view;
    const box = (await keys(app).boundingBox())!;
    const point = (key: number) => ({ x: box.x + box.width * 0.8, y: box.y + (v.topKey - key - 0.5) * v.keyPx });
    const live = () => mock.eval<string[]>("(m) => m.live().sort()");
    const offs = async () => (await mock.calls("channel_note")).filter((c) => !c.args.on).map((c) => c.args.key);

    // The window loses focus with C4 held: it stops, and the later pointer-up sends nothing more.
    await app.mouse.move(point(60).x, point(60).y);
    await app.mouse.down();
    await expect.poll(live).toEqual(["1:60"]);
    await app.evaluate(() => window.dispatchEvent(new Event("blur")));
    await expect.poll(live).toEqual([]);
    await app.mouse.up();
    await app.evaluate(() => new Promise((r) => setTimeout(r, 100)));
    expect(await offs()).toEqual([60]);

    // Playing, then stopped (Space) with D4 held: it stops too.
    await app.keyboard.press("Space");
    await expect.poll(() => mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(true);
    await app.mouse.move(point(62).x, point(62).y);
    await app.mouse.down();
    await expect.poll(live).toEqual(["1:62"]);
    await app.keyboard.press("Space");
    await expect.poll(() => mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(false);
    await expect.poll(live).toEqual([]);
    await app.mouse.up();
    await app.evaluate(() => new Promise((r) => setTimeout(r, 100)));
    expect(await offs()).toEqual([60, 62]);
  });
});

test.describe("drag previews", () => {
  // Two notes at full velocity: a step at C4 and a step at D4 two steps later.
  test.use({
    seed: {
      project: bass([
        { id: 1, channel: 1, tick: 0, length: 240, key: 60, velocity: 1 },
        { id: 2, channel: 1, tick: 480, length: 240, key: 62, velocity: 1 },
      ]),
    },
  });

  /** Whether each (tick, key) shows the channel's ink. */
  const inked = async (page: Page, points: [number, number][]) => {
    const want = await ink(page);
    const px = await pixelsAt(page, canvas(page), await Promise.all(points.map(([t, k]) => local(page, t, k))));
    return px.map((c) => colourDistance(c, want) < 40);
  };

  test("a_held_drag_moves_and_resizes_the_notes_on_screen_before_release", async ({ app, mock }, info) => {
    await openRoll(app);
    await tool(app, "Select").click();
    await drag(app, await at(app, 10, 64), await at(app, 900, 59));
    await nextFrame(app);
    const builds = (await stats(app)).bufferBuilds;

    // Grab C4 and hold it four steps on and three rows up (snapped from 950 ticks).
    const from = await at(app, 120, 60);
    await app.mouse.move(from.x, from.y);
    await app.mouse.down();
    const to = await at(app, 120 + 950, 63);
    await app.mouse.move(to.x, to.y, { steps: 5 });
    await nextFrame(app);
    await shot(app, info, "held-drag");
    // Before release: drawn at the new places only, from the same buffer, nothing sent.
    expect(await inked(app, [[120, 60], [600, 62], [1080, 63], [1560, 65]])).toEqual([false, false, true, true]);
    expect((await stats(app)).bufferBuilds).toBe(builds);
    expect(await mock.calls("notes_edit")).toEqual([]);
    // Back past the start and below key 0's reach: the group stops at tick 0, keeping its shape.
    const back = await at(app, 120 - 2000, 61);
    await app.mouse.move(back.x, back.y, { steps: 5 });
    await nextFrame(app);
    expect(await inked(app, [[120, 61], [600, 63], [120, 60], [1080, 63]])).toEqual([true, true, false, false]);
    await app.mouse.move(to.x, to.y, { steps: 5 });
    await app.mouse.up();
    await expect.poll(() => notes(mock)).toEqual([
      [960, 240, 63],
      [1440, 240, 65],
    ]);
    await nextFrame(app);
    expect(await inked(app, [[1080, 63], [1560, 65], [120, 60]])).toEqual([true, true, false]);

    // Resize both from C4's right edge, held two steps longer: the preview is what is committed.
    const edge = await at(app, 1197, 63);
    await app.mouse.move(edge.x, edge.y);
    await app.mouse.down();
    const longer = await at(app, 1197 + 480, 63);
    await app.mouse.move(longer.x, longer.y, { steps: 5 });
    await nextFrame(app);
    expect(await inked(app, [[1500, 63], [1980, 65], [1740, 63]])).toEqual([true, true, false]);
    expect(await mock.calls("notes_edit")).toHaveLength(1);
    await app.mouse.up();
    await expect.poll(() => notes(mock)).toEqual([
      [960, 720, 63],
      [1440, 720, 65],
    ]);
    await nextFrame(app);
    expect(await inked(app, [[1500, 63], [1980, 65], [1740, 63]])).toEqual([true, true, false]);
  });
});

test.describe("renderers", () => {
  test.use({ seed: { project: bass([{ id: 1, channel: 1, tick: 0, length: 960, key: 60, velocity: 1 }]) } });

  test("a_lost_context_is_rebuilt", async ({ app }, info) => {
    await openRoll(app);
    const before = await stats(app);
    expect(before.renderer).toBe("webgl2");
    const restored = await canvas(app).evaluate(async (c: HTMLCanvasElement) => {
      const gl = c.getContext("webgl2");
      const ext = gl?.getExtension("WEBGL_lose_context");
      if (!ext) return null;
      const lost = new Promise<void>((resolve) => {
        c.addEventListener("webglcontextlost", () => {
          resolve();
        }, { once: true });
      });
      // Capture runs before the renderer's restoration listener. Counters copied here
      // cannot be satisfied by an upload or draw from before restoration.
      const rebuilt = new Promise<{ bufferBuilds: number; draws: number }>((resolve) => {
        c.addEventListener("webglcontextrestored", () => {
          const { bufferBuilds, draws } = (window as unknown as { __grooviqRoll: RollStats }).__grooviqRoll;
          resolve({ bufferBuilds, draws });
        }, { once: true, capture: true });
      });
      ext.loseContext();
      await lost;
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      ext.restoreContext();
      return await rebuilt;
    });
    test.skip(!restored, "no WEBGL_lose_context here");
    await expect.poll(async () => (await stats(app)).bufferBuilds).toBeGreaterThan(restored!.bufferBuilds);
    await expect.poll(async () => (await stats(app)).draws).toBeGreaterThan(restored!.draws);
    expect((await stats(app)).renderer).toBe("webgl2");
    // The note is on screen again, in its channel's ink.
    const [onNote, offNote] = await pixelsAt(app, canvas(app), [await local(app, 480, 60), await local(app, 480, 66)]);
    await shot(app, info, "restored");
    expect(colourDistance(onNote, await ink(app))).toBeLessThan(40);
    expect(colourDistance(offNote, await ink(app))).toBeGreaterThan(100);
  });

  test("canvas_2d_is_the_fallback_without_webgl2", async ({ app, mock }, info) => {
    await app.addInitScript(() => {
      const get = HTMLCanvasElement.prototype.getContext;
      HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, type: string, ...rest: unknown[]) {
        return type === "webgl2" ? null : (get as (...a: unknown[]) => unknown).call(this, type, ...rest);
      } as typeof get;
    });
    await app.reload();
    await expect(app.getByRole("button", { name: "Stop", exact: true })).toBeVisible();
    await openRoll(app);
    expect((await stats(app)).renderer).toBe("canvas2d");
    const [onNote, offNote] = await pixelsAt(app, canvas(app), [await local(app, 480, 60), await local(app, 480, 66)]);
    await shot(app, info, "canvas-2d");
    expect(colourDistance(onNote, await ink(app))).toBeLessThan(40);
    expect(colourDistance(offNote, await ink(app))).toBeGreaterThan(100);
    // It edits as well.
    const p = await at(app, 1930, 62);
    await app.mouse.click(p.x, p.y);
    await expect.poll(() => notes(mock)).toEqual([
      [0, 960, 60],
      [1920, 240, 62],
    ]);
  });
});

test.describe("the production build", () => {
  // What ships: no counters on `window`, so the renderer is read off the canvas itself.
  // No retention shim: the buffer lifetime must be the one that ships.
  test.use({ baseURL: PROD_URL, keepFrames: false, seed: { project: bass([{ id: 1, channel: 1, tick: 0, length: 960, key: 60, velocity: 1 }]) } });

  test("the_production_build_draws_with_webgl2", async ({ app }) => {
    const webgl2 = await app.evaluate(() => {
      const c = document.createElement("canvas");
      return c.getContext("webgl2") !== null;
    });
    test.skip(!webgl2, "no WebGL2 in this browser");
    await app.keyboard.press("F7");
    await app.keyboard.press("F6");
    await expect(canvas(app)).toBeVisible();
    expect(await app.evaluate(() => "__grooviqRoll" in window)).toBe(false);
    // Give a fallback the time it would take (a microtask, a render).
    await nextFrame(app);
    await nextFrame(app);
    // A canvas holding a 2D context answers null for WebGL2, and the reverse.
    const kind = await canvas(app).evaluate((c: HTMLCanvasElement) => ({
      webgl2: c.getContext("webgl2") !== null,
      canvas2d: c.getContext("2d") !== null,
      preserveDrawingBuffer: c.getContext("webgl2")?.getContextAttributes()?.preserveDrawingBuffer,
    }));
    expect(kind).toEqual({ webgl2: true, canvas2d: false, preserveDrawingBuffer: false });
  });
});
