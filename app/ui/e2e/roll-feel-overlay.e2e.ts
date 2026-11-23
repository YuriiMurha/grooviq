import type { Locator, Page } from "@playwright/test";
import { expect, project, test, workspaceReady, type Mock } from "./fixture";
import { boxSelect, sessionNotes, settled } from "./rollHelpers";

const notes = [60, 64, 67].map((key, i) => ({ id: i + 1, channel: 1, tick: 480, key, length: 240, velocity: Math.fround(0.8), slide: false }));
const seed = project(["Keys"]);
seed.pattern.notes = notes;
test.use({ seed: { project: seed } });
const panel = (p: Page) => p.getByRole("group", { name: "Transform amounts" });
const trigger = (p: Page) => p.getByText("Feel", { exact: true });
const spread = (p: Page) => p.getByRole("slider", { name: "Strum spread", exact: true });

async function ready(p: Page) {
  await workspaceReady(p);
  await p.keyboard.press("F7");
  await expect(p.getByRole("region", { name: "Channel rack" })).toBeVisible();
  await expect(p.getByRole("region", { name: "Piano roll", exact: true })).toBeVisible();
  await expect(p.getByRole("separator")).toBeVisible();
  await settled(p);
}
async function open(p: Page) {
  await trigger(p).click();
  await expect(panel(p)).toBeVisible();
}
async function held(p: Page, el: Locator) {
  const b = (await el.boundingBox())!;
  await p.mouse.move(b.x + b.width / 2, b.y + b.height / 2);
  await p.mouse.down();
  await p.mouse.move(b.x + b.width / 2, b.y + b.height / 2 - 24, { steps: 4 });
  await expect(el).not.toHaveAttribute("aria-valuenow", "30");
}
async function unchanged(p: Page, mock: Mock, before: number[][], cause = "dismissal") {
  expect(await sessionNotes(mock)).toEqual(before);
  expect((await mock.view()).canUndo).toBe(false);
  await open(p);
  await expect(spread(p), `Feel ${cause} cancellation`).toHaveAttribute("aria-valuenow", "30");
}

