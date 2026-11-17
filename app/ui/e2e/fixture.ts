import { expect, test as base, type Locator, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { keepWebglFrames } from "./webglProbe";
import { installMockTauri, type MockChannel, type MockProject, type MockSeed } from "./mockTauri";

/** The sampler's parameter ids, ranges and defaults, as the engine has them (the fixture the
 * Rust tests keep in step with `grooviq_dsp::sampler`). */
const DEVICE_PARAMS = JSON.parse(
  readFileSync(new URL("../../../tests/fixtures/device-params.json", import.meta.url), "utf8"),
) as { sampler: MockSeed["samplerParams"]; synth: MockSeed["synthParams"] };
const SAMPLER_PARAMS = DEVICE_PARAMS.sampler;
/** The synth's parameter ids, ranges and defaults, in the engine's order. */
export const SYNTH_PARAMS = DEVICE_PARAMS.synth!;
/** Every synth factory preset's 40 values in SPEC order (dsp/tests/params_fixture.rs writes it). */
export const SYNTH_PRESETS = JSON.parse(
  readFileSync(new URL("../../../tests/fixtures/synth-presets.json", import.meta.url), "utf8"),
) as Record<string, number[]>;
/** A preset's values by parameter id. */
export const presetValues = (name: string): Record<string, number> =>
  Object.fromEntries(SYNTH_PARAMS.map((p, i) => [p.id, SYNTH_PRESETS[name][i]]));

// Grid geometry (src/rack/gridModel.ts): 28 px cells, 6 px gaps, 4 px edge.
const CELL = 28;
const GAP = 6;
const EDGE = 4;

/** A project with these channels (named after their sample files) and hits. */
export function project(
  names: string[],
  hits: Record<number, number[]> = {},
  extra: Partial<MockProject> = {},
): MockProject {
  const channels: MockChannel[] = names.map((name, i) => ({
    id: i + 1,
    name,
    color: i % 6,
    sample: `/qa/sounds/${name}.wav`,
    embedded: true,
    volume: 1,
    pan: 0,
    mute: false,
    solo: false,
    sampler: {},
  }));
  const notes = Object.entries(hits).flatMap(([id, steps]) =>
    steps.map((step) => ({ channel: Number(id), tick: step * 240, velocity: 0.8 })),
  );
  return {
    name: "QA beat",
    bpm: 120,
    timeSignature: { beats: 4, unit: 4 },
    channels,
    pattern: { id: 0, name: "Pattern 1", steps: 16, swing: 0, notes },
    nextId: channels.length + 1,
    ...extra,
  };
}

/** Sound files for every channel name, for adding or relinking them. */
export function soundsFor(names: string[]): MockSeed["files"] {
  return Object.fromEntries(names.map((n) => [`/qa/sounds/${n}.wav`, { kind: "sample" as const }]));
}

export interface Mock {
  /** Every command the UI sent, in order. */
  calls: (cmd?: string) => Promise<{ cmd: string; args: Record<string, unknown> }[]>;
  view: () => Promise<{
    dirty: boolean;
    path: string | null;
    bpm: number;
    recording: boolean;
    notesRevision: number;
    generation: number;
    canUndo: boolean;
    canRedo: boolean;
    pattern: { steps: number; swing: number; cells: Record<string, (number | null)[]> };
    channels: { id: number; name: string; volume: number; mute: boolean; kind: string; preset: string | null; roll: boolean }[];
  }>;
  eval: <T>(fn: string) => Promise<T>;
}

/** Workspace selects its initial channel in a mount effect. A subsequent frame lets the
 * same effect flush finish installing keyboard shortcuts, including for an empty project. */
export async function workspaceReady(page: Page) {
  await expect(page.getByRole("region", { name: "Channel rack" })).toBeVisible();
  await expect.poll(() => page.evaluate(() =>
    (window as unknown as { __grooviqMock: { calls: { cmd: string }[] } }).__grooviqMock.calls.some((c) => c.cmd === "channel_select"),
  )).toBe(true);
  await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => resolve())));
}

