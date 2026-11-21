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

for (const name of ["Late Night Flip", "Untitled"]) {
  test.describe(`project title: ${name}`, () => {
    test.use({ seed: { project: project(KIT, {}, { name }), files: soundsFor(KIT) } });
    test("uses available width at 1280 (#104)", async ({ app }, info) => {
      await app.evaluate(() => document.fonts.ready.then(() => undefined));
      const label = app.locator(".project-name-text");
      expect(await label.evaluate((el) => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(0);
      await app.locator("header.toolbar").screenshot({ path: info.outputPath("toolbar.png") });
    });
  });
}

test.describe("long project title", () => {
  const name = "Late Night Flip — extended arrangement with alternate drums and final mix";
  test.use({ seed: { project: project(KIT, {}, { name }), files: soundsFor(KIT) }, viewport: { width: 900, height: 560 } });
  test("ellipsizes without hiding controls and keeps the full project name (#104)", async ({ app, mock }, info) => {
    await app.evaluate(() => document.fonts.ready.then(() => undefined));
    const title = app.getByRole("button", { name, exact: true });
    const geometry = await app.locator(".project-name-text").evaluate((el) => ({ width: el.clientWidth, natural: el.scrollWidth }));
    expect(geometry.width).toBeGreaterThan(20);
    expect(geometry.natural).toBeGreaterThan(geometry.width);
    expect(await app.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBe(0);
    const controls = [title, app.getByRole("button", { name: "Play", exact: true }), app.getByRole("button", { name: "Stop", exact: true }), app.locator(".toolbar-audio")];
    const boxes = [];
    for (const control of controls) {
      const box = await control.boundingBox();
      expect(box).not.toBeNull();
      boxes.push(box!);
      expect(box!.x).toBeGreaterThanOrEqual(0);
      expect(box!.x + box!.width).toBeLessThanOrEqual(900);
    }
    for (let i = 1; i < boxes.length; i++) expect(boxes[i].x).toBeGreaterThanOrEqual(boxes[i - 1].x + boxes[i - 1].width);
    await title.click();
    await expect(app.getByRole("menuitem", { name: /^New/ })).toBeFocused();
    await app.keyboard.press("Escape");
    await expect(title).toBeFocused();
    await title.click();
    await app.getByRole("menuitem", { name: /Save as/ }).click();
    expect(await mock.eval("(m) => m.state.dialogCalls.at(-1).options.defaultPath")).toBe(`${name}.grooviq`);
    await expect(title).toHaveAccessibleName(name);
    await mock.eval(`(m) => m.queueDialog(${JSON.stringify(`/qa/projects/${name}.grooviq`)})`);
    await title.click();
    await app.getByRole("menuitem", { name: /Save as/ }).click();
    await expect.poll(() => mock.view().then((v) => v.path)).toBe(`/qa/projects/${name}.grooviq`);
    expect(await mock.eval(`(m) => m.file(${JSON.stringify(`/qa/projects/${name}.grooviq`)}).project.name`)).toBe(name);
    await expect(title).toHaveAccessibleName(name);
    await title.click();
    await app.getByRole("menuitem", { name: /Export WAV/ }).click();
    await expect(app.getByRole("dialog", { name: "Export WAV" })).toBeVisible();
    await app.getByRole("button", { name: "Close", exact: true }).click();
    await expect(title).toHaveAccessibleName(name);
    await info.attach("geometry", { body: JSON.stringify({ geometry, boxes }), contentType: "application/json" });
    await app.locator("header.toolbar").screenshot({ path: info.outputPath("toolbar-long.png") });
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
