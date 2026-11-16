import { expect, test as base, type Locator, type Page } from "@playwright/test";
import { installMockTauri, type MockChannel, type MockProject, type MockSeed } from "./mockTauri";

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

/** Generated sound files for every channel name, so nothing shows as missing. */
export function soundsFor(names: string[]): MockSeed["files"] {
  return Object.fromEntries(names.map((n) => [`/qa/sounds/${n}.wav`, { kind: "sample" as const }]));
}

export interface Mock {
  /** Every command the UI sent, in order. */
  calls: (cmd?: string) => Promise<{ cmd: string; args: Record<string, unknown> }[]>;
  view: () => Promise<{ dirty: boolean; path: string | null; bpm: number; pattern: { steps: number; swing: number; cells: Record<string, (number | null)[]> }; channels: { id: number; name: string; volume: number; mute: boolean }[] }>;
  eval: <T>(fn: string) => Promise<T>;
}

/** The page with the mock shell loaded and every console error and page error collected. */
export const test = base.extend<{ seed: MockSeed; app: Page; mock: Mock }>({
  seed: [{}, { option: true }],
  app: async ({ page, seed }, use) => {
    const consoleErrors: string[] = [];
    page.on("console", (m) => {
      // Headless Chromium's own GPU notes ("GPU stall due to ReadPixels" while it composites):
      // the app never reads pixels back.
      if (/GL Driver Message \(OpenGL, Performance/.test(m.text())) return;
      if (m.type() === "error" || m.type() === "warning") consoleErrors.push(`${m.type()}: ${m.text()}`);
    });
    page.on("pageerror", (e) => consoleErrors.push(`pageerror: ${e.message}`));
    await page.addInitScript(installMockTauri, seed);
    await page.goto("/");
    await expect(page.getByRole("button", { name: "Stop" })).toBeVisible();
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
