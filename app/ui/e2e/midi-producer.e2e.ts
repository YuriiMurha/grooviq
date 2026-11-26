import { expect, test } from "./fixture";
import { twoChannels } from "./rollHelpers";

const replacement = "/qa/projects/MIDI.grooviq";
test.use({
  seed: {
    project: twoChannels([]),
    files: { [replacement]: { kind: "project", project: twoChannels([]) } },
    midi: { inputs: ["Keys", "Pads"] },
  },
});

for (const midiFirst of [true, false]) {
  test(`MIDI_and_typing_release_only_their_own_press_${midiFirst}`, async ({ app, mock }) => {
    await expect.poll(() => mock.calls("channel_select")).not.toHaveLength(0);
    await app.getByRole("button", { name: "Record", exact: true }).click();
    await app.getByRole("button", { name: "Play", exact: true }).click();
    await mock.eval("(m) => { m.hold(0); m.midiNote('Keys', 60, 127, true); }");
    // The upper typing row begins at C4 (the lower row starts at C3).
    await app.keyboard.down("q");
    await expect.poll(async () => (await mock.calls("channel_note")).filter((c) => c.args.on && c.args.key === 60).length).toBe(1);
    await mock.eval("(m) => m.hold(240)");
    if (midiFirst) await mock.eval("(m) => m.midiNote('Keys', 60, 0, false)");
    else await app.keyboard.up("q");
    expect(await mock.eval("(m) => m.live()")).toEqual(["1:60"]);
    await mock.eval("(m) => m.hold(480)");
    if (midiFirst) await app.keyboard.up("q");
    else await mock.eval("(m) => m.midiNote('Keys', 60, 0, false)");
    await expect.poll(() => mock.eval("(m) => m.live()")).toEqual([]);
    expect(await mock.eval("(m) => m.notes().map(n => [n.length, n.velocity]).sort((a,b) => a[1]-b[1])")).toEqual(midiFirst ? [[480, 0.8], [240, 1]] : [[240, 0.8], [480, 1]]);
  });
}

test("MIDI_survives_preview_release_and_record_off", async ({ app, mock }) => {
  await expect.poll(() => mock.calls("channel_select")).not.toHaveLength(0);
  await app.getByRole("button", { name: "Record", exact: true }).click();
  await app.getByRole("button", { name: "Play", exact: true }).click();
  await mock.eval("(m) => { m.hold(0); m.midiNote('Keys', 60, 127, true); }");
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
  await mock.eval("(m) => { m.hold(720); m.midiNote('Keys', 60, 0, false); }");
  expect(await mock.eval("(m) => m.live()")).toEqual([]);
  expect(await mock.eval("(m) => m.notes().map(n => n.length).sort((a,b) => a-b)")).toEqual([240, 480]);
});

test("same-key owners wait for the last port, including a sustained key", async ({ app, mock }) => {
  await app.getByRole("button", { name: "Record", exact: true }).click();
  await app.getByRole("button", { name: "Play", exact: true }).click();
  await mock.eval("(m) => { m.hold(0); m.midiMessage('Keys', [176, 64, 127]); m.midiNote('Keys', 60, 127, true); }");
  await mock.eval("(m) => { m.hold(120); m.midiNote('Pads', 60, 64, true); m.midiNote('Keys', 60, 0, false); }");
  await mock.eval("(m) => { m.hold(240); m.midiNote('Pads', 60, 0, false); }");
  expect(await mock.eval("(m) => m.live()")).toEqual(["1:60"]);
  expect(await mock.eval("(m) => m.notes()")).toEqual([]);

  await mock.eval("(m) => { m.hold(480); m.midiMessage('Keys', [176, 64, 0]); }");
  expect(await mock.eval("(m) => m.live()")).toEqual([]);
  expect(await mock.eval("(m) => m.notes().map(n => [n.tick, n.length])")).toEqual([[0, 480], [120, 360]]);
});

