import { expect, test } from "./fixture";
import { project, soundsFor } from "./fixture";

const KIT = ["Kick", "Snare", "Hat"];
test.use({ seed: { project: project(KIT, { 1: [0] }), files: soundsFor(KIT) } });

test("a post-rename directory warning adopts the written path, stays dirty, and Save retries in place", async ({ app, mock }, info) => {
  await app.getByRole("button", { name: "Mute Kick" }).click();
  await expect.poll(() => mock.view().then((v) => v.dirty)).toBe(true);
  await mock.eval("(m) => m.queueDialog('/qa/projects/Closing')");
  await mock.eval("(m) => m.unconfirmNextSave()");
  await mock.eval("(m) => m.requestClose()");
  await app.getByRole("button", { name: "Save", exact: true }).click();
  const warn = app.getByRole("status", { name: "Messages" }).locator(".toast.warn");
  await expect(warn).toContainText("directory durability could not be confirmed");
  await expect(app).toHaveTitle("• Closing — Grooviq");
  expect(await mock.eval<number>("(m) => m.state.exitCount")).toBe(0);
  await app.screenshot({ path: info.outputPath("unconfirmed.png") });
  await app.keyboard.press("ControlOrMeta+s");
  await expect.poll(() => mock.view().then((v) => v.dirty)).toBe(false);
  await expect(app).toHaveTitle("Closing — Grooviq");
  expect(await mock.calls("plugin:dialog|save")).toHaveLength(1);
  expect(await mock.calls("project_save")).toHaveLength(1);
});

test("an in-place Save warning refreshes a clean titled project to dirty", async ({ app, mock }) => {
  await mock.eval("(m) => m.queueDialog('/qa/projects/Titled')");
  await app.keyboard.press("ControlOrMeta+s");
  await expect(app).toHaveTitle("Titled — Grooviq");
  await expect.poll(() => mock.view().then((v) => v.dirty)).toBe(false);

  await mock.eval("(m) => m.unconfirmNextSave()");
  await app.keyboard.press("ControlOrMeta+s");
  const warn = app.getByRole("status", { name: "Messages" }).locator(".toast.warn");
  await expect(warn).toContainText("directory durability could not be confirmed");
  await expect.poll(() => mock.view().then((v) => v.dirty)).toBe(true);
  await expect(app).toHaveTitle("• Titled — Grooviq");
  expect(await mock.calls("plugin:dialog|save")).toHaveLength(1);
  expect(await mock.calls("project_save")).toHaveLength(1);
});
