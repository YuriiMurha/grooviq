import { choosePickerOption, pickerButton } from "./listPickerHelpers";
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
    await info.attach("toolbar-width-budget", { body: JSON.stringify(await app.locator(".toolbar, .toolbar-project, .project-anchor, .project-name, .project-name-text, .transport, .transport-buttons, .pattern-mode-controls, .tempo, .position, .toolbar-audio, .chip, .tone, .meter").evaluateAll((nodes) => nodes.map((node) => {
      const box = node.getBoundingClientRect(); const style = getComputedStyle(node);
      return { className: node.className, x: box.x, width: box.width, height: box.height, scrollWidth: node.scrollWidth,
        flex: style.flex, minWidth: style.minWidth, maxWidth: style.maxWidth, gap: style.gap };
    }))), contentType: "application/json" });
    expect(geometry.width).toBeGreaterThan(20);
    expect(geometry.natural).toBeGreaterThan(geometry.width);
    expect(await app.locator(".toolbar-project").evaluate((el) => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(0);
    expect(await app.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBe(0);
    const controls = [title, app.getByRole("button", { name: "Play", exact: true }), app.getByRole("button", { name: "Stop", exact: true }), app.locator(".pattern-mode-controls"), app.locator(".toolbar-audio")];
    const boxes = [];
    for (const control of controls) {
      const box = await control.boundingBox();
      expect(box).not.toBeNull();
      boxes.push(box!);
      expect(box!.x).toBeGreaterThanOrEqual(0);
      expect(box!.x + box!.width).toBeLessThanOrEqual(900);
      expect(box!.y).toBeGreaterThanOrEqual(0);
      expect(box!.y + box!.height).toBeLessThanOrEqual((await app.locator(".toolbar").boundingBox())!.height);
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
  test("long current pattern and mode progress retain title width and toolbar containment", async ({ app, mock }) => {
    const rail = app.getByRole("complementary", { name: "Patterns" });
    await rail.getByRole("button", { name: "Rename", exact: true }).click();
    const patternName = "Alternate drums with a long generated pattern name";
    await rail.getByRole("textbox", { name: "Pattern name" }).fill(patternName);
    await rail.getByRole("button", { name: "Save name", exact: true }).click();
    await expect(rail.getByRole("button", { name: patternName, exact: true })).toHaveAttribute("aria-current", "true");
    const check = async () => {
      expect(await app.locator(".project-name-text").evaluate((el) => el.clientWidth)).toBeGreaterThan(20);
      expect(await app.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBe(0);
      const toolbar = (await app.locator(".toolbar").boundingBox())!;
      const mode = (await app.locator(".pattern-mode-controls").boundingBox())!;
      expect(mode.x).toBeGreaterThanOrEqual(toolbar.x);
      expect(mode.x + mode.width).toBeLessThanOrEqual(toolbar.x + toolbar.width);
      expect(mode.y).toBeGreaterThanOrEqual(toolbar.y);
      expect(mode.y + mode.height).toBeLessThanOrEqual(toolbar.y + toolbar.height);
    };
    await check();
    await mock.eval("(m) => m.holdCommand('arrangement_mode')");
    const mode = pickerButton(app, "Playback mode");
    await choosePickerOption(app, "Playback mode", "Song");
    await expect(mode).toBeDisabled();
    await expect(app.locator(".pattern-current")).toHaveText("Changing mode…");
    await check();
    await mock.eval("(m) => m.releaseCommand('arrangement_mode')");
    await expect(mode).toBeEnabled();
    await expect(mode).toHaveAccessibleName("Playback mode: Song");
    await expect(app.locator(".pattern-current")).toHaveText(patternName);
    await check();
    expect(await mock.calls("transport_play")).toHaveLength(0);
  });
});

test.describe("keyboard path", () => {
  test.use({ seed: { project: project(KIT, { 1: [0] }), files: soundsFor(KIT) } });

  test("Tab reaches every toolbar control, then the rack, then the grid", async ({ app }, info) => {
    await expect(pickerButton(app, "Playback mode")).toBeEnabled();
    await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
    const focusStops = await app.locator('button:enabled, input:enabled, select:enabled, [tabindex="0"]').evaluateAll((nodes) =>
      nodes.filter((node) => node instanceof HTMLElement && node.tabIndex >= 0 && node.getClientRects().length > 0).length);
    expect(focusStops).toBeGreaterThan(0);
    const order: string[] = [];
    const visited = new Set<number>();
    for (let i = 0; i <= focusStops; i++) {
      await app.keyboard.press("Tab");
      const focused = await app.evaluate(() => {
        const el = document.activeElement as HTMLElement | null;
        const stops = [...document.querySelectorAll<HTMLElement>('button:enabled, input:enabled, select:enabled, [tabindex="0"]')]
          .filter((node) => node.tabIndex >= 0 && node.getClientRects().length > 0);
        return { index: el ? stops.indexOf(el) : -1,
          label: !el || el === document.body ? "body" : el.getAttribute("aria-label") ?? el.textContent?.trim().slice(0, 30) ?? el.tagName };
      });
      order.push(focused.label);
      expect(focused.index, `Tab reaches an enabled visible stop: ${focused.label}`).toBeGreaterThanOrEqual(0);
      expect(visited.has(focused.index), `Tab cycled before reaching the grid: ${focused.label}`).toBe(false);
      visited.add(focused.index);
      if (focused.label === "Step grid") break;
    }
    await info.attach("tab-order", { body: order.join("\n"), contentType: "text/plain" });
    for (const stop of ["QA beat", "Play", "Stop", "Playback mode: Pattern", "BPM", "Beats per bar: 4", "Beat note value: 4", "Test tone", "New", "Pattern 1", "Duplicate", "Rename", "Refresh patterns", "Pattern length", "Mute Kick", "Step grid"]) {
      expect(order.some((o) => o.replace(/\s/g, "") === stop.replace(/\s/g, "") || o === stop), `Tab reaches ${stop}`).toBe(true);
    }
    await expect(grid(app)).toBeFocused();
    await app.screenshot({ path: info.outputPath("grid-focused-by-tab.png") });
  });
});
