// The window at its normal and its smallest size (tauri.conf.json: 1280×800, at least 900×560),
// the keyboard path through the toolbar into the grid, and the project name in the toolbar.
import { expect, grid, project, soundsFor, test } from "./fixture";

const KIT = ["Kick", "Snare", "Hat", "Clap"];

for (const [w, h] of [
  [1280, 800],
  [900, 560],
] as const) {
  test.describe(`${w}×${h}`, () => {
    test.use({ seed: { project: project(KIT, { 1: [0, 4, 8, 12] }), files: soundsFor(KIT) }, viewport: { width: w, height: h } });

    test("no page overflow; toolbar controls do not overlap", async ({ app }, info) => {
      await info.attach("page-aria", { body: await app.locator("body").ariaSnapshot(), contentType: "text/yaml" });
      const overflow = await app.evaluate(() => ({
        x: document.documentElement.scrollWidth - window.innerWidth,
        y: document.documentElement.scrollHeight - window.innerHeight,
      }));
      expect(overflow).toEqual({ x: 0, y: 0 });
      const names = ["QA beat", "Play", "Stop", "Test tone"];
      const boxes = [];
      for (const name of names) {
        const b = await app.getByRole("button", { name, exact: true }).boundingBox();
        if (!b) throw new Error(`no ${name}`);
        boxes.push({ name, ...b });
      }
      for (const a of boxes) {
        for (const b of boxes) {
          if (a === b) continue;
          const overlap = a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
          expect(overlap, `${a.name} overlaps ${b.name}`).toBe(false);
        }
      }
      await app.screenshot({ path: info.outputPath(`window-${w}x${h}.png`) });
    });
  });
}

test.describe("the project name in the toolbar", () => {
  for (const name of ["Untitled", "Late Night Flip"]) {
    test.describe(name, () => {
      test.use({ seed: { project: { ...project(KIT), name }, files: soundsFor(KIT) } });
      test(`"${name}" shows in full at 1280 px`, async ({ app }, info) => {
        // #104: the toolbar cuts names short long before it is full. Remove this once fixed.
        test.fail(name === "Late Night Flip", "#104");
        const button = app.getByRole("button", { name, exact: true });
        const cut = await button.evaluate((el) => {
          const label = el.querySelector("span:not([aria-hidden])") ?? el;
          return label.scrollWidth > label.clientWidth;
        });
        await app.locator("header.toolbar").screenshot({ path: info.outputPath("toolbar.png") });
        expect(cut, "the name is cut off").toBe(false);
      });
    });
  }
});

test.describe("keyboard path", () => {
  test.use({ seed: { project: project(KIT, { 1: [0] }), files: soundsFor(KIT) } });

  test("Tab reaches every toolbar control, then the rack, then the grid", async ({ app }, info) => {
    const order: string[] = [];
    for (let i = 0; i < 40; i++) {
      await app.keyboard.press("Tab");
      const label = await app.evaluate(() => {
        const el = document.activeElement as HTMLElement | null;
        if (!el || el === document.body) return "body";
        return el.getAttribute("aria-label") ?? el.textContent?.trim().slice(0, 30) ?? el.tagName;
      });
      order.push(label);
      if (label === "Step grid") break;
    }
    await info.attach("tab-order", { body: order.join("\n"), contentType: "text/plain" });
    for (const stop of ["QA beat▾", "Play", "Stop", "BPM", "Beats per bar", "Beat note value", "Test tone", "Pattern length", "Mute Kick", "Step grid"]) {
      expect(order.some((o) => o.replace(/\s/g, "") === stop.replace(/\s/g, "") || o === stop), `Tab reaches ${stop}`).toBe(true);
    }
    await expect(grid(app)).toBeFocused();
    await app.screenshot({ path: info.outputPath("grid-focused-by-tab.png") });
  });
});
