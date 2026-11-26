import type { Page } from "@playwright/test";
import { cellPoint, colourDistance, expect, grid, pixelsAt, project, soundsFor, test, workspaceReady } from "./fixture";
import type { MockNote } from "./mockTauri";
import { at, canvas, lane, local, settled, token } from "./rollHelpers";
import { REQUIRED_THEME_TOKENS, themeTokenProblems } from "../src/theme/contract";
import { themes, THEME_PREFERENCE_KEY } from "../src/theme/preference";

const beat = project(["Lead", "Kick"], { 2: [1] });
// Both editors use the first ink, with one sequenced channel and one melodic channel.
beat.channels[1].color = 0;
beat.pattern.notes.push(
  { id: 2, channel: 1, tick: 240, length: 240, key: 60, velocity: 1 },
  { id: 3, channel: 1, tick: 960, length: 240, key: 62, velocity: 1 },
);

test.use({ seed: { project: beat, files: soundsFor(["Lead", "Kick"]) } });

async function openAppearance(page: Page) {
  const dialog = page.getByRole("dialog", { name: "Settings" });
  await page.getByRole("button", { name: /Silent \(QA mode\)/ }).click();
  await expect(dialog).toBeVisible();
  return dialog.getByRole("radiogroup", { name: "Appearance" });
}

test("selecting each theme repaints both mounted editors and survives a reload", async ({ app, mock }, info) => {
  test.slow();
  await app.keyboard.press("F7");
  const roll = canvas(app);
  const steps = grid(app);
  const velocity = lane(app);
  await expect(roll).toBeVisible();
  await expect(steps).toBeVisible();
  await settled(app);

  const notes = async () => (await mock.eval<Required<MockNote>[]>("(m) => m.notes()")).sort((a, b) => a.id - b.id);
  const beforeEdit = await notes();
  const added = await at(app, 1690, 64);
  await app.mouse.click(added.x, added.y);
  await expect.poll(async () => (await notes()).length).toBe(beforeEdit.length + 1);
  await expect.poll(async () => (await mock.calls("edit_history_enabled")).at(-1)?.args).toEqual({ undo: true, redo: false });
  const selected = await at(app, 360, 60);
  await app.mouse.click(selected.x, selected.y);

  const rollPoint = await local(app, 1080, 62);
  const selectedBar = await local(app, 240, 60);
  const barPoint = { x: selectedBar.x + 1, y: 30 };
  const storedNotes = await notes();
  const view = await mock.view();
  const gridNode = await steps.elementHandle();
  const rollNode = await roll.elementHandle();
  const html = app.locator("html");

  // Start with a different theme so the loop also exercises switching back to the default.
  const initial = themes[1];
  let appearance = await openAppearance(app);
  await expect(appearance.getByRole("radio")).toHaveCount(themes.length);
  for (const [index, { name }] of themes.entries()) {
    await expect(appearance.getByRole("radio").nth(index)).toHaveAccessibleName(name);
  }
  await expect(appearance.getByRole("radio", { name: themes[0].name })).toBeChecked();
  await appearance.getByRole("radio", { name: initial.name }).click();
  await expect(html).toHaveAttribute("data-theme", initial.id);

  for (const { id, name } of themes) {
    const option = appearance.getByRole("radio", { name, exact: true });
    await option.click();
    await expect(option).toBeChecked();
    await expect(html).toHaveAttribute("data-theme", id);
    const values = await app.evaluate((names) => {
      const style = getComputedStyle(document.documentElement);
      return Object.fromEntries(names.map((token) => [token, style.getPropertyValue(token)]));
    }, REQUIRED_THEME_TOKENS);
    expect(themeTokenProblems((name) => values[name] ?? "")).toEqual([]);

    await app.screenshot({ path: info.outputPath(`${id}.png`), scale: "css", animations: "disabled" });
    await app.keyboard.press("Escape");
    await expect(appearance).toBeHidden();
    const ink = await token(app, "--ink-1");
    const selection = await token(app, "--roll-select");
    await expect.poll(async () => {
      const [stepPixel] = await pixelsAt(app, steps, [cellPoint(1, 1)]);
      const [notePixel] = await pixelsAt(app, roll, [rollPoint]);
      const [selectedBarPixel] = await pixelsAt(app, velocity, [barPoint]);
      return Math.max(colourDistance(stepPixel, ink), colourDistance(notePixel, ink), colourDistance(selectedBarPixel, selection));
    }, { message: `${name} repaints the lit step, roll note and selected velocity bar` }).toBeLessThan(8);
    expect(await steps.evaluate((node, before) => node === before, gridNode)).toBe(true);
    expect(await roll.evaluate((node, before) => node === before, rollNode)).toBe(true);
    expect(await notes()).toEqual(storedNotes);
    expect(await mock.view()).toMatchObject({ notesRevision: view.notesRevision, canUndo: view.canUndo, canRedo: view.canRedo });
    await expect.poll(async () => (await mock.calls("edit_history_enabled")).at(-1)?.args).toEqual({ undo: true, redo: false });
    appearance = await openAppearance(app);
  }

  const last = themes[themes.length - 1];
  expect(await app.evaluate((key) => localStorage.getItem(key), THEME_PREFERENCE_KEY)).toBe(last.id);

  await app.keyboard.press("Escape");
  await mock.eval("(m) => m.emit('edit-history', false)");
  await expect.poll(notes).toEqual(beforeEdit);
  await mock.eval("(m) => m.emit('edit-history', true)");
  await expect.poll(notes).toEqual(storedNotes);

  await app.reload();
  await workspaceReady(app);
  await expect(html).toHaveAttribute("data-theme", last.id);
  appearance = await openAppearance(app);
  await expect(appearance.getByRole("radio", { name: last.name })).toBeChecked();
});

test("keyboard selection still applies with storage denied and the notice stays visible on reopening", async ({ app }) => {
  await app.addInitScript(() => {
    Object.defineProperty(window, "localStorage", {
      get: () => { throw new DOMException("Blocked", "SecurityError"); },
    });
  });
  await app.reload();
  await workspaceReady(app);
  const appearance = await openAppearance(app);
  const first = appearance.getByRole("radio", { name: themes[0].name });
  await expect(first).toBeChecked();
  await first.focus();
  await app.keyboard.press("ArrowRight");
  await expect(app.locator("html")).toHaveAttribute("data-theme", themes[1].id);
  const second = appearance.getByRole("radio", { name: themes[1].name });
  await expect(second).toBeFocused();
  await expect(second).toBeChecked();
  await expect(appearance.getByRole("status")).toBeVisible();

  await app.keyboard.press("Escape");
  await openAppearance(app);
  await expect(second).toBeChecked();
  await expect(appearance.getByRole("status")).toBeVisible();
});
