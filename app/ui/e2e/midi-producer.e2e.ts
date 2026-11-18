import { expect, test } from "./fixture";
import { twoChannels } from "./rollHelpers";

test.use({ seed: { project: twoChannels([]) } });

for (const midiFirst of [true, false]) {
  test(`MIDI_and_typing_release_only_their_own_press_${midiFirst}`, async ({ app, mock }) => {
    await expect.poll(() => mock.calls("channel_select")).not.toHaveLength(0);
    await app.getByRole("button", { name: "Record", exact: true }).click();
    await app.getByRole("button", { name: "Play", exact: true }).click();
    await mock.eval("(m) => { m.hold(0); m.midiNote(60, 127, true); }");
    // The upper typing row begins at C4 (the lower row starts at C3).
    await app.keyboard.down("q");
    await expect.poll(async () => (await mock.calls("channel_note")).filter((c) => c.args.on && c.args.key === 60).length).toBe(1);
    await mock.eval("(m) => m.hold(240)");
    if (midiFirst) await mock.eval("(m) => m.midiNote(60, 0, false)");
    else await app.keyboard.up("q");
    expect(await mock.eval("(m) => m.live()")).toEqual(["1:60"]);
    await mock.eval("(m) => m.hold(480)");
    if (midiFirst) await app.keyboard.up("q");
    else await mock.eval("(m) => m.midiNote(60, 0, false)");
    await expect.poll(() => mock.eval("(m) => m.live()")).toEqual([]);
    expect(await mock.eval("(m) => m.notes().map(n => [n.length, n.velocity]).sort((a,b) => a[1]-b[1])")).toEqual(midiFirst ? [[480, 0.8], [240, 1]] : [[240, 0.8], [480, 1]]);
  });
}

test("MIDI_survives_preview_release_and_record_off", async ({ app, mock }) => {
  await expect.poll(() => mock.calls("channel_select")).not.toHaveLength(0);
  await app.getByRole("button", { name: "Record", exact: true }).click();
  await app.getByRole("button", { name: "Play", exact: true }).click();
  await mock.eval("(m) => { m.hold(0); m.midiNote(60, 127, true); }");
  const name = app.getByRole("button", { name: "Bass Init", exact: true });
  const box = (await name.boundingBox())!;
  await app.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await app.mouse.down();
  await expect.poll(async () => (await mock.calls("channel_note")).filter(c => c.args.on).length).toBe(1);
  await mock.eval("(m) => m.hold(240)");
  await app.mouse.up();
  await expect.poll(async () => (await mock.calls("channel_note")).filter(c => !c.args.on).length).toBe(1);
  expect(await mock.eval("(m) => m.live()")).toEqual(["1:60"]);
  await mock.eval("(m) => m.hold(480)");
  await app.getByRole("button", { name: "Record", exact: true }).click();
  expect(await mock.eval("(m) => m.live()")).toEqual(["1:60"]);
  await mock.eval("(m) => { m.hold(720); m.midiNote(60, 0, false); }");
  expect(await mock.eval("(m) => m.live()")).toEqual([]);
  expect(await mock.eval("(m) => m.notes().map(n => n.length).sort((a,b) => a-b)")).toEqual([240, 480]);
});
