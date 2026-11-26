import type { Locator, Page } from "@playwright/test";
import { expect, project, soundsFor, test } from "./fixture";

test.use({
  seed: { project: project(["Kick"]), files: soundsFor(["Kick"]) },
  colorScheme: "light",
  reducedMotion: "reduce",
});

// Native select painting can ignore CSS background/color. Sample the rendered label area,
// excluding its border and arrow, so a computed-style check cannot hide unreadable text.
async function labelContrast(page: Page, select: Locator) {
  const png = await select.screenshot({ animations: "disabled", scale: "css" });
  const measurement = await page.evaluate(async (data) => {
    const image = new Image();
    image.src = `data:image/png;base64,${data}`;
    await image.decode();
    const canvas = document.createElement("canvas");
    canvas.width = image.width;
    canvas.height = image.height;
    const context = canvas.getContext("2d", { willReadFrequently: true });
    if (!context) throw new Error("A real rendered label requires a 2D pixel reader");
    context.drawImage(image, 0, 0);
    const pixels = context.getImageData(4, 4, image.width - 22, image.height - 8).data;
    const values: number[] = [];
    for (let offset = 0; offset < pixels.length; offset += 4) {
      const linear = [pixels[offset], pixels[offset + 1], pixels[offset + 2]].map((channel) => {
        const value = channel / 255;
        return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
      });
      values.push(0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]);
    }
    values.sort((a, b) => a - b);
    const low = values[Math.floor((values.length - 1) * 0.05)];
    const high = values[Math.floor((values.length - 1) * 0.95)];
    return { width: image.width, height: image.height, samples: values.length, low, high, contrast: (high + 0.05) / (low + 0.05) };
  }, png.toString("base64"));
  return { png, measurement };
}

for (const viewport of [{ width: 900, height: 560 }, { width: 1280, height: 800 }]) {
  test(`Playback labels stay readable with light native controls at ${viewport.width}x${viewport.height}`, async ({ app, mock }, info) => {
    await app.setViewportSize(viewport);
    const mode = app.getByRole("combobox", { name: "Playback mode" });
    await expect(mode).toBeEnabled();
    for (const value of ["pattern", "song"] as const) {
      await mode.selectOption(value);
      await expect(mode).toHaveValue(value);
      await expect.poll(() => mock.eval<string>("(m) => m.workspaceSnapshot().playMode")).toBe(value);
      await mode.press("Tab");
      const rendered = await labelContrast(app, mode);
      await info.attach(`${value}-native-label`, { body: rendered.png, contentType: "image/png" });
      await info.attach(`${value}-rendered-contrast`, { body: JSON.stringify(rendered.measurement), contentType: "application/json" });
      await info.attach(`${value}-workspace`, { body: await app.screenshot({ animations: "disabled", scale: "css" }), contentType: "image/png" });
      expect(rendered.measurement.samples).toBeGreaterThan(0);
      expect(rendered.measurement.contrast, `rendered ${value} label contrast`).toBeGreaterThanOrEqual(4.5);
    }
    const output = app.getByRole("button", { name: /Silent \(QA mode\)/ });
    await output.click();
    await expect(app.getByRole("dialog", { name: "Settings" })).toBeVisible();
    await info.attach("output-icons", { body: await app.screenshot({ animations: "disabled", scale: "css" }), contentType: "image/png" });
    await app.keyboard.press("Escape");
    await expect(app.getByRole("dialog", { name: "Settings" })).toBeHidden();
    await expect(output).toBeFocused();
  });
}
