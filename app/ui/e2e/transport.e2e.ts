// Owner checklist 9, 10 and 12: Space after the transport fields, swing released outside, and
// the pattern length changed while playing.
import type { Page } from "@playwright/test";
import { expect, position, project, soundsFor, test, type Mock } from "./fixture";

const KIT = ["Kick", "Snare", "Hat"];
test.use({ seed: { project: project(KIT, { 1: [0, 4, 8, 12], 3: [2, 6, 10, 14] }), files: soundsFor(KIT) } });

const playing = (mock: Mock) => mock.eval<boolean>("(m) => m.state.transport.playing");

/** Space toggles playback twice from where focus is now. */
async function spaceToggles(page: Page, mock: Mock) {
  const before = await playing(mock);
  await page.keyboard.press("Space");
  await expect.poll(() => playing(mock)).toBe(!before);
  await page.keyboard.press("Space");
  await expect.poll(() => playing(mock)).toBe(before);
}

test.describe("Space after the transport fields (check 9)", () => {
  test("after typing a BPM and pressing Enter", async ({ app, mock }) => {
    const bpm = app.getByRole("textbox", { name: "BPM" });
    await bpm.click();
    await bpm.fill("128");
    await bpm.press("Enter");
    await expect(bpm).not.toBeFocused();
    await expect.poll(() => mock.view().then((v) => v.bpm)).toBe(128);
    await spaceToggles(app, mock);
    // The tempo field kept its value: Space typed nothing into it.
    await expect(bpm).toHaveValue("128.00");
  });

  test("after moving the swing slider", async ({ app, mock }) => {
    const swing = app.getByRole("slider", { name: "Swing" });
    await swing.focus();
    await app.keyboard.press("ArrowRight");
    await app.keyboard.press("ArrowRight");
    await expect.poll(() => mock.view().then((v) => v.pattern.swing)).toBeCloseTo(0.02);
    await spaceToggles(app, mock);
    // Space did not move the slider either.
    await expect(swing).toHaveValue("2");
  });

  test("after picking 3/4", async ({ app, mock }) => {
    const beats = app.getByRole("combobox", { name: "Beats per bar" });
    await beats.selectOption("3");
    await expect.poll(() => mock.eval<number>("(m) => m.state.project.timeSignature.beats")).toBe(3);
    await beats.focus();
    await spaceToggles(app, mock);
    await expect(beats).toHaveValue("3");
  });

  test("after typing a pattern length and pressing Enter", async ({ app, mock }) => {
    const length = app.getByRole("spinbutton", { name: "Pattern length" });
    await length.fill("12");
    await length.press("Enter");
    await expect.poll(() => mock.view().then((v) => v.pattern.steps)).toBe(12);
    await spaceToggles(app, mock);
  });
});

test.describe("swing while playing (check 10)", () => {
  test("released outside the slider, the swing changes on release", async ({ app, mock }, info) => {
    await app.getByRole("button", { name: "Play", exact: true }).click();
    const swing = app.getByRole("slider", { name: "Swing" });
    const box = await swing.boundingBox();
    if (!box) throw new Error("no slider");
    const y = box.y + box.height / 2;
    await app.mouse.move(box.x + 2, y);
    await app.mouse.down();
    await app.mouse.move(box.x + box.width * 0.4, y, { steps: 6 });
    // Off the slider, down into the rack, then let go there.
    await app.mouse.move(box.x + box.width * 0.4, y + 160, { steps: 6 });
    // Moved, not yet released: nothing sent.
    const setSwing = async () =>
      (await mock.calls("project_edit")).filter((c) => (c.args.edit as { kind: string }).kind === "setSwing");
    expect(await setSwing()).toHaveLength(0);
    await app.mouse.up();
    await app.screenshot({ path: info.outputPath("swing-released-outside.png") });
    await expect
      .poll(() => mock.view().then((v) => v.pattern.swing), { timeout: 2000, message: "setSwing sent on release" })
      .toBeGreaterThan(0.2);
    expect(await setSwing()).toHaveLength(1);
    expect(await playing(mock)).toBe(true);
  });
});

test.describe("pattern length while playing (check 12)", () => {
  test("16 → 32 → 15 steps keeps playing without a restart", async ({ app, mock }, info) => {
    await app.getByRole("button", { name: "Play", exact: true }).click();
    await expect.poll(() => mock.eval<number>("(m) => m.ticks()")).toBeGreaterThan(6 * 240);
    const length = app.getByRole("spinbutton", { name: "Pattern length" });
    for (const steps of [32, 15]) {
      const before = await mock.eval<number>("(m) => m.ticks()");
      await length.fill(String(steps));
      await length.press("Enter");
      await expect.poll(() => mock.view().then((v) => v.pattern.steps)).toBe(steps);
      // The transport was not stopped or restarted, and the position only moved forward.
      expect(await playing(mock)).toBe(true);
      expect(await mock.eval<number>("(m) => m.ticks()")).toBeGreaterThanOrEqual(before);
      await expect(position(app)).not.toHaveText("1.1.01");
      await app.getByRole("region", { name: "Channel rack" }).screenshot({ path: info.outputPath(`length-${steps}.png`) });
    }
    const transport = (await mock.calls()).filter((c) => c.cmd.startsWith("transport_"));
    expect(transport.map((c) => c.cmd)).toEqual(["transport_play"]);
  });
});
