import { expect, project, soundsFor, test } from "./fixture";

const missing = project(["Hat"]);
missing.channels[0].embedded = false;
test.use({ seed: { project: missing, files: soundsFor(["Hat"]) } });

for (const expanded of [false, true]) test(`a cancelled pointer Relink keeps Escape with file details ${expanded ? "open" : "closed"}`, async ({ app, mock }) => {
  const opener = app.getByRole("button", { name: "Edit the Hat sampler", exact: true });
  await opener.click();
  const dialog = app.getByRole("dialog", { name: "Hat sampler", exact: true });
  const relink = dialog.getByRole("button", { name: "Relink Hat", exact: true });
  await expect(relink).toBeVisible();
  if (expanded) await dialog.getByText("File details", { exact: true }).click();
  const before = await mock.view();
  await mock.eval("m => m.queueDialog(null)");
  await relink.click();
  await expect.poll(async () => (await mock.calls("plugin:dialog|open")).length).toBe(1);
  await expect(relink).toBeFocused();
  expect(await mock.calls("channel_set_sample")).toEqual([]);
  expect(await mock.view()).toEqual(before);
  await app.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(opener).toBeFocused();
});

test("a delayed picker cancellation does not take focus from another control", async ({ app, mock }) => {
  await app.getByRole("button", { name: "Edit the Hat sampler", exact: true }).click();
  const dialog = app.getByRole("dialog", { name: "Hat sampler", exact: true });
  await mock.eval("m => { m.queueDialog(null); m.holdCommand('plugin:dialog|open'); }");
  await dialog.getByRole("button", { name: "Relink Hat", exact: true }).click();
  await expect.poll(async () => (await mock.calls("plugin:dialog|open")).length).toBe(1);
  const tempo = app.getByRole("textbox", { name: "BPM", exact: true });
  await tempo.focus();
  await mock.eval("m => m.releaseCommand('plugin:dialog|open')");
  await app.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
  await expect(tempo).toBeFocused();
  expect(await mock.calls("channel_set_sample")).toEqual([]);
  await expect(dialog).toBeVisible();
});

test("keyboard Relink cancellation keeps its focus and transport shortcuts", async ({ app, mock }) => {
  await app.getByRole("button", { name: "Edit the Hat sampler", exact: true }).click();
  const dialog = app.getByRole("dialog", { name: "Hat sampler", exact: true });
  const relink = dialog.getByRole("button", { name: "Relink Hat", exact: true });
  await mock.eval("m => m.queueDialog(null)");
  await relink.focus();
  await app.keyboard.press("Enter");
  await expect.poll(async () => (await mock.calls("plugin:dialog|open")).length).toBe(1);
  await expect(relink).toBeFocused();
  await app.keyboard.press("Space");
  await expect.poll(() => mock.eval<boolean>("m => m.state.transport.playing")).toBe(true);
  await app.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
});
