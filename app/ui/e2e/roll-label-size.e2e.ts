import type { Locator } from "@playwright/test";
import { expect, test } from "./fixture";
import { settled, twoChannels } from "./rollHelpers";

test.use({
  seed: { project: twoChannels([{ id: 1, channel: 1, tick: 480, length: 240, key: 60, velocity: 0.8 }]) },
  reducedMotion: "reduce",
});

async function labelBounds(label: Locator) {
  return label.evaluate((element) => {
    const style = getComputedStyle(element);
    const box = element.getBoundingClientRect();
    const range = document.createRange();
    range.selectNodeContents(element);
    const textBox = range.getBoundingClientRect();
    // A range includes the font's full ascent and descent. Check the painted glyphs
    // vertically so that unused descender space does not count as overflow on a key.
    const context = document.createElement("canvas").getContext("2d");
    if (!context) throw new Error("A 2D context is required for label metrics");
    context.font = `${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
    const metrics = context.measureText(element.textContent?.trim() ?? "");
    const baseline = textBox.top + metrics.fontBoundingBoxAscent;
    return {
      fontSize: Number.parseFloat(style.fontSize),
      fits: range.getClientRects().length === 1 &&
        textBox.left >= box.left && textBox.right <= box.right &&
        baseline - metrics.actualBoundingBoxAscent >= box.top &&
        baseline + metrics.actualBoundingBoxDescent <= box.bottom,
    };
  });
}

for (const viewport of [{ width: 900, height: 560 }, { width: 1280, height: 800 }]) {
  test.describe(`${viewport.width}×${viewport.height}`, () => {
    test.use({ viewport });

    test("Bass piano-roll labels fit at 11 px or larger", async ({ app }) => {
      await app.keyboard.press("F7");
      const roll = app.getByRole("region", { name: "Piano roll", exact: true });
      await expect(roll.getByText("Bass", { exact: true })).toBeVisible();
      await expect(app.getByRole("region", { name: "Channel rack" })).toBeVisible();
      await settled(app);
      await app.evaluate(async () => { await document.fonts.ready; });

      const labels = [
        { name: "C4", label: roll.getByText("C4", { exact: true }) },
        { name: "Velocity gutter", label: roll.locator(".roll-lane").getByText("Velocity", { exact: true }) },
      ];
      for (const { name, label } of labels) {
        await expect(label).toBeInViewport({ ratio: 1 });
        const { fontSize, fits } = await labelBounds(label);
        expect(fontSize, `${name} font size`).toBeGreaterThanOrEqual(11);
        expect(fits, `${name} text must fit inside its box`).toBe(true);
      }
    });
  });
}