for (const mode of ["native", "fallback"] as const) {
  test.describe(mode, () => {
    test.beforeEach(async ({ page }) => {
      if (mode === "fallback") await page.addInitScript(() => {
        Object.defineProperty(HTMLElement.prototype, "showPopover", { configurable: true, value: undefined });
        Object.defineProperty(HTMLElement.prototype, "hidePopover", { configurable: true, value: undefined });
      });
    });
    test(`Feel ${mode}: initial bounds and resize preserve full controls`, async ({ app }, info) => {
      await ready(app);
      await open(app);
      for (const [width, height] of [[1280, 800], [1440, 900], [1920, 1080], [900, 560]]) {
        await app.setViewportSize({ width, height });
        await expect.poll(() => panel(app).evaluate(el => {
          const r = el.getBoundingClientRect();
          const bottom = document.querySelector(".hintbar")!.getBoundingClientRect().top - 8;
          return r.left >= 8 && r.right <= innerWidth - 8 && r.top >= 8 && r.bottom <= bottom;
        })).toBe(true);
        const sliders = panel(app).getByRole("slider");
        for (const slider of await sliders.all()) {
          expect(await slider.evaluate(el => {
            const r = el.getBoundingClientRect();
            const p = el.parentElement!.getBoundingClientRect();
            return r.top >= p.top && r.bottom <= p.bottom && el.contains(document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2));
          })).toBe(true);
        }
        if (mode === "native") expect(await panel(app).evaluate(el => el.matches(":popover-open"))).toBe(true);
        else await expect(panel(app)).not.toHaveAttribute("popover");
        await app.screenshot({ path: info.outputPath(`Feel-${mode}-${width}.png`) });
      }
    });
    test(`Feel ${mode}: inside pointer and keyboard edits apply the intended strum`, async ({ app, mock }) => {
      await app.setViewportSize({ width: 1440, height: 900 });
      await ready(app);
      await boxSelect(app, 0, 72, 2000, 55);
      const before = await sessionNotes(mock);
      await open(app);
      await spread(app).click();
      await expect(panel(app)).toBeVisible();
      await spread(app).press("End");
      expect(await sessionNotes(mock)).toEqual(before);
      expect((await mock.view()).canUndo).toBe(false);
      await app.getByRole("button", { name: "Strum up", exact: true }).click();
      await expect.poll(() => sessionNotes(mock)).toEqual([[1, 480, 240, 60], [1, 600, 240, 64], [1, 720, 240, 67]]);
      await app.keyboard.press("ControlOrMeta+KeyZ");
      await expect.poll(() => sessionNotes(mock)).toEqual(before);
    });
    test(`Feel ${mode}: reposition during a held gesture never cancels it`, async ({ app, mock }) => {
      await ready(app);
      const before = await sessionNotes(mock);
      await open(app);
      await held(app, spread(app));
      const value = await spread(app).getAttribute("aria-valuenow");
      await app.setViewportSize({ width: 1440, height: 900 });
      await settled(app);
      await expect(spread(app)).toHaveAttribute("aria-valuenow", value!);
      await app.mouse.up();
      await app.keyboard.press("Escape");
      await open(app);
      await expect(spread(app)).toHaveAttribute("aria-valuenow", value!);
      expect(await sessionNotes(mock)).toEqual(before);
      expect((await mock.view()).canUndo).toBe(false);
    });
    test(`Feel ${mode}: Escape outside and toggle retire held gestures`, async ({ app, mock }) => {
      await ready(app);
      const before = await sessionNotes(mock);
      for (const cause of ["Escape", "outside", "toggle"]) {
        await open(app);
        await held(app, spread(app));
        if (cause === "Escape") await app.keyboard.press("Escape");
        else if (cause === "outside") await app.getByRole("textbox", { name: "BPM", exact: true }).focus().then(() => app.getByRole("textbox", { name: "BPM", exact: true }).dispatchEvent("pointerdown"));
        else await trigger(app).evaluate(el => {
          (el as HTMLElement).click();
          // Release before the queued details toggle so dismissal cannot win by timing luck.
          el.closest("details")!.querySelector('[data-param="spread"]')!.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, pointerId: 1 }));
        });
        await app.mouse.up();
        await expect(panel(app)).not.toBeVisible();
        if (cause === "Escape") await expect(trigger(app)).toBeFocused();
        if (cause === "outside") await expect(app.getByRole("textbox", { name: "BPM", exact: true })).toBeFocused();
        await unchanged(app, mock, before, cause);
        await app.keyboard.press("Escape");
      }
      await open(app);
      const wheelBox = (await spread(app).boundingBox())!;
      await app.mouse.move(wheelBox.x + wheelBox.width / 2, wheelBox.y + wheelBox.height / 2);
      await app.mouse.wheel(0, -40);
      await expect(spread(app)).not.toHaveAttribute("aria-valuenow", "30");
      await app.keyboard.press("Escape");
      // The existing owner settles a wheel gesture after 300ms; closing must retire it first.
      await app.waitForTimeout(350);
      await unchanged(app, mock, before);
    });
    test(`Feel ${mode}: keyboard focus transfer yields to another app overlay`, async ({ app, mock }) => {
      await ready(app);
      const before = await sessionNotes(mock);
      for (const kind of ["menu", "modal", "device"]) {
        await open(app);
        await held(app, spread(app));
        if (kind === "menu") {
          await app.locator(".project-name").focus();
          await app.keyboard.press("Enter");
          await expect(app.getByRole("menu", { name: "Project" })).toBeVisible();
        } else if (kind === "device") {
          await app.getByRole("button", { name: "Edit the Keys sampler", exact: true }).focus();
          await app.keyboard.press("Enter");
          await expect(app.getByRole("dialog", { name: "Keys sampler" })).toBeVisible();
        } else {
          await app.locator(".channel-name").focus();
          await app.keyboard.press("Delete");
          await expect(app.getByRole("dialog")).toBeVisible();
        }
        await expect(panel(app)).not.toBeVisible();
        await app.mouse.up();
        await expect(trigger(app)).not.toBeFocused();
        await app.keyboard.press("Escape");
        await unchanged(app, mock, before);
        await app.keyboard.press("Escape");
      }
    });
    test(`Feel ${mode}: window blur cancels without a late commit`, async ({ app, mock }) => {
      await ready(app);
      const before = await sessionNotes(mock);
      await open(app);
      await held(app, spread(app));
      await app.evaluate(() => window.dispatchEvent(new Event("blur")));
      await app.mouse.up();
      await expect(panel(app)).not.toBeVisible();
      await unchanged(app, mock, before);
    });
    test(`Feel ${mode}: F7 unmount retires the presenter and capture`, async ({ app, mock }) => {
      await ready(app);
      const before = await sessionNotes(mock);
      await open(app);
      await held(app, spread(app));
      await app.keyboard.press("F7");
      await app.mouse.up();
      await expect(panel(app)).toHaveCount(0);
      await app.keyboard.press("F7");
      await unchanged(app, mock, before);
    });
  });
}

test("Feel native: external native hide synchronizes details before release", async ({ app, mock }) => {
  await ready(app);
  const before = await sessionNotes(mock);
  await open(app);
  await held(app, spread(app));
  expect(await panel(app).evaluate(el => {
    (el as HTMLElement).hidePopover();
    // A release in the same task must already see a cancelled owner, before queued toggle.
    el.querySelector('[data-param="spread"]')!.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, pointerId: 1 }));
    return !(el.closest("details") as HTMLDetailsElement).open;
  })).toBe(true);
  await app.mouse.up();
  await expect(panel(app)).not.toBeVisible();
  await expect(trigger(app)).not.toBeFocused();
  await unchanged(app, mock, before);
});
