import { expect, project, soundsFor, test } from "./fixture";

const missing = project(["Reference", "Other"]);
missing.channels[0].embedded = false;
test.use({ seed: { project: missing, files: soundsFor(["Recovered", "Other"]) } });

for (const expanded of [false, true]) test(`successful pointer relink keeps Escape with details ${expanded ? "open" : "closed"}`, async ({ app, mock }) => {
  const opener = app.getByRole("button", { name: "Edit the Reference sampler", exact: true });
  await opener.click();
  const dialog = app.getByRole("dialog", { name: "Reference sampler", exact: true });
  if (expanded) await dialog.getByText("File details", { exact: true }).click();
  await mock.eval('m => m.queueDialog("/qa/sounds/Recovered.wav")');
  await dialog.getByRole("button", { name: "Relink Reference", exact: true }).click();
  await expect(dialog.locator(".sample-name")).toHaveText("Recovered.wav");
  await expect(dialog.getByRole("img", { name: "Reference waveform", exact: true })).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Relink Reference", exact: true })).toHaveCount(0);
  await expect(dialog.locator(".sample-file-details")).toHaveJSProperty("open", expanded);
  await app.keyboard.press("Space");
  await expect.poll(() => mock.eval<boolean>("m => m.state.transport.playing")).toBe(true);
  await app.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(opener).toBeFocused();
});

for (const destination of ["BPM", "Other", "closed"]) test(`delayed success preserves focus at ${destination}`, async ({ app, mock }) => {
  if (destination === "Other") await app.getByRole("button", { name: "Edit the Other sampler", exact: true }).click();
  await app.getByRole("button", { name: "Edit the Reference sampler", exact: true }).click();
  const dialog = app.getByRole("dialog", { name: "Reference sampler", exact: true });
  await mock.eval('m => { m.queueDialog("/qa/sounds/Recovered.wav"); m.holdCommand("plugin:dialog|open"); }');
  await dialog.getByRole("button", { name: "Relink Reference", exact: true }).click();
  await expect.poll(async () => (await mock.calls("plugin:dialog|open")).length).toBe(1);
  const target = destination === "Other" ? app.getByRole("button", { name: "Close the Other sampler", exact: true }) : app.getByRole("textbox", { name: "BPM", exact: true });
  if (destination === "Other") await target.focus();
  else {
    if (destination === "closed") {
      await dialog.getByRole("button", { name: "Close the Reference sampler", exact: true }).click();
      await expect(app.getByRole("button", { name: "Edit the Reference sampler", exact: true })).toBeFocused();
    }
    await target.focus();
  }
  await expect(target).toBeFocused();
  await mock.eval('m => m.releaseCommand("plugin:dialog|open")');
  await expect.poll(async () => (await mock.calls("channel_set_sample")).length).toBe(1);
  if (destination !== "closed") await expect(dialog.getByRole("img", { name: "Reference waveform", exact: true })).toBeVisible();
  await expect(target).toBeFocused();
  if (destination !== "Other") {
    await target.fill("137");
    await app.keyboard.press("Space");
    expect(await mock.eval<boolean>("m => m.state.transport.playing")).toBe(false);
    await app.keyboard.press("Enter");
    await expect.poll(async () => (await mock.view()).bpm).toBe(137);
  }
});


test("project replacement fences a late picker success", async ({ app, mock }) => {
  await app.getByRole("button", { name: "Edit the Reference sampler", exact: true }).click();
  await mock.eval('m => { m.queueDialog("/qa/sounds/Recovered.wav"); m.holdCommand("plugin:dialog|open"); }');
  await app.getByRole("button", { name: "Relink Reference", exact: true }).click();
  await expect.poll(async () => (await mock.calls("plugin:dialog|open")).length).toBe(1);
  await app.keyboard.press("ControlOrMeta+n");
  await expect.poll(async () => (await mock.calls("project_new")).length).toBe(1);
  await expect(app.getByRole("dialog", { name: "Reference sampler", exact: true })).toHaveCount(0);
  const tempo = app.getByRole("textbox", { name: "BPM", exact: true });
  await tempo.focus();
  await mock.eval('m => m.releaseCommand("plugin:dialog|open")');
  await app.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
  await expect(tempo).toBeFocused();
  expect(await mock.calls("channel_set_sample")).toEqual([]);
  expect((await mock.view()).channels).toEqual([]);
});
