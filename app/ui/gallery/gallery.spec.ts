import { createHash } from "node:crypto";
import { writeFileSync } from "node:fs";
import { test as base, expect, type Browser, type Locator, type Page, type TestInfo } from "@playwright/test";
import { captureContext } from "./captureContext";
import { references } from "./contract";

const test = base.extend<{ galleryErrors: string[] }>({
  galleryErrors: [async ({ page }, runTest) => {
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      if (message.type() === "error" || message.type() === "warning") errors.push(`${message.type()}: ${message.text()}`);
    });
    await runTest(errors);
    expect(errors).toEqual([]);
  }, { auto: true }],
});
const source = captureContext();

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => document.fonts.ready);
  await expect(page.locator(".gallery-reference")).toHaveCount(references.length);
  expect(await page.evaluate(() => document.fonts.check("13px 'Manrope Variable'"))).toBe(true);
});

test.afterEach(async ({ galleryErrors }, info) => {
  await info.attach("gallery-errors", { body: JSON.stringify(galleryErrors), contentType: "application/json" });
});

async function tabTo(page: Page, target: Locator) {
  for (let step = 0; step < 30; step++) {
    await page.keyboard.press("Tab");
    if (await target.evaluate((element) => element === document.activeElement)) return;
  }
  throw new Error("Gallery control is unreachable through sequential keyboard navigation");
}

async function capture(page: Page, browser: Browser, info: TestInfo,
  reference: typeof references[number], state: string, errors: readonly string[]) {
  const frame = page.locator(`[data-reference="${reference.id}"]`);
  const control = frame.getByRole(reference.role, { name: reference.name, exact: true });
  await expect(frame).toHaveCount(1);
  await frame.scrollIntoViewIfNeeded();
  if (state === "hover") await control.hover();
  await expect(control).toBeVisible();
  const cropBounds = await frame.boundingBox();
  expect(cropBounds).not.toBeNull();
  const context = await control.evaluate((element) => {
    const box = element.getBoundingClientRect();
    return { box: { x: box.x, y: box.y, width: box.width, height: box.height },
      cssViewport: { width: innerWidth, height: innerHeight }, dpr: devicePixelRatio,
      focusVisible: element.matches(":focus-visible"), focused: element === document.activeElement,
      hovered: element.matches(":hover"), value: element.getAttribute("aria-valuenow"),
      pressed: element.getAttribute("aria-pressed"), checked: element.getAttribute("aria-checked"),
      selectedOptions: [...(element.closest('[role="radiogroup"]')?.querySelectorAll('[role="radio"][aria-checked="true"]') ?? [])]
        .map((option) => option.getAttribute("aria-label") ?? option.textContent),
      fonts: { status: document.fonts.status, family: getComputedStyle(document.body).fontFamily },
      reducedMotion: matchMedia("(prefers-reduced-motion: reduce)").matches };
  });
  expect(context.box.width).toBeGreaterThan(0);
  expect(context.box.height).toBeGreaterThan(0);
  if (state === "hover") expect(context.hovered).toBe(true);
  expect(context.cssViewport).toEqual(page.viewportSize());
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  const png = await frame.screenshot({ animations: "disabled", scale: "css" });
  const name = `${reference.id}.${state}`;
  const path = info.outputPath(`${name}.png`);
  writeFileSync(path, png, { flag: "wx" });
  const manifest = {
    schemaVersion: 1,
    captureId: `${info.project.name}.${reference.id}.${state}.${context.cssViewport.width}x${context.cssViewport.height}`,
    semanticId: reference.id, sourceComponent: reference.source, state,
    variant: reference.id === "device.parameter.field" ? "field" : reference.id === "device.parameter.knob" ? "knob" : "default",
    support: "actual imported component", source,
    errorsObservedBeforeCapture: [...errors], completeCaseErrorAttachment: "gallery-errors",
    browser: { project: info.project.name, version: browser.version() }, ...context, cropBounds,
    viewportState: context.cssViewport.width === 900 ? "compact" : "desktop",
    renderer: "existing DOM/SVG controls; no canvas, native engine or audio fixture",
    png: { path, bytes: png.length, sha256: createHash("sha256").update(png).digest("hex"),
      width: png.readUInt32BE(16), height: png.readUInt32BE(20), scale: "css" },
  };
  const record = info.outputPath(`${name}.json`);
  writeFileSync(record, JSON.stringify(manifest, null, 2) + "\n", { flag: "wx" });
  await info.attach(`${name}-candidate`, { path, contentType: "image/png" });
  await info.attach(`${name}-manifest`, { path: record, contentType: "application/json" });
  if (process.env.GROOVIQ_GALLERY_APPROVED_BASELINES === "1") {
    expect(png).toMatchSnapshot(`${name}.${context.cssViewport.width}x${context.cssViewport.height}.png`);
  }
}

