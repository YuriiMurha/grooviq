import type { Locator, Page } from "@playwright/test";
import { expect, test, workspaceReady } from "./fixture";
import { twoChannels } from "./rollHelpers";

async function tabToSeparator(page: Page, separator: Locator) {
  for (let step = 0; step < 100; step++) {
    await page.keyboard.press("Tab");
    if (await separator.evaluate((element) => element === document.activeElement)) return;
  }
  throw new Error("The splitter must be reachable through natural keyboard navigation");
}

async function focusIndicator(separator: Locator) {
  return separator.evaluate((element) => {
    const style = getComputedStyle(element);
    const probe = document.createElement("span");
    probe.style.cssText = "position:fixed;visibility:hidden;color:var(--select)";
    document.body.append(probe);
    const color = getComputedStyle(probe).color;
    probe.remove();
    const luminance = (value: string) => {
      const channels = value.match(/[\d.]+/g)!.slice(0, 3).map(Number).map((channel) => {
        const normalized = channel / 255;
        return normalized <= 0.04045 ? normalized / 12.92 : ((normalized + 0.055) / 1.055) ** 2.4;
      });
      return channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722;
    };
    const foreground = luminance(color);
    const background = luminance(style.backgroundColor);
    return {
      focusVisible: element.matches(":focus-visible"),
      shadowColor: color,
      shadow: style.boxShadow,
      contrast: (Math.max(foreground, background) + 0.05) / (Math.min(foreground, background) + 0.05),
    };
  });
}

test.use({ seed: { project: twoChannels([]) } });

for (const [width, height] of [[1280, 800], [900, 560]]) {
  test(`splitter keyboard focus stays visible while resizing at ${width}x${height}`, async ({ app }, info) => {
    await app.setViewportSize({ width, height });
    await workspaceReady(app);
    await app.keyboard.press("F7");
    const rack = app.getByRole("region", { name: "Channel rack" });
    const roll = app.getByRole("region", { name: "Piano roll", exact: true });
    const separator = app.getByRole("separator", { name: "Resize the channel rack and the piano roll" });
    await expect(rack).toBeVisible();
    await expect(roll).toBeVisible();
    await tabToSeparator(app, separator);
    await app.screenshot({ path: info.outputPath(`splitter-focus-${width}x${height}.png`) });

    const indicator = await focusIndicator(separator);
    expect(indicator.focusVisible).toBe(true);
    expect(indicator.shadow).toContain(indicator.shadowColor);
    expect(indicator.shadow.replace(indicator.shadowColor, "").trim()).toBe("0px 0px 0px 2px inset");
    expect(indicator.contrast).toBeGreaterThanOrEqual(3);
    const separatorBox = await separator.boundingBox();
    const workspaceBox = await app.locator(".workspace").boundingBox();
    expect(separatorBox).not.toBeNull();
    expect(workspaceBox).not.toBeNull();
    if (!separatorBox || !workspaceBox) throw new Error("Visible splitter and workspace required");
    expect(separatorBox.height).toBe(6);
    expect(separatorBox.y).toBeGreaterThanOrEqual(workspaceBox.y);
    expect(separatorBox.y + separatorBox.height).toBeLessThanOrEqual(workspaceBox.y + workspaceBox.height);

    const rackPane = app.locator(".panel-rack");
    const initialHeight = (await rackPane.boundingBox())!.height;
    const initialValue = Number(await separator.getAttribute("aria-valuenow"));
    await app.keyboard.press("ArrowUp");
    await expect(separator).toHaveAttribute("aria-valuenow", String(initialValue - 5));
    await expect.poll(async () => (await rackPane.boundingBox())!.height).toBeLessThan(initialHeight);
    await expect(separator).toBeFocused();
    expect((await focusIndicator(separator)).shadow).toBe(indicator.shadow);
    await app.keyboard.press("ArrowDown");
    await expect(separator).toHaveAttribute("aria-valuenow", String(initialValue));
    await expect.poll(async () => (await rackPane.boundingBox())!.height).toBe(initialHeight);
    await expect(separator).toBeFocused();
    expect((await focusIndicator(separator)).focusVisible).toBe(true);
    expect((await focusIndicator(separator)).shadow).toBe(indicator.shadow);
  });
}
