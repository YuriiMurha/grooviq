// M2 Task 8: MIDI keyboards in the output popover. The notes themselves go from the keyboard to
// the engine without touching the UI (app/src-tauri/tests/midi.rs covers them), so what the UI
// shows is the list of keyboards and their switches.
import type { Page } from "@playwright/test";
import { expect, test, type Mock } from "./fixture";

const openPopover = async (page: Page) => {
  await page.getByRole("button", { name: /Silent \(QA mode\)/ }).click();
  await expect(page.getByRole("dialog", { name: "Audio output" })).toBeVisible();
};
const keyboard = (page: Page, name: string) => page.getByRole("switch", { name });
const disabled = (mock: Mock) => mock.eval<string[]>("(m) => m.midiDisabled()");

test.describe("MIDI keyboards", () => {
  test.use({ seed: { midi: { inputs: ["KeyStep 37", "Launchpad"], disabled: ["Launchpad"] } } });

  test("each keyboard has a switch that is saved, and the list follows plugging and unplugging", async ({ app, mock }) => {
    await openPopover(app);
    // On unless the person switched it off earlier.
    await expect(keyboard(app, "KeyStep 37")).toBeChecked();
    await expect(keyboard(app, "Launchpad")).not.toBeChecked();

    await keyboard(app, "KeyStep 37").click();
    await expect(keyboard(app, "KeyStep 37")).not.toBeChecked();
    await expect.poll(() => disabled(mock)).toEqual(["Launchpad", "KeyStep 37"]);
    await keyboard(app, "Launchpad").click();
    await expect(keyboard(app, "Launchpad")).toBeChecked();
    await expect.poll(() => disabled(mock)).toEqual(["KeyStep 37"]);

    // A keyboard plugged in while the popover is open shows up without reopening it.
    await app.evaluate("window.__grooviqMock.midiPlug('Minilab')");
    await expect(keyboard(app, "Minilab")).toBeChecked();
    await app.evaluate("window.__grooviqMock.midiUnplug('KeyStep 37')");
    await expect(keyboard(app, "KeyStep 37")).toHaveCount(0);

    // The choice is the shell's: closing and reopening the popover shows it again, and an
    // unplugged keyboard that was switched off comes back off.
    await app.keyboard.press("Escape");
    await expect(app.getByRole("dialog", { name: "Audio output" })).toHaveCount(0);
    await app.evaluate("window.__grooviqMock.midiPlug('KeyStep 37')");
    await openPopover(app);
    await expect(keyboard(app, "KeyStep 37")).not.toBeChecked();
    await expect(keyboard(app, "Launchpad")).toBeChecked();
  });
});

test.describe("no MIDI keyboard", () => {
  test("the popover lists no switch, and a keyboard plugged in gets a working one", async ({ app, mock }) => {
    await openPopover(app);
    // Nothing plugged in: no switch at all.
    await expect(app.getByRole("switch")).toHaveCount(0);
    await app.evaluate("window.__grooviqMock.midiPlug('KeyStep 37')");
    await expect(keyboard(app, "KeyStep 37")).toBeChecked();
    await expect(app.getByRole("switch")).toHaveCount(1);
    // And the new keyboard's switch works.
    await keyboard(app, "KeyStep 37").click();
    await expect(keyboard(app, "KeyStep 37")).not.toBeChecked();
    await expect.poll(() => disabled(mock)).toEqual(["KeyStep 37"]);
  });
});
