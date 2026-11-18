import { test, expect, project, soundsFor } from "./fixture";

const beat = project(["Kick", "Missing Snare", "Hat"], { 2: [4] });
beat.channels[1].embedded = false;
const files = soundsFor(["Kick", "Hat", "Recovered"]);
test.use({ seed: { project: beat, files } });

test("missing sample preserves rack geometry and recovery stays inside its sampler", async ({ app, mock }, info) => {
  const rows = ["Kick", "Missing Snare", "Hat"].map((name) => app.getByRole("group", { name, exact: true }));
  const gridBefore = await app.locator('canvas[aria-label="Step grid"]').boundingBox();
  const solo = await Promise.all(rows.map((row, i) => row.getByRole("button", { name: `Solo ${beat.channels[i].name}`, exact: true }).boundingBox()));
  expect(solo.map((box) => box!.x)).toEqual([solo[0]!.x, solo[0]!.x, solo[0]!.x]);
  for (const control of ["volume", "pan"]) {
    const boxes = await Promise.all(rows.map((row, i) => row.getByRole("slider", { name: `${beat.channels[i].name} ${control}`, exact: true }).boundingBox()));
    expect(boxes[1]!.x).toBe(boxes[0]!.x);
    expect(boxes[2]!.x).toBe(boxes[0]!.x);
  }
  await app.getByRole("button", { name: "Edit the Missing Snare sampler" }).click();
  const editor = app.getByRole("dialog", { name: "Missing Snare sampler", exact: true });
  await editor.getByText("File details", { exact: true }).click();
  await expect(editor.locator(".sample-file-details")).toContainText("Missing Snare.wav");
  await mock.eval("(m) => m.queueDialog(null)");
  await editor.getByRole("button", { name: "Relink Missing Snare" }).click();
  await expect.poll(async () => (await mock.calls("plugin:dialog|open")).length).toBe(1);
  expect(await mock.calls("channel_set_sample")).toHaveLength(0);
  await mock.eval("(m) => m.queueDialog('/qa/sounds/Recovered.wav')");
  await editor.getByRole("button", { name: "Relink Missing Snare" }).click();
  await expect.poll(async () => (await mock.calls("channel_set_sample")).length).toBe(1);
  expect((await mock.calls("channel_set_sample"))[0].args).toMatchObject({ channel: 2, path: "/qa/sounds/Recovered.wav" });
  await expect(editor.locator(".sample-file-details")).toContainText("Recovered.wav");
  expect(await mock.eval("(m) => m.state.project.channels.map(c => c.sample)")).toEqual(["/qa/sounds/Kick.wav", "/qa/sounds/Recovered.wav", "/qa/sounds/Hat.wav"]);
  expect(await mock.eval("(m) => m.view().channels.map(c => c.sampleName)")).toEqual(["Kick.wav", "Recovered.wav", "Hat.wav"]);
  expect((await app.locator('canvas[aria-label="Step grid"]').boundingBox())!.x).toBe(gridBefore!.x);
  expect(await mock.eval("(m) => m.state.project.pattern.notes.map(n => ({ channel: n.channel, tick: n.tick }))")).toEqual([{ channel: 2, tick: 960 }]);
  await app.screenshot({ path: info.outputPath("sampler-files.png") });
});

test("late sampler picker cannot relink a replacement project", async ({ app, mock }) => {
  await app.getByRole("button", { name: "Edit the Missing Snare sampler" }).click();
  await mock.eval("(m) => { m.queueDialog('/qa/sounds/Recovered.wav'); m.holdCommand('plugin:dialog|open'); }");
  await app.getByRole("button", { name: "Relink Missing Snare" }).click();
  await expect.poll(async () => (await mock.calls("plugin:dialog|open")).length).toBe(1);
  await app.keyboard.press("ControlOrMeta+n");
  await expect.poll(async () => (await mock.view()).channels.length).toBe(0);
  await mock.eval("(m) => m.drag(['/qa/sounds/Kick.wav', '/qa/sounds/Hat.wav'], true)");
  await expect.poll(async () => (await mock.view()).channels.length).toBe(2);
  const before = await mock.eval("(m) => m.state.project");
  await mock.eval("(m) => m.releaseCommand('plugin:dialog|open')");
  // Drain the dialog's promise and the project's serialized edit queue.
  await app.evaluate(() => new Promise<void>((resolve) => setTimeout(resolve, 100)));
  expect(await mock.calls("channel_set_sample")).toHaveLength(0);
  expect(await mock.eval("(m) => m.state.project")).toEqual(before);
});

test.describe("long logical filename", () => {
  const name = `${"Long sample name ".repeat(20)}.wav`;
  const longBeat = project(["Kick"]);
  longBeat.channels[0].sample = `/qa/sounds/${name}`;
  longBeat.channels[0].frames = 12345;
  test.use({ seed: { project: longBeat, files: { [`/qa/sounds/${name}`]: { kind: "sample" } } } });
  test("details expand by keyboard inside the plugin", async ({ app }, info) => {
    const rack = await app.locator('.channel-row').boundingBox();
    await app.getByRole("button", { name: "Edit the Kick sampler" }).click();
    const editor = app.getByRole("dialog", { name: "Kick sampler", exact: true });
    const summary = editor.locator("summary");
    await summary.focus();
    await app.keyboard.press("Enter");
    await expect(editor.locator("dd").first()).toHaveText(name);
    await expect(editor.locator("dd").last()).toHaveText("12,345 frames");
    const fit = await editor.locator(".sample-file-details").evaluate((el) => ({ width: el.clientWidth, scroll: el.scrollWidth }));
    expect(fit.scroll).toBeLessThanOrEqual(fit.width);
    expect(await app.locator('.channel-row').boundingBox()).toEqual(rack);
    await app.screenshot({ path: info.outputPath("long-file.png") });
  });
});
