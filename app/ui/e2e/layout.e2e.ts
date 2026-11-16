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
  // The mockup's name. "Untitled" is cut too in the real app and on Linux, but fits in macOS
  // headless WebKit, so only this name pins #104 everywhere.
  test.use({ seed: { project: { ...project(KIT), name: "Late Night Flip" }, files: soundsFor(KIT) } });

  test("is cut short at 1280 px while the toolbar has room (#104)", async ({ app }, info) => {
    await app.evaluate(() => document.fonts.ready.then(() => undefined));
    const button = app.getByRole("button", { name: "Late Night Flip", exact: true });
    const label = button.locator(".project-name-text");
    const { cut, room } = await label.evaluate((el) => {
      const chip = document.querySelector(".chip-anchor")?.getBoundingClientRect();
      const position = document.querySelector("[aria-label='Position']")?.getBoundingClientRect();
      return { cut: el.scrollWidth > el.clientWidth, room: chip && position ? chip.left - position.right : 0 };
    });
    await app.locator("header.toolbar").screenshot({ path: info.outputPath("toolbar.png") });
    expect(room, "free toolbar space between the position and the output chip").toBeGreaterThan(300);
    // Known defect #104: pinned. When it is fixed this fails: expect `false` instead.
    info.annotations.push({ type: "issue", description: "#104" });
    expect(cut, "#104 fixed? expect the full name now").toBe(true);
  });
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