/** The page with the mock shell loaded and every console error and page error collected. */
export const test = base.extend<{ seed: MockSeed; keepFrames: boolean; app: Page; mock: Mock }>({
  seed: [{}, { option: true }],
  // Retain WebGL frames (see webglProbe.ts). Off only where the shipping buffer lifetime is the subject.
  keepFrames: [true, { option: true }],
  app: async ({ page, seed, keepFrames }, use) => {
    const consoleErrors: string[] = [];
    page.on("console", (m) => {
      // Headless Chromium's own GPU notes ("GPU stall due to ReadPixels" while it composites):
      // the app never reads pixels back.
      if (/GL Driver Message \(OpenGL, Performance/.test(m.text())) return;
      if (m.type() === "error" || m.type() === "warning") consoleErrors.push(`${m.type()}: ${m.text()}`);
    });
    page.on("pageerror", (e) => consoleErrors.push(`pageerror: ${e.message}`));
    if (keepFrames) await page.addInitScript(keepWebglFrames);
    await page.addInitScript(installMockTauri, { samplerParams: SAMPLER_PARAMS, synthParams: SYNTH_PARAMS, synthPresets: SYNTH_PRESETS, ...seed });
    await page.goto("/");
    await expect(page.getByRole("button", { name: "Stop" })).toBeVisible();
    await workspaceReady(page);
    await use(page);
    expect(consoleErrors, "console errors and warnings").toEqual([]);
  },
  mock: async ({ app }, use) => {
    const evalIn = <T>(fn: string) => app.evaluate(`(${fn})(window.__grooviqMock)`) as Promise<T>;
    await use({
      calls: async (cmd) => {
        const all = await evalIn<{ cmd: string; args: Record<string, unknown> }[]>("(m) => m.calls");
        return cmd ? all.filter((c) => c.cmd === cmd) : all;
      },
      view: () => evalIn("(m) => m.view()"),
      eval: evalIn,
    });
  },
});

export { expect };

/** The step grid canvas. */
export const grid = (page: Page): Locator => page.getByLabel("Step grid");

/** The centre of a grid cell, relative to the canvas. */
export function cellPoint(row: number, step: number) {
  return { x: EDGE + step * (CELL + GAP) + CELL / 2, y: EDGE + row * (CELL + GAP) + CELL / 2 };
}

/** Clicks a grid cell with the left (default) or right button. */
export async function clickCell(page: Page, row: number, step: number, button: "left" | "right" = "left") {
  await grid(page).click({ position: cellPoint(row, step), button });
}

/** The position readout ("bar.beat.step"). */
export const position = (page: Page) => page.getByRole("timer", { name: "Position" });

/** RGBA at points (CSS pixels, relative to the element) of what the element shows on screen.
 * WebGL canvases are read through a screenshot; `keepWebglFrames` (see webglProbe.ts) makes that
 * deterministic on Linux WebKit. */
export async function pixelsAt(page: Page, el: Locator, points: { x: number; y: number }[]) {
  const png = (await el.screenshot({ scale: "css" })).toString("base64");
  return page.evaluate(
    async ({ png, points }) => {
      const img = new Image();
      img.src = `data:image/png;base64,${png}`;
      await img.decode();
      const c = document.createElement("canvas");
      c.width = img.width;
      c.height = img.height;
      const ctx = c.getContext("2d", { willReadFrequently: true });
      if (!ctx) throw new Error("no 2D context");
      ctx.drawImage(img, 0, 0);
      return points.map(({ x, y }) => Array.from(ctx.getImageData(Math.round(x), Math.round(y), 1, 1).data));
    },
    { png, points },
  );
}

/** Distance between two colours (RGB, 0..441). */
export const colourDistance = (a: number[], b: number[]) => Math.hypot(a[0] - b[0], a[1] - b[1], a[2] - b[2]);

/** Brightness, 0..255. */
export const brightness = (c: number[]) => (c[0] + c[1] + c[2]) / 3;
