import { expect, project, test } from "./fixture";

test.use({ seed: { project: project(["Kick"], { 1: [0] }), exportMs: 100 } });

test("prompt failure surfaces, cancellation stays quiet, and retry retains options", async ({ app, mock }) => {
  await app.keyboard.press("ControlOrMeta+r");
  const dialog = app.getByRole("dialog", { name: "Export WAV" });
  await dialog.getByLabel("32-bit float", { exact: true }).check();
  await dialog.getByLabel("96 kHz", { exact: true }).check();
  await dialog.getByRole("spinbutton", { name: "Loops" }).fill("3");
  await mock.eval("(m) => m.refuseNext('plugin:dialog|save', 'File prompt unavailable: permission denied')");
  await dialog.getByRole("button", { name: "Export…" }).click();
  await expect(dialog.getByRole("alert")).toHaveText("File prompt unavailable: permission denied");
  expect(await mock.calls("project_export")).toHaveLength(0);
  await mock.eval("(m) => m.queueDialog(null)");
  await dialog.getByRole("button", { name: "Export…" }).click();
  await expect(dialog.getByRole("alert")).toHaveCount(0);
  expect(await mock.calls("project_export")).toHaveLength(0);
  expect(await mock.eval("(m) => [...m.files.keys()].filter(p => p.startsWith('/qa/out/'))")).toEqual([]);
  await expect(dialog.getByLabel("32-bit float", { exact: true })).toBeChecked();
  await expect(dialog.getByLabel("96 kHz", { exact: true })).toBeChecked();
  await expect(dialog.getByRole("spinbutton", { name: "Loops" })).toHaveValue("3");
  await mock.eval("(m) => m.queueDialog('/qa/out/retry.wav')");
  await dialog.getByRole("button", { name: "Export…" }).click();
  await expect(dialog.getByRole("button", { name: "Done", exact: true })).toBeVisible();
  expect((await mock.calls("project_export"))[0].args).toEqual({ path: "/qa/out/retry.wav", options: { sampleRate: 96000, format: "float32", loops: 3 } });
  expect(await mock.eval("(m) => m.file('/qa/out/retry.wav')")).toMatchObject({ kind: "sample" });
});

test("closing a held prompt cannot export into the reopened dialog", async ({ app, mock }) => {
  await app.keyboard.press("ControlOrMeta+r");
  const dialog = app.getByRole("dialog", { name: "Export WAV" });
  await mock.eval("(m) => { m.queueDialog('/qa/out/stale.wav'); m.holdCommand('plugin:dialog|save'); }");
  await dialog.getByRole("button", { name: "Export…" }).click();
  await expect.poll(() => mock.calls("plugin:dialog|save")).toHaveLength(1);
  await dialog.getByRole("button", { name: "Close", exact: true }).click();
  await app.keyboard.press("ControlOrMeta+r");
  await expect(dialog.getByRole("button", { name: "Export…" })).toBeVisible();
  await mock.eval("(m) => m.releaseCommand('plugin:dialog|save')");
  // A subsequent successful prompt/export is a barrier past the old prompt's continuation.
  await mock.eval("(m) => m.queueDialog('/qa/out/fresh.wav')");
  await dialog.getByRole("button", { name: "Export…" }).click();
  await expect(dialog.getByRole("button", { name: "Done", exact: true })).toBeVisible();
  expect((await mock.calls("project_export")).map(c => c.args.path)).toEqual(["/qa/out/fresh.wav"]);
  expect(await mock.eval("(m) => m.file('/qa/out/stale.wav')")).toBeNull();
});