test("disabling and unplugging retire each port and reconnect with the pedal up", async ({ app, mock }) => {
  await app.getByRole("button", { name: /Silent \(QA mode\)/ }).click();
  const keys = app.getByRole("switch", { name: "Keys" });
  await mock.eval(`(m) => {
    m.midiMessage('Keys', [176, 64, 127]); m.midiNote('Keys', 60, 127, true); m.midiNote('Keys', 60, 0, false);
    m.midiMessage('Pads', [176, 64, 127]); m.midiNote('Pads', 62, 127, true); m.midiNote('Pads', 62, 0, false);
  }`);
  await keys.click();
  await expect(keys).not.toBeChecked();
  expect(await mock.eval("(m) => m.midiNote('Keys', 60, 127, true)")).toBeNull();
  expect(await mock.eval("(m) => m.live()")).toEqual(["1:62"]);

  await mock.eval("(m) => m.midiUnplug('Pads')");
  expect(await mock.eval("(m) => m.live()")).toEqual([]);
  expect(await mock.eval("(m) => m.midiNote('Pads', 62, 127, true)")).toBeNull();
  await keys.click();
  await expect(keys).toBeChecked();
  await mock.eval(`(m) => {
    m.midiPlug('Pads');
    for (const port of ['Keys', 'Pads']) { m.midiNote(port, 64, 127, true); m.midiNote(port, 64, 0, false); }
  }`);
  expect(await mock.eval("(m) => m.live()")).toEqual([]);
});

test("a stalled queue owes the old release without claiming refused note-ons", async ({ app, mock }) => {
  await app.getByRole("button", { name: "Record", exact: true }).click();
  await app.getByRole("button", { name: "Play", exact: true }).click();
  await mock.eval("(m) => { m.hold(0); m.midiNote('Keys', 60, 127, true); m.midiStall(true); }");
  await mock.eval("(m) => { m.hold(120); m.midiNote('Keys', 60, 0, false); m.midiNote('Pads', 61, 127, true); m.midiRetry(); }");
  expect(await mock.eval("(m) => m.live()")).toEqual(["1:60"]);
  expect(await mock.eval("(m) => m.notes()")).toEqual([]);

  await mock.eval("(m) => { m.hold(240); m.midiStall(false); m.midiNote('Keys', 60, 64, true); }");
  expect(await mock.eval("(m) => m.live()")).toEqual(["1:60"]);
  expect(await mock.eval("(m) => m.notes().map(n => [n.key, n.tick, n.length])")).toEqual([[60, 0, 240]]);
  await mock.eval("(m) => { m.hold(480); m.midiNote('Pads', 61, 0, false); m.midiNote('Keys', 60, 0, false); }");
  expect(await mock.eval("(m) => m.live()")).toEqual([]);
  expect(await mock.eval("(m) => m.notes().map(n => [n.key, n.tick, n.length])")).toEqual([[60, 0, 240], [60, 240, 240]]);
});

test("opening a project retires a held MIDI key even when the channel ID is reused", async ({ app, mock }) => {
  await mock.eval("(m) => m.midiNote('Keys', 60, 127, true)");
  expect(await mock.eval("(m) => m.live()")).toEqual(["1:60"]);
  await mock.eval(`(m) => m.queueDialog(${JSON.stringify(replacement)})`);
  await app.keyboard.press("ControlOrMeta+o");
  await expect.poll(async () => (await mock.view()).path).toBe(replacement);
  expect(await mock.eval("(m) => m.live()")).toEqual([]);

  await mock.eval("(m) => { m.midiNote('Pads', 60, 127, true); m.midiNote('Keys', 60, 0, false); }");
  expect(await mock.eval("(m) => m.live()")).toEqual(["1:60"]);
  await mock.eval("(m) => m.midiNote('Pads', 60, 0, false)");
  expect(await mock.eval("(m) => m.live()")).toEqual([]);
});
