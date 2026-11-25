import type { Locator, Page } from "@playwright/test";
import { colourDistance, expect, presetValues, project, test } from "./fixture";

const synthChannel = (id: number, name: string) => ({
  id,
  name,
  color: 3,
  sample: null,
  volume: 1,
  pan: 0,
  mute: false,
  solo: false,
  sampler: {},
  kind: "synth" as const,
  synth: presetValues("808 Glide"),
  preset: "808 Glide",
});

const synthProject = project([], {}, { channels: [synthChannel(1, "Bass")], nextId: 2 });

test.use({ seed: { project: synthProject } });

async function openScope(page: Page, name = "Bass") {
  await page.getByRole("button", { name: `Edit the ${name} synth` }).click();
  const scope = page.getByRole("dialog", { name: `${name} synth` }).getByLabel("Synth output waveform");
  await expect(scope).toBeVisible();
  return scope;
}

test("the synth output scope keeps drawing without per-frame computed-style reads", async ({ app }) => {
  const scope = await openScope(app);
  const { reads, strokes } = await scope.evaluate(async (canvas: HTMLCanvasElement) => {
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("No synth output context");
    const getComputedStyle = window.getComputedStyle;
    const stroke = ctx.stroke;
    let reads = 0;
    let strokes = 0;
    window.getComputedStyle = (element, pseudo) => {
      if (element === canvas) reads++;
      return getComputedStyle.call(window, element, pseudo);
    };
    ctx.stroke = (...args: [] | [Path2D]) => {
      strokes++;
      Reflect.apply(stroke, ctx, args);
    };
    try {
      // Allow for the probe starting partway through the first frame.
      for (let frame = 0; frame < 32; frame++) {
        await new Promise(requestAnimationFrame);
      }
      return { reads, strokes };
    } finally {
      window.getComputedStyle = getComputedStyle;
      ctx.stroke = stroke;
    }
  });
  expect(strokes, "the scope drew across at least 30 animation frames").toBeGreaterThanOrEqual(30);
  expect(reads, "scope style reads stay bounded while drawing").toBeLessThanOrEqual(1);
});

const probeColour = [24, 241, 51];

const setTheme = (page: Page, theme: string | null) => page.evaluate((value) => {
  if (value === null) delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = value;
}, theme);

const strokePixel = (scope: Locator) => scope.evaluate((canvas: HTMLCanvasElement) => {
  // The silent trace is centred between the two middle rows. Pick the more opaque pixel.
  const sample = document.createElement("canvas");
  sample.width = 1;
  sample.height = 2;
  const ctx = sample.getContext("2d", { willReadFrequently: true });
  if (!ctx) throw new Error("No pixel sampling context");
  ctx.drawImage(canvas, canvas.width / 2, canvas.height / 2 - 1, 1, 2, 0, 0, 1, 2);
  const pixels = ctx.getImageData(0, 0, 1, 2).data;
  return Array.from(pixels[3] >= pixels[7] ? pixels.slice(0, 4) : pixels.slice(4));
});

test("the existing synth output canvas repaints and restores its stroke on live theme changes", async ({ app }) => {
  const scope = await openScope(app);
  const originalNode = await scope.elementHandle();
  const originalTheme = await app.locator("html").getAttribute("data-theme");
  await app.addStyleTag({ content: `
    :root[data-theme="scope-probe"] canvas[aria-label="Synth output waveform"] {
      --kc: rgb(${probeColour.join(", ")});
    }
  ` });

  await expect.poll(async () => (await strokePixel(scope))[3]).toBeGreaterThan(100);
  const originalPixel = await strokePixel(scope);
  expect(colourDistance(originalPixel, probeColour)).toBeGreaterThan(80);

  await setTheme(app, "scope-probe");
  await expect.poll(async () => colourDistance(await strokePixel(scope), probeColour),
    { message: "the waveform stroke uses the probe colour" }).toBeLessThan(8);
  expect(await scope.evaluate((canvas, before) => canvas === before, originalNode)).toBe(true);

  await setTheme(app, originalTheme);
  await expect.poll(() => strokePixel(scope), { message: "the waveform stroke restores its original colour" }).toEqual(originalPixel);
  expect(await scope.evaluate((canvas, before) => canvas === before, originalNode)).toBe(true);
});

test.describe("with a second synth editor selected", () => {
  test.use({ seed: { project: project([], {}, { channels: [synthChannel(1, "Bass"), synthChannel(2, "Lead")], nextId: 3 }) } });

  test("an open but inactive synth output scope repaints on a live theme change", async ({ app }) => {
    const bass = await openScope(app, "Bass");
    // The Bass window covers the rack, so press Lead's opener directly. Opening Lead focuses and
    // selects it, so the Bass scope stops its frame loop.
    await app.getByRole("button", { name: "Edit the Lead synth" }).evaluate((b: HTMLElement) => b.click());
    await expect(app.getByRole("dialog", { name: "Lead synth" }).getByLabel("Synth output waveform")).toBeVisible();
    await app.addStyleTag({ content: `
      :root[data-theme="scope-probe"] canvas[aria-label="Synth output waveform"] {
        --kc: rgb(${probeColour.join(", ")});
      }
    ` });
    await expect.poll(async () => (await strokePixel(bass))[3]).toBeGreaterThan(100);
    const originalPixel = await strokePixel(bass);
    expect(colourDistance(originalPixel, probeColour)).toBeGreaterThan(80);
    const idleStrokes = await bass.evaluate(async (canvas: HTMLCanvasElement) => {
      const ctx = canvas.getContext("2d");
      if (!ctx) throw new Error("No synth output context");
      const stroke = ctx.stroke;
      let strokes = 0;
      ctx.stroke = (...args: [] | [Path2D]) => {
        strokes++;
        Reflect.apply(stroke, ctx, args);
      };
      try {
        for (let frame = 0; frame < 10; frame++) await new Promise(requestAnimationFrame);
        return strokes;
      } finally {
        ctx.stroke = stroke;
      }
    });
    expect(idleStrokes, "the Bass scope is inactive and runs no frame loop").toBe(0);

    await setTheme(app, "scope-probe");
    await expect.poll(async () => colourDistance(await strokePixel(bass), probeColour),
      { message: "the inactive waveform stroke uses the probe colour" }).toBeLessThan(8);
  });
});
