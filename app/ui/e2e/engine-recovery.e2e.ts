import { expect, test } from "./fixture";

// Drive the existing shell event seam; no hardware or native audio stream is opened.
test("recovery failure and stopped events remain visible and dismissible", async ({ app, mock }) => {
  for (const [kind, message] of [
    ["recovery-failed", "Sound engine rebuild failed; project settings were preserved."],
    ["recovery-stopped", "Sound engine stopped after repeated errors. Reopen the project to retry."],
    ["recovery-reset", "Sound engine recovery budget reset."],
  ]) {
    await mock.eval(`(m) => m.emit("audio-event", ${JSON.stringify({ kind, message })})`);
    const toast = app.getByRole("status", { name: "Messages" });
    await expect(toast.getByText(message, { exact: true })).toBeVisible();
    await toast.getByRole("button").last().click();
    await expect(toast.getByText(message, { exact: true })).toHaveCount(0);
  }
  await app.getByRole("button", { name: "Stop", exact: true }).click();
  expect((await mock.calls("transport_stop")).length).toBeGreaterThan(0);
});
