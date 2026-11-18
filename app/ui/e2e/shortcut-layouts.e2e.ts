import type { Page } from "@playwright/test";
import { clickCell, expect, project, test } from "./fixture";

// Delivered DOM events prove App routing, not the keys emitted by an OS keyboard layout.
async function delivered(page: Page, key: string, code: string, shiftKey = false) {
  return page.evaluate(({ key, code, shiftKey }) => {
    const mac = /^mac/i.test(navigator.platform) || /iphone|ipad/i.test(navigator.platform);
    const event = new KeyboardEvent("keydown", {
      key, code, shiftKey, metaKey: mac, ctrlKey: !mac, bubbles: true, cancelable: true,
    });
    (document.activeElement ?? document.body).dispatchEvent(event);
    return event.defaultPrevented;
  }, { key, code, shiftKey });
}

test.use({ seed: { project: project(["Kick"]) } });
for (const letter of ["і", "σ"]) {
  test(`${letter}: delivered Mod shortcuts undo, redo and save real project state`, async ({ app, mock }) => {
    await clickCell(app, 0, 0);
    await expect.poll(() => mock.view().then((v) => v.pattern.cells["1"][0])).toBe(0.8);
    expect(await delivered(app, letter, "KeyZ")).toBe(true);
    await expect.poll(() => mock.view().then((v) => v.pattern.cells["1"][0])).toBeNull();
    expect(await delivered(app, letter, "KeyZ", true)).toBe(true);
    await expect.poll(() => mock.view().then((v) => v.pattern.cells["1"][0])).toBe(0.8);

    // ASCII letters keep their printed meaning even when a physical position is a shortcut.
    expect(await delivered(app, "q", "KeyZ")).toBe(false);
    expect((await mock.view()).pattern.cells["1"][0]).toBe(0.8);
    const bpm = app.getByRole("textbox", { name: "BPM" });
    await bpm.focus();
    expect(await delivered(app, letter, "KeyZ")).toBe(false);
    expect((await mock.view()).pattern.cells["1"][0]).toBe(0.8);
    await mock.eval("(m) => m.queueDialog('/qa/projects/Layout.grooviq')");
    expect(await delivered(app, letter, "KeyS")).toBe(true);
    await expect.poll(() => mock.view().then((v) => [v.path, v.dirty])).toEqual(["/qa/projects/Layout.grooviq", false]);
    expect(await mock.eval<boolean>("(m) => m.files.get('/qa/projects/Layout.grooviq')?.kind === 'project'")).toBe(true);
    await expect(app).toHaveTitle("Layout — Grooviq");
  });
}
