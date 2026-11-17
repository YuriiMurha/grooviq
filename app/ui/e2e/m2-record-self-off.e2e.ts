// Recording that switches itself off (the pattern is full) in the middle of a roll gesture
// (#187): the person is told, Record shows off, held keys are let go and the gesture still
// commits, once.
import { expect, test } from "./fixture";
import { at, openRoll, sessionNotes, twoChannels } from "./rollHelpers";

test.use({ seed: { project: twoChannels([]) } });

test("recording_stopping_itself_mid_draw_warns_releases_held_keys_and_commits_the_gesture_once", async ({ app, mock }) => {
  await openRoll(app);
  const record = app.getByRole("button", { name: "Record", exact: true });
  const warnings = app.getByRole("status", { name: "Messages" }).locator(".toast.warn");
  const live = () => mock.eval<string[]>("(m) => m.live().sort()");
  await record.click();
  await expect(record).toHaveAttribute("aria-pressed", "true");
  await app.getByRole("button", { name: "Play" }).click();
  const before = await warnings.count();
  // A typed key held while a note is drawn.
  await app.keyboard.down("z");
  await expect.poll(live).toContain("1:48");
  const p = await at(app, 960, 64);
  await app.mouse.move(p.x, p.y);
  await app.mouse.down();
  await mock.eval("(m) => m.recordingStopsItself(1)");
  await expect(warnings).toHaveCount(before + 1);
  await expect(record).toHaveAttribute("aria-pressed", "false");
  // The held key is let go, by its own press.
  await expect.poll(live).not.toContain("1:48");
  const releases = (await mock.calls("channel_note")).filter((c) => !c.args.on && c.args.key === 48);
  expect(releases).toHaveLength(1);
  await app.mouse.up();
  await app.keyboard.up("z");
  // The drawn note is one edit; the refused typed note is not in the pattern.
  await expect.poll(() => sessionNotes(mock)).toEqual([[1, 960, 240, 64]]);
  expect(await mock.calls("notes_edit")).toHaveLength(1);
  await expect.poll(live).toEqual([]);
});
