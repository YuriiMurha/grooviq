import { expect, project, soundsFor, test } from "./fixture";

const names = ["Kick", "Snare", "Hat"];
const name = "Late Night Flip — extended arrangement with alternate drums and final mix";
test.use({ seed: { project: project(names, { 1: [0, 4, 8, 12] }, { name }), files: soundsFor(names) }, reducedMotion: "reduce" });

for (const [width, height] of [[900, 560], [1280, 800], [1440, 900], [1920, 1080]]) {
  test(`transport hierarchy and real states at ${width}x${height}`, async ({ app, mock }, info) => {
    await app.setViewportSize({ width, height });
    await app.evaluate(() => document.fonts.ready);
    const toolbar = app.locator("header.toolbar");
    const play = app.getByRole("button", { name: "Play", exact: true });
    const stop = app.getByRole("button", { name: "Stop", exact: true });
    const record = app.getByRole("button", { name: "Record", exact: true });
    const metronome = app.getByRole("button", { name: "Metronome", exact: true });
    const typing = app.getByRole("button", { name: "Typing keyboard", exact: true });
    const title = app.getByRole("button", { name, exact: true });
    await info.attach("toolbar-before-actions", { body: await toolbar.screenshot({ scale: "css" }), contentType: "image/png" });
    const boxes = await Promise.all([play, stop, record, metronome, typing].map((control) => control.boundingBox()));
    if (boxes.some((box) => !box)) throw new Error("Every real transport action must have visible bounds");
    const [p, s, r, m, k] = boxes.map((box) => box!);
    const withinGroup = Math.max(s.x - p.x - p.width, k.x - m.x - m.width);
    const betweenGroups = m.x - r.x - r.width;
    await info.attach("action-spacing", { body: JSON.stringify({ withinGroup, betweenGroups }), contentType: "application/json" });
    expect(withinGroup).toBeGreaterThan(0);
    expect(betweenGroups, "Recording actions and performance toggles need a visible group separation").toBeGreaterThan(withinGroup * 1.5);
    expect(await app.locator(".project-name-text").evaluate((el) => el.clientWidth)).toBeGreaterThan(20);
    expect(await app.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBe(0);
    const bounds = await toolbar.boundingBox();
    if (!bounds) throw new Error("The toolbar must remain visible");
    expect(bounds.height).toBe(56);
    for (const control of [title, play, stop, record, metronome, typing, app.getByRole("button", { name: "Test tone", exact: true })]) {
      const box = await control.boundingBox();
      if (!box) throw new Error("A toolbar control disappeared");
      expect(box.x).toBeGreaterThanOrEqual(bounds.x);
      expect(box.x + box.width).toBeLessThanOrEqual(bounds.x + bounds.width);
      expect(box.y + box.height).toBeLessThanOrEqual(bounds.y + bounds.height);
    }

    await title.click();
    await expect(app.getByRole("menuitem", { name: /^New/ })).toBeFocused();
    await app.keyboard.press("Escape");
    await expect(title).toBeFocused();
    // Continue the real sequential keyboard path from the project opener.
    await app.keyboard.press("Tab");
    await expect(play).toBeFocused();
    expect(await play.evaluate((el) => el.matches(":focus-visible"))).toBe(true);
    await play.press("Enter");
    await expect.poll(() => mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(true);
    await expect(play).toHaveAttribute("aria-pressed", "true");
    await record.click();
    await expect.poll(async () => (await mock.view()).recording).toBe(true);
    await expect(record).toHaveAttribute("aria-pressed", "true");
    await metronome.click();
    await expect.poll(() => mock.eval<boolean>("(m) => m.state.metronome")).toBe(true);
    await expect(metronome).toHaveAttribute("aria-pressed", "true");
    await info.attach("toolbar-applied-states", { body: await toolbar.screenshot({ scale: "css" }), contentType: "image/png" });
    await record.click();
    await stop.click();
    await expect.poll(() => mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(false);

    const mode = app.getByRole("combobox", { name: "Playback mode" });
    await mock.eval("(m) => m.holdCommand('arrangement_mode')");
    await mode.selectOption("song");
    await expect(mode).toBeDisabled();
    await info.attach("toolbar-pending-mode", { body: await toolbar.screenshot({ scale: "css" }), contentType: "image/png" });
    await mock.eval("(m) => m.releaseCommand('arrangement_mode')");
    await expect(mode).toBeEnabled();
    await expect.poll(() => mock.eval<string>("(m) => m.workspaceSnapshot().playMode")).toBe("song");
    await expect(title).toHaveAccessibleName(name);
  });
}
