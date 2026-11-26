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

  test("after typing a pattern length and pressing Enter", async ({ app, mock }) => {
    const length = app.getByRole("spinbutton", { name: "Pattern length" });
    await length.fill("12");
    await length.press("Enter");
    await expect.poll(() => mock.view().then((v) => v.pattern.steps)).toBe(12);
    await spaceToggles(app, mock);
  });
});

test("time-signature arrows and Escape leave the project alone until Enter commits once", async ({ app, mock }, info) => {
  await app.setViewportSize({ width: 900, height: 560 });
  await app.evaluate(() => document.fonts.ready.then(() => true));
  const cases = [
    { label: "Beats per bar", finalValue: 7, arrowSteps: 3, beats: 7, unit: 4 },
    { label: "Beat note value", finalValue: 16, arrowSteps: 2, beats: 7, unit: 16 },
  ];
  const edits = async () => (await mock.calls("project_edit")).map((call) => call.args.edit);
  const committed: { kind: "setTimeSignature"; beats: number; unit: number }[] = [];

  for (const { label, finalValue, arrowSteps, beats, unit } of cases) {
    await test.step(label, async () => {
      const trigger = app.getByRole("button", { name: new RegExp(`^${label}:`) });
      const list = app.getByRole("listbox", { name: label, exact: true });
      await expect(trigger).toBeInViewport({ ratio: 1 });

      await trigger.press("Enter");
      await expect(list).toBeFocused();
      await expect(list).toBeInViewport({ ratio: 1 });
      for (let step = 0; step < arrowSteps; step++) await app.keyboard.press("ArrowDown");
      const beforeEnter = await edits();
      await app.keyboard.press("Enter");
      await expect(list).toBeHidden();
      await expect.poll(() => mock.eval("(m) => m.state.project.timeSignature")).toEqual({ beats, unit });
      committed.push({ kind: "setTimeSignature", beats, unit });
      expect(await edits(), `exactly one project edit for ${label}`).toEqual(committed);
      expect(beforeEnter).toEqual(committed.slice(0, -1));
      await expect(trigger).toHaveAccessibleName(`${label}: ${finalValue}`);

      await trigger.press("Enter");
      await expect(list).toBeFocused();
      await app.keyboard.press("ArrowUp");
      await app.keyboard.press("Escape");
      await expect(list).toBeHidden();
      await expect(trigger).toBeFocused();
      expect(await edits()).toEqual(committed);

      await trigger.press("Enter");
      await expect(list).toBeFocused();
      await app.keyboard.press("ArrowUp");
      await app.getByRole("textbox", { name: "BPM" }).focus();
      await expect(list).toBeHidden();
      expect(await edits()).toEqual(committed);
    });
  }

  await app.screenshot({ path: info.outputPath("time-signature-900x560.png") });
});

test("Space chooses a time signature, then plays again when focus leaves the picker", async ({ app, mock }) => {
  const beats = app.getByRole("button", { name: /^Beats per bar:/ });
  const list = app.getByRole("listbox", { name: "Beats per bar" });
  await beats.press("Space");
  await expect(list).toBeFocused();
  await app.keyboard.press("ArrowUp");
  await app.keyboard.press("Space");
  await expect(beats).toHaveAccessibleName("Beats per bar: 3");
  expect((await mock.calls()).filter((call) => call.cmd.startsWith("transport_"))).toEqual([]);
  await app.getByRole("button", { name: "Stop", exact: true }).focus();
  await spaceToggles(app, mock);
});

test.describe("swing while playing (check 10)", () => {
  test("released outside the slider, the swing changes on release", async ({ app, mock }, info) => {
    await app.getByRole("button", { name: "Play" }).click();
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
    await app.getByRole("button", { name: "Play" }).click();
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
