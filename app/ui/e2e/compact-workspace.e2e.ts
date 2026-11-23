import type { Locator, Page } from "@playwright/test";
import { expect, project, soundsFor, test, workspaceReady } from "./fixture";
import { twoChannels } from "./rollHelpers";

async function bothPanes(page: Page) {
  await workspaceReady(page);
  await page.keyboard.press("F7");
  await expect(page.getByRole("region", { name: "Channel rack" })).toBeVisible();
  await expect(page.getByRole("region", { name: "Piano roll", exact: true })).toBeVisible();
  await expect(page.getByRole("separator")).toBeVisible();
  // A finite local guard: no polling an empty/hidden canvas into acceptance.
  const frames = await page.locator(".rack-head").evaluateAll(async (headers) => {
    const samples: number[][] = [];
    for (let frame = 0; frame < 3; frame++) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      samples.push(headers.flatMap((el) => {
        const r = el.getBoundingClientRect();
        return [r.x, r.y, r.width, r.height];
      }));
    }
    return samples;
  });
  expect(frames[0]).toHaveLength(8);
  expect(frames[1]).toEqual(frames[0]);
  expect(frames[2]).toEqual(frames[1]);
}

async function contained(child: Locator, parent: Locator) {
  const c = await child.boundingBox();
  const p = await parent.boundingBox();
  expect(c).not.toBeNull();
  expect(p).not.toBeNull();
  if (!c || !p) throw new Error("visible geometry required");
  expect(c.width).toBeGreaterThan(0);
  expect(c.height).toBeGreaterThan(0);
  expect(c.x).toBeGreaterThanOrEqual(p.x);
  expect(c.y).toBeGreaterThanOrEqual(p.y);
  expect(c.x + c.width).toBeLessThanOrEqual(p.x + p.width);
  expect(c.y + c.height).toBeLessThanOrEqual(p.y + p.height);
}

async function geometry(page: Page) {
  const headers = page.locator(".rack-head");
  await expect(headers).toHaveCount(2);
  await expect(page.getByRole("region", { name: "Channel rack" }).locator(".rack-head").locator(":scope > span, :scope > label")).toHaveCount(2);
  await expect(page.getByRole("region", { name: "Piano roll", exact: true }).locator(".rack-head").locator(":scope > span, :scope > label")).toHaveCount(1);
  await expect(page.locator(".rack-head > span, .rack-head > label")).toHaveCount(3);
  for (const header of await headers.all()) {
    expect((await header.boundingBox())?.height).toBe(40);
    expect(await header.locator("h2").evaluate((el) => getComputedStyle(el).fontSize)).toBe("14px");
    // Structural selectors intentionally still find metadata if its class regresses to .chip.
    for (const metadata of await header.locator(":scope > span, :scope > label").all()) {
      expect((await metadata.boundingBox())?.height).toBe(24);
      await contained(metadata, header);
    }
  }
  const output = page.locator("button.chip[aria-haspopup='dialog']");
  expect((await output.boundingBox())?.height).toBe(32);
  expect(await output.evaluate((el) => {
    const s = getComputedStyle(el);
    return [s.gap, s.paddingTop, s.paddingRight, s.paddingBottom, s.paddingLeft, s.borderRadius];
  })).toEqual(["4px", "3px", "8px", "3px", "8px", "4px"]);
  await contained(output, page.locator(".toolbar"));
  await contained(page.getByRole("spinbutton", { name: "Pattern length" }), page.locator(".pattern-length"));
}

test.describe("compact workspace metadata", () => {
  test.use({ seed: { project: twoChannels([]) } });
  for (const [width, height] of [[1280, 800], [1440, 900], [1920, 1080], [900, 560]]) {
    test(`${width}x${height} geometry24/40/output32 and containment`, async ({ app }) => {
      await app.setViewportSize({ width, height });
      expect(await app.evaluate(() => [window.innerWidth, window.innerHeight])).toEqual([width, height]);
      await bothPanes(app);
      await geometry(app);
    });
  }

  test("PatternLength Enter commits actual model steps", async ({ app, mock }) => {
    await bothPanes(app);
    const input = app.getByRole("spinbutton", { name: "Pattern length" });
    await input.fill("32");
    await input.press("Enter");
    await expect.poll(async () => (await mock.view()).pattern.steps).toBe(32);
    await expect(input).toHaveValue("32");
    await expect(input).not.toBeFocused();
  });

  test("PatternLength blur commits actual model steps", async ({ app, mock }) => {
    await bothPanes(app);
    const input = app.getByRole("spinbutton", { name: "Pattern length" });
    await input.fill("24");
    await input.press("Tab");
    await expect.poll(async () => (await mock.view()).pattern.steps).toBe(24);
    await expect(input).toHaveValue("24");
    await expect(input).not.toBeFocused();
  });

  test("Output pointer, Escape and actual-opener focus return including WebKit body fallback", async ({ app }) => {
    await bothPanes(app);
    const output = app.locator("button.chip[aria-haspopup='dialog']");
    const dialog = app.getByRole("dialog", { name: "Audio output" });
    await output.focus();
    await output.click();
    await expect(dialog).toBeVisible();
    await expect.poll(() => dialog.evaluate((el) => el.contains(document.activeElement))).toBe(true);
    await app.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await expect(output).toBeFocused();
    // WebKit pointer clicks leave body active; exercise that actual browser path as well.
    await output.evaluate((el) => (el as HTMLElement).blur());
    expect(await app.evaluate(() => document.activeElement === document.body)).toBe(true);
    await output.click();
    await expect(dialog).toBeVisible();
    await app.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await expect(output).toBeFocused();
  });
});

test.describe("long generated metadata", () => {
  const name = "Generated channel with a deliberately long musical label ".repeat(8);
  const seedProject = project([name]);
  seedProject.pattern.name = "Generated pattern with a deliberately long musical label ".repeat(8);
  test.use({ viewport: { width: 900, height: 560 }, seed: { project: seedProject, files: soundsFor([name]) } });

  test("Long generated metadata remains contained", async ({ app }) => {
    expect(await app.evaluate(() => [window.innerWidth, window.innerHeight])).toEqual([900, 560]);
    await bothPanes(app);
    await geometry(app);
    await expect(app.locator(".rack-head > span b")).toHaveCount(2);
    for (const label of await app.locator(".rack-head > span b").all()) {
      expect(await label.evaluate((el) => {
        const s = getComputedStyle(el);
        return el.scrollWidth > el.clientWidth && s.textOverflow === "ellipsis" && s.whiteSpace === "nowrap";
      })).toBe(true);
      await contained(label, label.locator(".."));
    }
    const input = app.getByRole("spinbutton", { name: "Pattern length" });
    await input.click();
    await expect(input).toBeFocused();
  });
});
