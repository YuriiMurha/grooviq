import { test, expect } from "./fixture";

test.use({ seed: { holdAudioLog: true, audioLog: [
  { id: 1, atMs: 7, level: "warn", message: "qa-startup-a" },
  { id: 2, atMs: 7, level: "info", message: "qa-startup-info" },
  { id: 3, atMs: 7, level: "error", message: "qa-overlap" },
] } });

test("startup snapshot and concurrent live events each toast once", async ({ app, mock }) => {
  const messages = app.getByRole("status", { name: "Messages" });
  await expect.poll(async () => (await mock.calls("audio_log")).length).toBeGreaterThan(0);
  await mock.eval(`m => m.emit("audio-event", { id: 3, kind: "no-device", message: "qa-overlap" })`);
  await expect(messages.getByText("qa-overlap", { exact: true })).toHaveCount(1);
  await mock.eval(`m => m.releaseCommand("audio_log")`);
  await expect(messages.getByText("qa-startup-a", { exact: true })).toHaveCount(1);
  await expect(messages.getByText("qa-startup-info", { exact: true })).toHaveCount(0);
  await expect(messages.getByText("qa-overlap", { exact: true })).toHaveCount(1);
  await mock.eval(`m => { for (const id of [4, 5]) m.emit("audio-event", { id, kind: "no-device", message: "qa-repeat" }); }`);
  await expect(messages.getByText("qa-repeat", { exact: true })).toHaveCount(2);
  await messages.getByRole("button", { name: "Dismiss" }).first().click();
  await mock.eval(`m => m.emit("audio-event", { id: 3, kind: "no-device", message: "qa-overlap" })`);
  await expect(messages.getByText("qa-overlap", { exact: true })).toHaveCount(0);
  await app.locator("button.project-name").click();
  await app.getByRole("menuitem", { name: /^New/ }).click();
  await expect.poll(async () => (await mock.calls("project_new")).length).toBe(1);
  await expect(messages.getByText("qa-overlap", { exact: true })).toHaveCount(0);
  await expect(messages.getByText("qa-startup-a", { exact: true })).toHaveCount(1);
});

test("failed startup history leaves live warnings connected", async ({ app, mock }) => {
  await expect.poll(async () => (await mock.calls("audio_log")).length).toBeGreaterThan(0);
  await mock.eval(`m => { m.refuseNext("audio_log", "qa-query-failure"); m.releaseCommand("audio_log"); }`);
  await mock.eval(`m => m.emit("audio-event", { id: 9, kind: "no-device", message: "qa-after-failure" })`);
  await expect(app.getByRole("status", { name: "Messages" }).getByText("qa-after-failure", { exact: true })).toHaveCount(1);
});
