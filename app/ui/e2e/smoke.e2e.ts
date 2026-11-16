import { expect, project, soundsFor, test } from "./fixture";

const KIT = ["Kick", "Snare", "Hat", "Clap"];

test.use({ seed: { project: project(KIT, { 1: [0, 4, 8, 12], 3: [2, 6, 10, 14] }), files: soundsFor(KIT) } });

test("the web build starts against the mock engine and draws the rack", async ({ app }) => {
  await expect(app.getByRole("region", { name: "Channel rack" })).toBeVisible();
  for (const name of KIT) await expect(app.getByRole("group", { name, exact: true })).toBeVisible();
  await expect(app.getByRole("button", { name: /Silent \(QA mode\)/ })).toBeVisible();
  await expect(app).toHaveTitle("QA beat — Grooviq");
});
