import type { Page } from "@playwright/test";
import { expect, test } from "./fixture";
import { at, openRoll, sessionNotes, twoChannels } from "./rollHelpers";

// Synthetic events verify delivered-event routing, not OS layout delivery.
async function delivered(page: Page, key: string, code: string, mod = true, altKey = false) {
  return page.evaluate(({ key, code, mod, altKey }) => {
    const mac = /^mac/i.test(navigator.platform);
    const e = new KeyboardEvent("keydown", { key, code, altKey, metaKey: mod && mac, ctrlKey: mod && !mac, bubbles: true, cancelable: true });
    document.body.dispatchEvent(e);
    return e.defaultPrevented;
  }, { key, code, mod, altKey });
}

test.use({ seed: { project: twoChannels([
  { id: 1, channel: 1, tick: 100, length: 200, key: 60, velocity: 0.8 },
  { id: 2, channel: 1, tick: 1130, length: 240, key: 62, velocity: 0.8 },
  { id: 3, channel: 2, tick: 100, length: 240, key: 70, velocity: 0.8 },
]) } });

for (const fallback of [false, true]) {
  test(`${fallback ? "non-ASCII fallback" : "printed letters"} edit actual notes`, async ({ app, mock }) => {
    await openRoll(app);
    // Printed Q at physical A must leave the empty selection alone.
    expect(await delivered(app, "q", "KeyA")).toBe(false);
    await app.keyboard.press("Delete");
    expect(await sessionNotes(mock)).toHaveLength(3);
    expect(await delivered(app, fallback ? "я" : "a", fallback ? "KeyA" : "KeyQ")).toBe(true);
    // Selection is demonstrated by the physical Option+Q transform's note data.
    expect(await delivered(app, "œ", "KeyQ", false, true)).toBe(true);
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 0, 200, 60], [1, 1200, 240, 62], [2, 100, 240, 70]]);
    expect(await delivered(app, fallback ? "Dead" : "c", fallback ? "KeyC" : "KeyI")).toBe(true);
    const p = await at(app, 3000, 66);
    await app.mouse.move(p.x, p.y);
    expect(await delivered(app, fallback ? "." : "v", fallback ? "KeyV" : "Period")).toBe(true);
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 0, 200, 60], [1, 2880, 200, 60], [1, 1200, 240, 62], [1, 4080, 240, 62], [2, 100, 240, 70]]);
    // The pasted selection crosses a bar line, so duplication advances by two bars.
    expect(await delivered(app, fallback ? "Unidentified" : "b", fallback ? "KeyB" : "KeyX")).toBe(true);
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 0, 200, 60], [1, 2880, 200, 60], [1, 10560, 200, 60], [1, 1200, 240, 62], [1, 4080, 240, 62], [1, 11760, 240, 62], [2, 100, 240, 70]]);
    await app.keyboard.press("Backspace");
    await expect.poll(() => sessionNotes(mock).then((n) => n.length)).toBe(5);
  });
}

test("editing exclusions leave note selection unchanged", async ({ app, mock }) => {
  await openRoll(app);
  for (const kind of ["input", "editable", "dialog", "rack"]) {
    const prevented = await app.evaluate((kind) => {
      const container = document.createElement("div");
      if (kind === "dialog") { container.setAttribute("role", "dialog"); container.setAttribute("aria-modal", "true"); }
      if (kind === "rack") container.className = "panel-rack";
      const target = document.createElement(kind === "input" ? "input" : "div");
      if (kind === "editable") target.contentEditable = "true";
      container.append(target);
      document.body.append(container);
      const mac = /^mac/i.test(navigator.platform);
      const event = new KeyboardEvent("keydown", { key: "a", code: "KeyQ", metaKey: mac, ctrlKey: !mac, bubbles: true, cancelable: true });
      target.dispatchEvent(event);
      container.remove();
      return event.defaultPrevented;
    }, kind);
    expect(prevented).toBe(false);
    await app.keyboard.press("Delete");
    expect(await sessionNotes(mock)).toHaveLength(3);
  }
  await delivered(app, "a", "KeyQ");
  await app.keyboard.press("Delete");
  await expect.poll(() => sessionNotes(mock)).toEqual([[2, 100, 240, 70]]);
});