for (const [width, height] of [[1280, 800], [1440, 900], [1920, 1080], [900, 560]]) {
  for (const state of ["normal", "keyboard-focus", "hover"] as const) {
    test(`actual controls ${state} at ${width}x${height}`, async ({ page, browser, galleryErrors }, info) => {
      await page.setViewportSize({ width, height });
      for (const reference of references) {
        const frame = page.locator(`[data-reference="${reference.id}"]`);
        const control = frame.getByRole(reference.role, { name: reference.name, exact: true });
        if (state === "keyboard-focus") {
          await tabTo(page, control);
          await expect(control).toBeFocused();
          expect(await control.evaluate((element) => element.matches(":focus-visible"))).toBe(true);
        }
        await capture(page, browser, info, reference, state, galleryErrors);
      }
    });
  }
}

test("actual controls update generated state", async ({ page, browser, galleryErrors }, info) => {
  const level = page.getByRole("slider", { name: "Gallery level", exact: true });
  await tabTo(page, level);
  await page.keyboard.press("ArrowUp");
  await expect(level).toHaveAttribute("aria-valuenow", "0.51");
  await page.keyboard.press("Home");
  await expect(level).toHaveAttribute("aria-valuenow", "0");
  await level.dblclick();
  await expect(level).toHaveAttribute("aria-valuenow", "0.5");
  await level.scrollIntoViewIfNeeded();
  const knobBounds = await level.boundingBox();
  if (!knobBounds) throw new Error("Gallery level has no pointer target");
  const pointerX = knobBounds.x + knobBounds.width / 2;
  const pointerY = Math.round(knobBounds.y + knobBounds.height / 2);
  const drawnScale = await level.evaluate((element) => element.getBoundingClientRect().height / (element as HTMLElement).offsetHeight);
  const expectedDragValue = String(Number((0.5 + 18 / drawnScale / 180).toFixed(4)));
  await info.attach("gallery-drag-premise", { body: JSON.stringify({ drawnScale, pointerDelta: 18, expectedDragValue }), contentType: "application/json" });
  await page.mouse.move(pointerX, pointerY);
  await page.mouse.down();
  try {
    await page.mouse.move(pointerX, pointerY - 18);
    await expect(level, "held gallery drag updates the live value").toHaveAttribute("aria-valuenow", expectedDragValue);
    await expect(level).toHaveAttribute("aria-valuetext", "60%");
  } finally {
    await page.mouse.up();
  }
  await expect(level, "released gallery drag retains the live value").toHaveAttribute("aria-valuenow", expectedDragValue);
  await page.keyboard.press("End");
  await expect(level).toHaveAttribute("aria-valuenow", "1");
  const field = page.getByRole("slider", { name: "Gallery amount", exact: true });
  await tabTo(page, field);
  await page.keyboard.press("End");
  await expect(field).toHaveAttribute("aria-valuenow", "1");
  const power = page.getByRole("switch", { name: "Gallery power" });
  await power.click();
  await expect(power).toBeChecked();
  const sine = page.getByRole("radio", { name: "Sine", exact: true });
  await tabTo(page, sine);
  await page.keyboard.press("ArrowRight");
  await expect(page.getByRole("radio", { name: "Triangle", exact: true })).toBeChecked();
  for (const name of ["Record", "Typing keyboard", "Metronome"]) {
    const button = page.getByRole("button", { name, exact: true });
    await button.click();
    await expect(button).toHaveAttribute("aria-pressed", "true");
  }
  const rack = page.getByRole("slider", { name: "Gallery rack volume" });
  await tabTo(page, rack);
  await page.keyboard.press("End");
  await expect(rack).toHaveAttribute("aria-valuenow", "1");
  for (const reference of references) await capture(page, browser, info, reference, "interaction-result", galleryErrors);
});
