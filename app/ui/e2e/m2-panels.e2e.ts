// M2 Task 10: the panels (F6 / F7), synth channels and roll previews in the rack, the typing
// keyboard, record and metronome, removing a channel (#69) and the rack items of #73. Assertions
// read the mock session's command log and state.
import type { Page, TestInfo } from "@playwright/test";
import { brightness, cellPoint, clickCell, colourDistance, expect, grid, pixelsAt, presetValues, project, soundsFor, test, type Mock } from "./fixture";
import type { MockProject } from "./mockTauri";

const KIT = ["Kick", "Snare", "Hat"];
const kitSeed = { project: project(KIT, { 1: [0, 8], 2: [4] }), files: soundsFor(KIT) };

/** The kit plus a synth channel "Bass" (id 4) holding piano-roll notes: a long note, a slide
 * and a note off the step grid. */
function withBass(): MockProject {
  const p = project(KIT, { 1: [0, 8], 2: [4] });
  p.channels.push({
    id: 4,
    name: "Bass",
    color: 3,
    sample: null,
    volume: 1,
    pan: 0,
    mute: false,
    solo: false,
    sampler: {},
    kind: "synth",
    preset: "808 Glide",
  });
  p.nextId = 5;
  p.pattern.notes.push(
    { channel: 4, tick: 0, length: 960, key: 36, velocity: 1 },
    { channel: 4, tick: 1920, length: 480, key: 43, velocity: 0.9, slide: true },
    { channel: 4, tick: 2880, length: 120, key: 40, velocity: 0.7 },
  );
  return p;
}

/** Live notes sounding in the engine, as "channel:key". */
const live = (mock: Mock) => mock.eval<string[]>("(m) => m.live().sort()");
/** The `channel_note` calls, as [channel, key, on]. */
const noteCalls = async (mock: Mock) =>
  (await mock.calls("channel_note")).map((c) => [c.args.channel, c.args.key, c.args.on]);
const nameButton = (page: Page, name: string) => page.getByRole("button", { name: new RegExp(`^${name} `) });
const rows = (page: Page) =>
  page.getByRole("region", { name: "Channel rack" }).getByRole("group").evaluateAll((els) => els.map((e) => e.getAttribute("aria-label")));
const status = (page: Page) => page.getByTestId("step-grid-status");

/** An accessibility snapshot, then the screenshot. */
async function shot(page: Page, info: TestInfo, name: string) {
  await info.attach(`${name}-aria`, { body: await page.locator("body").ariaSnapshot(), contentType: "text/yaml" });
  await page.screenshot({ path: info.outputPath(`${name}.png`) });
}

/** Selects a channel the way the keyboard does: focusing its row. */
async function selectChannel(page: Page, mock: Mock, name: string, id: number) {
  await nameButton(page, name).focus();
  await expect.poll(async () => (await mock.calls("channel_select")).at(-1)?.args.channel).toBe(id);
}

test.describe("the typing keyboard", () => {
  test.use({ seed: kitSeed });

  test("typing_plays_the_selected_channel_and_release_stops_it", async ({ app, mock }, info) => {
    // The first channel is selected at start (and MIDI goes there).
    await expect.poll(async () => (await mock.calls("channel_select")).at(-1)?.args.channel).toBe(1);
    await selectChannel(app, mock, "Snare", 2);
    await app.keyboard.down("z");
    await expect.poll(() => live(mock)).toEqual(["2:48"]);
    expect((await mock.calls("channel_note"))[0].args).toEqual({ channel: 2, key: 48, velocity: 0.8, on: true });
    await shot(app, info, "typing-held");
    // = moves an octave up: Q is now 72, while Z still holds 48.
    await app.keyboard.press("Equal");
    await app.keyboard.down("q");
    await expect.poll(() => live(mock)).toEqual(["2:48", "2:72"]);
    await app.keyboard.up("z");
    await app.keyboard.up("q");
    await expect.poll(() => live(mock)).toEqual([]);
    expect(await noteCalls(mock)).toEqual([
      [2, 48, true],
      [2, 72, true],
      [2, 48, false],
      [2, 72, false],
    ]);
    // A key outside the rows, a held key's repeats and Cmd/Ctrl chords play nothing.
    await app.keyboard.press("a");
    await app.keyboard.down("x");
    await app.keyboard.down("x");
    await app.keyboard.down("x");
    await app.keyboard.up("x");
    await app.keyboard.press("ControlOrMeta+c");
    expect((await noteCalls(mock)).slice(4)).toEqual([
      [2, 62, true],
      [2, 62, false],
    ]);
    // Typing in a text field types.
    await app.getByLabel("Pattern length").focus();
    await app.keyboard.press("z");
    expect(await noteCalls(mock)).toHaveLength(6);
    // Space is still play/stop while the keyboard is on.
    await app.locator("body").click({ position: { x: 5, y: 300 } });
    await app.keyboard.press("Space");
    await expect.poll(() => mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(true);
    await app.keyboard.press("Space");
    await expect.poll(() => mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(false);
  });

  test("blur_and_toggle_release_every_held_key", async ({ app, mock }) => {
    await nameButton(app, "Kick").focus();
    await app.keyboard.down("z");
    await app.keyboard.down("x");
    await expect.poll(() => live(mock)).toEqual(["1:48", "1:50"]);
    // The window loses focus (Cmd+Tab): no keyup will ever come.
    await app.evaluate(() => window.dispatchEvent(new Event("blur")));
    await expect.poll(() => live(mock)).toEqual([]);
    await app.keyboard.up("z");
    await app.keyboard.up("x");
    await app.keyboard.down("c");
    await expect.poll(() => live(mock)).toEqual(["1:52"]);
    // The page is hidden.
    await app.evaluate(() => {
      Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true });
      document.dispatchEvent(new Event("visibilitychange"));
      Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
    });
    await expect.poll(() => live(mock)).toEqual([]);
    await app.keyboard.up("c");
    await app.keyboard.down("v");
    await expect.poll(() => live(mock)).toEqual(["1:53"]);
    // The toolbar toggle switches the keyboard off while the key is held.
    const toggle = app.getByRole("button", { name: "Typing keyboard" });
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-pressed", "false");
    await expect.poll(() => live(mock)).toEqual([]);
    await app.keyboard.up("v");
    const before = (await noteCalls(mock)).length;
    await app.keyboard.press("b");
    await app.waitForTimeout(100);
    expect(await noteCalls(mock)).toHaveLength(before);
    // Every note that started was stopped exactly once.
    const calls = await noteCalls(mock);
    expect(calls.filter((c) => c[2]).length).toBe(calls.filter((c) => !c[2]).length);
  });

  test("changing_channel_releases_on_the_old_one", async ({ app, mock }) => {
    await nameButton(app, "Kick").focus();
    await app.keyboard.down("z");
    await expect.poll(() => live(mock)).toEqual(["1:48"]);
    await selectChannel(app, mock, "Hat", 3);
    await expect.poll(() => live(mock)).toEqual([]);
    expect(await noteCalls(mock)).toEqual([
      [1, 48, true],
      [1, 48, false],
    ]);
    // Letting go of Z later sends nothing more; the next key plays the new channel.
    await app.keyboard.up("z");
    await app.keyboard.down("x");
    await expect.poll(() => live(mock)).toEqual(["3:50"]);
    await app.keyboard.up("x");
    await expect.poll(() => live(mock)).toEqual([]);
  });
});

test.describe("a typing note and a synth row's preview on one key", () => {
  test.use({ seed: { project: withBass(), files: soundsFor(KIT) } });

  /** The presses the mock engine holds, as ids. */
  const presses = (mock: Mock) => mock.eval<number[]>("(m) => [...m.state.presses.keys()]");
  const releases = async (mock: Mock) =>
    (await mock.calls("channel_note")).filter((c) => !c.args.on).map((c) => c.args.press);

  /** Holds Z on Bass as C4 (press 1), then holds Bass's name, which previews C4 (press 2). */
  async function both(app: Page, mock: Mock) {
    await selectChannel(app, mock, "Bass", 4);
    // Unfocus the name (Bass stays selected): WebKit does not focus a pressed button, so a
    // focused name would blur on the press and stop its preview (#185).
    await app.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
    await app.keyboard.press("Equal");
    await app.keyboard.down("z");
    await expect.poll(() => presses(mock)).toEqual([1]);
    const box = (await nameButton(app, "Bass").boundingBox())!;
    await app.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await app.mouse.down();
  }

  for (const previewFirst of [true, false]) {
    test(`each_release_ends_its_own_press_${previewFirst ? "preview" : "typing"}_first`, async ({ app, mock }) => {
      await both(app, mock);
      await expect.poll(() => presses(mock)).toEqual([1, 2]);
      expect(await live(mock)).toEqual(["4:60"]);
      if (previewFirst) await app.mouse.up();
      else await app.keyboard.up("z");
      // The release named its own press; the other note still sounds.
      await expect.poll(() => releases(mock)).toEqual([previewFirst ? 2 : 1]);
      expect(await presses(mock)).toEqual([previewFirst ? 1 : 2]);
      expect(await live(mock)).toEqual(["4:60"]);
      if (previewFirst) await app.keyboard.up("z");
      else await app.mouse.up();
      await expect.poll(() => live(mock)).toEqual([]);
      expect(await releases(mock)).toEqual(previewFirst ? [2, 1] : [1, 2]);
    });
  }

  test("a_refused_preview_releases_nothing_and_the_typing_note_ends_on_its_own", async ({ app, mock }) => {
    await selectChannel(app, mock, "Bass", 4);
    // Unfocus the name (Bass stays selected): WebKit does not focus a pressed button, so a
    // focused name would blur on the press and stop its preview (#185).
    await app.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
    await app.keyboard.press("Equal");
    await app.keyboard.down("z");
    await expect.poll(() => presses(mock)).toEqual([1]);
    // The engine refuses the preview's note-on (its queue is full): no press is made.
    await mock.eval(`(m) => m.refuseNext("channel_note", "The engine is busy.")`);
    const box = (await nameButton(app, "Bass").boundingBox())!;
    await app.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await app.mouse.down();
    await expect.poll(async () => (await mock.calls("channel_note")).length).toBe(2);
    await app.mouse.up();
    expect(await live(mock)).toEqual(["4:60"]);
    await app.keyboard.up("z");
    await expect.poll(() => live(mock)).toEqual([]);
    // One release, of the typing press: the refused preview had nothing to release.
    expect(await releases(mock)).toEqual([1]);
  });
});

test.describe("synth channels", () => {
  test.use({ seed: kitSeed });

  test("add_channel_menu_offers_a_sample_or_a_synth_preset", async ({ app, mock }, info) => {
    const add = app.getByRole("button", { name: "+ Add channel" });
    await add.click();
    const menu = app.getByRole("menu", { name: "Add channel" });
    await expect(menu.getByRole("menuitem")).toHaveText([
      "Sample…",
      "Init",
      "808 Glide",
      "Sub Bass",
      "Reese",
      "Bell Keys",
      "Dark Pad",
      "Pluck",
      "Trap Lead",
    ]);
    await expect(menu.getByRole("menuitem", { name: "Sample…" })).toBeFocused();
    await shot(app, info, "add-menu");
    // Escape closes it and gives focus back.
    await app.keyboard.press("Escape");
    await expect(menu).toBeHidden();
    await expect(add).toBeFocused();
    // The keyboard reaches a preset.
    await app.keyboard.press("Enter");
    await app.keyboard.press("ArrowDown");
    await app.keyboard.press("ArrowDown");
    await expect(menu.getByRole("menuitem", { name: "808 Glide" })).toBeFocused();
    await app.keyboard.press("Enter");
    await expect.poll(() => rows(app)).toEqual([...KIT, "808 Glide"]);
    expect((await mock.calls("channel_add_synth")).map((c) => c.args)).toEqual([{ preset: "808 Glide" }]);
    const synth = (await mock.view()).channels[3];
    expect(synth).toMatchObject({ id: 4, name: "808 Glide", kind: "synth", preset: "808 Glide", roll: false });
    // The new synth is selected: the typing keyboard plays it.
    await expect.poll(async () => (await mock.calls("channel_select")).at(-1)?.args.channel).toBe(4);
    await app.keyboard.press("z");
    await expect.poll(() => noteCalls(mock)).toEqual([
      [4, 48, true],
      [4, 48, false],
    ]);
    // A sample still comes from the file dialog.
    await mock.eval("(m) => m.queueDialog(['/qa/sounds/Hat.wav'])");
    await add.click();
    await menu.getByRole("menuitem", { name: "Sample…" }).click();
    await expect.poll(() => rows(app)).toEqual([...KIT, "808 Glide", "Hat"]);
    await shot(app, info, "synth-added");
  });
});

test.describe("roll channels in the rack", () => {
  test.use({ seed: { project: withBass(), files: soundsFor(KIT) } });

  test("roll_rows_show_a_preview_and_open_the_piano_roll", async ({ app, mock }, info) => {
    const g = grid(app);
    await expect.poll(async () => (await mock.calls("pattern_notes")).length).toBeGreaterThan(0);
    await expect(app.getByRole("region", { name: "Piano roll" })).toBeHidden();
    // The preview: the long note's bar spans steps 1-4 low in the row; the slide sits higher in
    // steps 9-10; no step squares (the gaps between steps are well, not background).
    const bassRow = 3;
    const at = (step: number, dy: number) => ({ x: cellPoint(bassRow, step).x, y: cellPoint(bassRow, step).y + dy });
    const gapBetween = (step: number) => ({ x: cellPoint(bassRow, step).x + 17, y: cellPoint(bassRow, step).y });
    await expect
      .poll(async () => {
        const [low, lowEnd, wellAbove, well, gap, kickGap] = await pixelsAt(app, g, [
          at(1, 8), // the long note (key 36, the lowest): bottom of the row
          at(3, 8),
          at(1, -8),
          at(6, 0), // no note here
          gapBetween(5), // between two steps of the roll row: still the well
          { x: cellPoint(0, 5).x + 17, y: cellPoint(0, 5).y }, // between two kick steps: the page
        ]);
        return {
          bar: colourDistance(low, wellAbove) > 100 && colourDistance(lowEnd, wellAbove) > 100,
          wellIsEven: colourDistance(well, gap) < 10,
          stepGapsDiffer: colourDistance(gap, kickGap) > 5,
          inkBright: brightness(low) > brightness(well) + 60,
        };
      })
      .toEqual({ bar: true, wellIsEven: true, stepGapsDiffer: true, inkBright: true });
    await info.attach("rack-aria", { body: await app.getByRole("region", { name: "Channel rack" }).ariaSnapshot(), contentType: "text/yaml" });
    await g.screenshot({ path: info.outputPath("roll-preview.png") });

    // A click on the row opens the piano roll on Bass and edits nothing.
    const before = await mock.eval<unknown[]>("(m) => m.notes()");
    await clickCell(app, bassRow, 6);
    const roll = app.getByRole("region", { name: "Piano roll" });
    await expect(roll).toBeVisible();
    await expect(roll).toContainText("Bass");
    await expect.poll(async () => (await mock.calls("channel_select")).at(-1)?.args.channel).toBe(4);
    // A right click, a drag and Enter, Delete and Alt+Up on the row edit nothing either.
    await clickCell(app, bassRow, 0, "right");
    const box = await g.boundingBox();
    if (!box) throw new Error("no grid");
    const p = cellPoint(bassRow, 0);
    await app.mouse.move(box.x + p.x, box.y + p.y);
    await app.mouse.down();
    await app.mouse.move(box.x + p.x, box.y + p.y - 40, { steps: 6 });
    await app.mouse.up();
    await app.keyboard.press("F7");
    await expect(roll).toBeHidden();
    await g.focus();
    for (const key of ["ArrowDown", "ArrowDown", "ArrowDown"]) await app.keyboard.press(key);
    await expect(status(app)).toHaveText("Bass, piano-roll notes");
    for (const key of ["Delete", "Backspace", "Alt+ArrowUp"]) await app.keyboard.press(key);
    await app.keyboard.press("Enter");
    await expect(roll).toBeVisible();
    expect(await mock.calls("project_edit")).toEqual([]);
    expect(await mock.eval<unknown[]>("(m) => m.notes()")).toEqual(before);
    await shot(app, info, "roll-open");
  });

  test("f6_and_f7_toggle_the_panels_and_the_splitter_resizes", async ({ app }, info) => {
    const rack = app.getByRole("region", { name: "Channel rack" });
    const roll = app.getByRole("region", { name: "Piano roll" });
    await nameButton(app, "Snare").focus();
    await app.keyboard.press("F7");
    await expect(roll).toBeVisible();
    // F7 opens the roll for the selected channel.
    await expect(roll).toContainText("Snare");
    const splitter = app.getByRole("separator");
    const s = (await splitter.boundingBox())!;
    await app.mouse.move(s.x + s.width / 2, s.y + 3);
    await app.mouse.down();
    await app.mouse.move(s.x + s.width / 2, s.y + 103, { steps: 5 });
    await app.mouse.up();
    // The rack's panel grows with the drag: the splitter follows the pointer.
    await expect.poll(async () => (await splitter.boundingBox())!.y).toBeCloseTo(s.y + 100, -1);
    await splitter.focus();
    const v = Number(await splitter.getAttribute("aria-valuenow"));
    await app.keyboard.press("ArrowUp");
    await expect(splitter).toHaveAttribute("aria-valuenow", String(v - 5));
    await shot(app, info, "both-panels");
    await app.keyboard.press("F6");
    await expect(rack).toBeHidden();
    await expect(roll).toBeVisible();
    await app.keyboard.press("F7");
    await expect(roll).toBeHidden();
    await app.keyboard.press("F6");
    await expect(rack).toBeVisible();
  });
});

test.describe("record and metronome", () => {
  test.use({ seed: kitSeed });

  test("record_and_metronome_toggle", async ({ app, mock }, info) => {
    const record = app.getByRole("button", { name: "Record" });
    const metronome = app.getByRole("button", { name: "Metronome" });
    await expect(metronome).toHaveAttribute("aria-pressed", "false");
    await record.click();
    await expect(record).toHaveAttribute("aria-pressed", "true");
    expect((await mock.view()).recording).toBe(true);
    await metronome.click();
    await expect(metronome).toHaveAttribute("aria-pressed", "true");
    expect(await mock.eval<boolean>("(m) => m.state.metronome")).toBe(true);
    await shot(app, info, "recording");

    // What is played while it loops lands in the pattern where it was heard, and the rack shows
    // the channel's notes as a preview (the note is not a plain step).
    const revision = (await mock.view()).notesRevision;
    await app.getByRole("button", { name: "Play" }).click();
    await mock.eval("(m) => m.hold(4 * 3840 + 480)"); // the second loop, step 3
    await nameButton(app, "Hat").focus();
    await app.keyboard.down("z");
    await expect.poll(() => live(mock)).toEqual(["3:48"]);
    await mock.eval("(m) => m.hold(4 * 3840 + 960)");
    await app.keyboard.up("z");
    await expect
      .poll(() => mock.eval("(m) => m.notes().filter((n) => n.channel === 3)"))
      .toEqual([{ id: expect.any(Number), channel: 3, tick: 480, length: 480, key: 48, velocity: 0.8, slide: false }]);
    await expect.poll(() => mock.view().then((v) => v.channels[2].roll)).toBe(true);
    await expect.poll(() => mock.view().then((v) => v.notesRevision)).toBeGreaterThan(revision);
    // The UI fetched the view and the notes again after `notes-changed`.
    await expect.poll(async () => (await mock.calls("pattern_notes")).length).toBeGreaterThan(1);
    await clickCell(app, 2, 10);
    await expect(app.getByRole("region", { name: "Piano roll" })).toContainText("Hat");
    expect(await mock.calls("project_edit")).toEqual([]);

    await record.click();
    await metronome.click();
    await expect(record).toHaveAttribute("aria-pressed", "false");
    await expect(metronome).toHaveAttribute("aria-pressed", "false");
    expect((await mock.calls("record_set")).map((c) => c.args.on)).toEqual([true, false]);
    expect((await mock.calls("metronome_set")).map((c) => c.args.on)).toEqual([true, false]);
  });

  test("r_toggles_record_only_while_the_typing_keyboard_is_off", async ({ app, mock }) => {
    await nameButton(app, "Kick").focus();
    // On: R is a note (F above middle C from base 48).
    await app.keyboard.press("r");
    await expect.poll(() => noteCalls(mock)).toEqual([
      [1, 65, true],
      [1, 65, false],
    ]);
    expect(await mock.calls("record_set")).toEqual([]);
    await app.getByRole("button", { name: "Typing keyboard" }).click();
    await app.keyboard.press("r");
    await expect.poll(() => mock.view().then((v) => v.recording)).toBe(true);
    await expect(app.getByRole("button", { name: "Record" })).toHaveAttribute("aria-pressed", "true");
    await app.keyboard.press("r");
    await expect.poll(() => mock.view().then((v) => v.recording)).toBe(false);
    // Not while typing in a field, and Cmd/Ctrl+R stays Export.
    await app.getByLabel("Pattern length").focus();
    await app.keyboard.press("r");
    await app.waitForTimeout(100);
    expect(await mock.calls("record_set")).toHaveLength(2);
    expect(await noteCalls(mock)).toHaveLength(2);
  });
});

test.describe("removing a channel (#69)", () => {
  test.use({ seed: kitSeed });

  test("delete_on_a_focused_row_or_the_context_menu_removes_the_channel_after_confirming", async ({ app, mock }, info) => {
    const snare = nameButton(app, "Snare");
    await snare.focus();
    await app.keyboard.press("Delete");
    const dialog = app.getByRole("dialog", { name: "Remove Snare?" });
    await expect(dialog).toBeVisible();
    await shot(app, info, "remove-confirm");
    await dialog.getByRole("button", { name: "Cancel" }).click();
    await expect(dialog).toBeHidden();
    expect(await mock.calls("project_edit")).toEqual([]);
    // Again, and Remove. A note held on it stops.
    await snare.focus();
    await app.keyboard.down("z");
    await expect.poll(() => live(mock)).toEqual(["2:48"]);
    await app.keyboard.press("Backspace");
    await app.getByRole("dialog", { name: "Remove Snare?" }).getByRole("button", { name: "Remove" }).click();
    await expect.poll(() => rows(app)).toEqual(["Kick", "Hat"]);
    await expect.poll(() => live(mock)).toEqual([]);
    await app.keyboard.up("z");
    expect((await mock.calls("project_edit")).map((c) => c.args.edit)).toEqual([{ kind: "removeChannel", channel: 2 }]);
    // Focus stays in the rack, on the row that took its place.
    await expect(nameButton(app, "Hat")).toBeFocused();

    // The context menu: Escape on the dialog keeps it, Remove removes it.
    await nameButton(app, "Kick").click({ button: "right" });
    const menu = app.getByRole("menu", { name: "Kick channel" });
    await expect(menu.getByRole("menuitem", { name: "Remove channel…" })).toBeFocused();
    await shot(app, info, "row-menu");
    await menu.getByRole("menuitem", { name: "Remove channel…" }).click();
    await app.keyboard.press("Escape");
    await expect(app.getByRole("dialog")).toBeHidden();
    await expect.poll(() => rows(app)).toEqual(["Kick", "Hat"]);
    await nameButton(app, "Kick").click({ button: "right" });
    await menu.getByRole("menuitem", { name: "Remove channel…" }).click();
    await app.getByRole("dialog", { name: "Remove Kick?" }).getByRole("button", { name: "Remove" }).click();
    await expect.poll(() => rows(app)).toEqual(["Hat"]);
    expect((await mock.view()).pattern.cells).toEqual({ "3": Array(16).fill(null) });
    // No error was shown on the way.
    await expect(app.getByRole("status", { name: "Messages" })).toHaveText("");
  });

  test("files_dropped_while_a_dialog_is_open_are_ignored", async ({ app, mock }) => {
    await nameButton(app, "Hat").focus();
    await app.keyboard.press("Delete");
    await expect(app.getByRole("dialog", { name: "Remove Hat?" })).toBeVisible();
    await mock.eval("(m) => m.drag(['/qa/sounds/Kick.wav'], true)");
    await app.waitForTimeout(200);
    expect(await mock.calls("channel_add")).toEqual([]);
    await expect(app.getByRole("region", { name: "Channel rack" })).not.toHaveClass(/drop-over/);
    await app.keyboard.press("Escape");
    await mock.eval("(m) => m.drag(['/qa/sounds/Kick.wav'], true)");
    await expect.poll(() => rows(app)).toEqual([...KIT, "Kick"]);
  });
});

test.describe("the step grid's keys and canvas (#73)", () => {
  test.use({ seed: kitSeed });

  test("holding_enter_toggles_once", async ({ app, mock }) => {
    await grid(app).focus();
    await app.keyboard.press("ArrowRight");
    for (let i = 0; i < 6; i++) await app.keyboard.down("Enter");
    await app.keyboard.up("Enter");
    await expect(status(app)).toHaveText("Kick, step 2, on, velocity 80%");
    expect((await mock.calls("project_edit")).map((c) => c.args.edit)).toEqual([{ kind: "toggleStep", channel: 1, step: 1 }]);
  });

  test("the_canvas_swap_keeps_keyboard_focus", async ({ app }) => {
    const g = grid(app);
    await g.focus();
    await app.keyboard.press("ArrowRight");
    await expect(status(app)).toHaveText("Kick, step 2, off");
    // The WebGL2 context is lost and never comes back: the grid moves to a 2D canvas.
    const lost = await g.evaluate((c: HTMLCanvasElement) => {
      const ext = c.getContext("webgl2")?.getExtension("WEBGL_lose_context");
      ext?.loseContext();
      return Boolean(ext);
    });
    test.skip(!lost, "no WEBGL_lose_context here");
    await expect.poll(() => g.evaluate((c: HTMLCanvasElement) => c.getContext("2d") !== null), { timeout: 5000 }).toBe(true);
    await expect(g).toBeFocused();
    await app.keyboard.press("ArrowRight");
    await expect(status(app)).toHaveText("Kick, step 3, off");
  });

  test("a_dpr_change_redraws_sharp", async ({ app, browserName }) => {
    test.skip(browserName !== "chromium", "changes the ratio through the Chrome DevTools Protocol");
    const backing = () => grid(app).evaluate((c: HTMLCanvasElement) => c.width / c.clientWidth);
    expect(await backing()).toBeCloseTo(1);
    // The window moves to a 2x screen: the ratio changes, its size does not (#117).
    const cdp = await app.context().newCDPSession(app);
    await cdp.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 800, deviceScaleFactor: 2, mobile: false });
    await expect.poll(backing).toBeCloseTo(2);
  });
});

test.describe("long patterns (#73)", () => {
  const MANY = Array.from({ length: 24 }, (_, i) => `Sound ${i + 1}`);
  const long = project(MANY, { 1: [0] });
  long.pattern.steps = 64;
  test.use({ seed: { project: long, files: soundsFor(MANY) }, viewport: { width: 900, height: 560 } });

  test("focus_scrolls_into_view_on_long_patterns", async ({ app }, info) => {
    const g = grid(app);
    /** Whether the cell is on screen: the canvas is what is drawn at its centre. */
    const visible = async (row: number, step: number) => {
      const box = await g.boundingBox();
      if (!box) throw new Error("no grid");
      const p = cellPoint(row, step);
      return app.evaluate(({ x, y }) => document.elementFromPoint(x, y)?.getAttribute("aria-label") === "Step grid", {
        x: box.x + p.x,
        y: box.y + p.y,
      });
    };
    await g.focus();
    expect(await visible(0, 63)).toBe(false);
    await app.keyboard.press("End");
    await expect(status(app)).toHaveText("Sound 1, step 64, off");
    await expect.poll(() => visible(0, 63)).toBe(true);
    for (let i = 0; i < 23; i++) await app.keyboard.press("ArrowDown");
    await expect(status(app)).toHaveText("Sound 24, step 64, off");
    await expect.poll(() => visible(23, 63)).toBe(true);
    await shot(app, info, "focus-far-corner");
    await app.keyboard.press("Home");
    await expect.poll(() => visible(23, 0)).toBe(true);
  });
});

test.describe("at a narrow window (900 px)", () => {
  test.use({ seed: { project: withBass(), files: soundsFor(KIT) }, viewport: { width: 900, height: 560 } });

  test("the flows fit and work", async ({ app, mock }, info) => {
    const overflow = () =>
      app.evaluate(() => ({
        x: document.documentElement.scrollWidth - window.innerWidth,
        y: document.documentElement.scrollHeight - window.innerHeight,
      }));
    expect(await overflow()).toEqual({ x: 0, y: 0 });
    // The new toolbar buttons are on screen and overlap nothing.
    const names = ["Play", "Stop", "Record", "Metronome", "Typing keyboard", "Test tone"];
    const boxes = [];
    for (const name of names) {
      const b = await app.getByRole("button", { name, exact: true }).boundingBox();
      if (!b) throw new Error(`no ${name}`);
      expect(b.x + b.width, `${name} is on screen`).toBeLessThanOrEqual(900);
      // One line each: the toolbar does not wrap its labels to make room.
      expect(b.height, `${name} stays one line`).toBeLessThanOrEqual(36);
      boxes.push({ name, ...b });
    }
    for (const a of boxes) {
      for (const b of boxes) {
        if (a === b) continue;
        const overlap = a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
        expect(overlap, `${a.name} overlaps ${b.name}`).toBe(false);
      }
    }
    for (const label of ["Position", "BPM"]) {
      const el = app.getByLabel(label, { exact: true });
      const h = await el.evaluate((e) => e.closest(".tempo, .position")?.getBoundingClientRect().height ?? 0);
      expect(h, `${label} block keeps its two lines`).toBeLessThanOrEqual(46);
    }
        // Typing, a synth from the menu, the roll from a preview row, removing a channel.
    await nameButton(app, "Kick").focus();
    await app.keyboard.press("z");
    await expect.poll(() => noteCalls(mock)).toEqual([
      [1, 48, true],
      [1, 48, false],
    ]);
    await app.getByRole("button", { name: "+ Add channel" }).click();
    const preset = app.getByRole("menuitem", { name: "Trap Lead" });
    await expect(preset).toBeInViewport();
    await shot(app, info, "narrow-add-menu");
    await preset.click();
    await expect.poll(() => rows(app)).toEqual([...KIT, "Bass", "Trap Lead"]);
    await clickCell(app, 3, 2);
    await expect(app.getByRole("region", { name: "Piano roll" })).toBeVisible();
    expect(await overflow()).toEqual({ x: 0, y: 0 });
    await shot(app, info, "narrow-both-panels");
    await nameButton(app, "Trap Lead").focus();
    await app.keyboard.press("Delete");
    await app.getByRole("dialog", { name: "Remove Trap Lead?" }).getByRole("button", { name: "Remove" }).click();
    await expect.poll(() => rows(app)).toEqual([...KIT, "Bass"]);
  });
});

test.describe("stuck notes at the edges (fix round 1)", () => {
  test.use({ seed: kitSeed });

  test("stop_and_record_off_release_held_keys", async ({ app, mock }) => {
    await app.getByRole("button", { name: "Play" }).click();
    await nameButton(app, "Kick").focus();
    await app.keyboard.down("z");
    await expect.poll(() => live(mock)).toEqual(["1:48"]);
    await app.getByRole("button", { name: "Stop" }).click();
    await expect.poll(() => live(mock)).toEqual([]);
    // Record off while a key is held.
    const record = app.getByRole("button", { name: "Record" });
    await record.click();
    await expect(record).toHaveAttribute("aria-pressed", "true");
    await nameButton(app, "Kick").focus();
    await app.keyboard.down("x");
    // Z is still physically down but was released at Stop: only X sounds.
    await expect.poll(() => live(mock)).toEqual(["1:50"]);
    await record.click();
    await expect(record).toHaveAttribute("aria-pressed", "false");
    await expect.poll(() => live(mock)).toEqual([]);
    // A stop the UI did not ask for (the speaker guard, another window): seen in the live frames.
    await app.getByRole("button", { name: "Play" }).click();
    await nameButton(app, "Kick").focus();
    await app.keyboard.down("c");
    await expect.poll(() => live(mock)).toEqual(["1:52"]);
    // The live frames have shown it playing (the position moved), then the engine stops alone.
    await mock.eval("(m) => m.hold(960)");
    await expect(app.getByRole("timer", { name: "Position" })).toHaveText("1.2.01");
    await mock.eval("(m) => { m.state.transport.playing = false; }");
    await expect.poll(() => live(mock)).toEqual([]);
    for (const k of ["z", "x", "c"]) await app.keyboard.up(k);
    // Each note-off went to the channel and key its note-on went to, once.
    const calls = await noteCalls(mock);
    expect(calls.filter((c) => c[2]).map((c) => c.slice(0, 2))).toEqual([
      [1, 48],
      [1, 50],
      [1, 52],
    ]);
    expect(calls.filter((c) => !c[2]).map((c) => c.slice(0, 2))).toEqual([
      [1, 48],
      [1, 50],
      [1, 52],
    ]);
  });

  test("hiding_the_rack_or_leaving_the_window_stops_a_preview", async ({ app, mock }) => {
    await app.getByRole("button", { name: "+ Add channel" }).click();
    await app.getByRole("menuitem", { name: "Init" }).click();
    await expect.poll(() => rows(app)).toEqual([...KIT, "Init"]);
    await nameButton(app, "Init").focus();
    await app.keyboard.down("Enter");
    await expect.poll(() => live(mock)).toEqual(["4:60"]);
    await app.keyboard.press("F6");
    await expect(app.getByRole("region", { name: "Channel rack" })).toBeHidden();
    await expect.poll(() => live(mock)).toEqual([]);
    await app.keyboard.up("Enter");
    await app.keyboard.press("F6");
    // A sample preview held while the window loses focus.
    const previews = () => mock.eval<[number, boolean][]>("(m) => [...m.state.previews.entries()]");
    await nameButton(app, "Kick").focus();
    await app.keyboard.down("Enter");
    await expect.poll(previews).toEqual([[1, true]]);
    await app.evaluate(() => window.dispatchEvent(new Event("blur")));
    await expect.poll(previews).toEqual([[1, false]]);
    await app.keyboard.up("Enter");
    expect(await live(mock)).toEqual([]);
  });
});

/** Calls a shell command straight from the page, as the UI's `invoke` would. */
const call = (page: Page, cmd: string, args: Record<string, unknown> = {}) =>
  page.evaluate(
    ([cmd, args]) => (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__.invoke(cmd, args),
    [cmd, args] as const,
  );

test.describe("the mock follows Task 7 (fix round 1)", () => {
  test.use({ seed: kitSeed });

  test("every_note_change_emits_notes_changed", async ({ app, mock }) => {
    // A listener of its own, registered through the event plugin like `listen` does.
    await app.evaluate(() => {
      const w = window as unknown as {
        seen: unknown[];
        __TAURI_INTERNALS__: { transformCallback: (cb: (e: { payload: unknown }) => void) => number; invoke: (c: string, a: unknown) => Promise<unknown> };
      };
      w.seen = [];
      const handler = w.__TAURI_INTERNALS__.transformCallback((e) => w.seen.push(e.payload));
      return w.__TAURI_INTERNALS__.invoke("plugin:event|listen", { event: "notes-changed", target: { kind: "Any" }, handler });
    });
    const seen = () => app.evaluate(() => (window as unknown as { seen: number[] }).seen);
    const revision = () => mock.view().then((v) => v.notesRevision);
    const expected: number[] = [];
    // A step edit.
    const generation = (await mock.view()).generation;
    await call(app, "project_edit", { edit: { kind: "toggleStep", channel: 1, step: 3 }, generation });
    expected.push(await revision());
    // A notes edit.
    await call(app, "notes_edit", {
      edit: { generation, baseRevision: await revision(), add: [{ channel: 2, tick: 120, length: 600, key: 64, velocity: 0.5, slide: false }], update: [], delete: [] },
    });
    expected.push(await revision());
    // A mix change is not a note change.
    await call(app, "project_edit", { edit: { kind: "setChannelMix", channel: 1, volume: 0.5, pan: 0, mute: false, solo: false }, generation });
    // Removing a channel with notes.
    await call(app, "project_edit", { edit: { kind: "removeChannel", channel: 2 }, generation });
    expected.push(await revision());
    // Replacing the project.
    await call(app, "project_new");
    expected.push(await revision());
    expect(new Set(expected).size).toBe(4);
    await expect.poll(seen).toEqual(expected);
  });

  test("presets_load_all_their_values", async ({ mock, app }) => {
    const synthOf = async (id: number) => (await mock.eval<{ channels: { id: number; synth: Record<string, number>; preset: string | null }[] }>("(m) => m.view()")).channels.find((c) => c.id === id)!;
    await call(app, "channel_add_synth", { preset: "808 Glide" });
    expect((await synthOf(4)).synth).toEqual(presetValues("808 Glide"));
    expect((await synthOf(4)).synth.filter_cutoff).toBe(650);
    await call(app, "project_edit", { edit: { kind: "setInstrumentParam", channel: 4, param: "filter_cutoff", value: 123 }, generation: (await mock.view()).generation });
    expect((await synthOf(4)).synth.filter_cutoff).toBe(123);
    await call(app, "project_edit", { edit: { kind: "loadSynthPreset", channel: 4, preset: "808 Glide" }, generation: (await mock.view()).generation });
    expect((await synthOf(4)).synth).toEqual(presetValues("808 Glide"));
    await call(app, "project_edit", { edit: { kind: "loadSynthPreset", channel: 4, preset: "Reese" }, generation: (await mock.view()).generation });
    expect(await synthOf(4)).toMatchObject({ synth: presetValues("Reese"), preset: "Reese" });
    await call(app, "channel_add_synth", { preset: "Init" });
    expect((await synthOf(5)).synth).toEqual(presetValues("Init"));
  });

  test("stop_closes_recorded_notes_at_the_stop_tick", async ({ app, mock }) => {
    await call(app, "record_set", { on: true });
    await call(app, "transport_play");
    await mock.eval("(m) => m.hold(480)");
    const press = await call(app, "channel_note", { channel: 1, key: 48, velocity: 0.8, on: true });
    await mock.eval("(m) => m.hold(960)");
    await call(app, "transport_stop");
    const recorded = () => mock.eval("(m) => m.notes().filter((n) => n.key === 48)");
    const one = [{ id: expect.any(Number), channel: 1, tick: 480, length: 480, key: 48, velocity: 0.8, slide: false }];
    expect(await recorded()).toEqual(one);
    expect(await mock.eval<number>("(m) => m.state.recordOpen.size")).toBe(0);
    // The key comes up after the stop: nothing changes, nothing doubles.
    await call(app, "channel_note", { channel: 1, key: 48, velocity: 0, on: false, press });
    expect(await recorded()).toEqual(one);
  });
});

test.describe("stuck notes at the edges (fix round 2)", () => {
  test.use({ seed: kitSeed });

  test("stop_while_idle_releases_held_keys", async ({ app, mock }) => {
    // Nothing plays; Stop is pressed anyway (the UI never saw a playing edge).
    await nameButton(app, "Kick").focus();
    await app.keyboard.down("z");
    await expect.poll(() => live(mock)).toEqual(["1:48"]);
    await app.getByRole("button", { name: "Stop" }).click();
    await expect.poll(() => live(mock)).toEqual([]);
    await app.keyboard.up("z");
    expect(await noteCalls(mock)).toEqual([
      [1, 48, true],
      [1, 48, false],
    ]);
  });

  test("an_external_stop_between_live_frames_releases_held_keys", async ({ app, mock }) => {
    const play = app.getByRole("button", { name: "Play" });
    // The stream stalls after an idle frame; Play is pressed and a key held; the engine stops on
    // its own; the stream resumes with a stopped frame, the same as the last one it sent.
    await app.waitForTimeout(100);
    await mock.eval("(m) => m.pauseLive(true)");
    await play.click();
    await expect(play).toHaveAttribute("aria-pressed", "true");
    await nameButton(app, "Kick").focus();
    await app.keyboard.down("z");
    await expect.poll(() => live(mock)).toEqual(["1:48"]);
    await mock.eval("(m) => { m.state.transport.playing = false; m.pauseLive(false); }");
    await expect(play).toHaveAttribute("aria-pressed", "false");
    await expect.poll(() => live(mock)).toEqual([]);
    await app.keyboard.up("z");
    // Space now plays again (the UI knows it is stopped).
    await app.keyboard.press("Space");
    await expect.poll(() => mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(true);
  });
});

test.describe("stuck notes when the project is replaced (fix round 2)", () => {
  // The other project has the same channel ids, so the selection does not change with it.
  test.use({ seed: { ...kitSeed, files: { ...kitSeed.files, "/qa/projects/Other.grooviq": { kind: "project", project: project(KIT, { 2: [4] }) } } } });

  test("opening_a_project_releases_held_keys", async ({ app, mock }) => {
    await nameButton(app, "Kick").focus();
    await app.keyboard.down("z");
    await expect.poll(() => live(mock)).toEqual(["1:48"]);
    await mock.eval("(m) => m.queueDialog('/qa/projects/Other.grooviq')");
    await app.getByRole("button", { name: "QA beat" }).click();
    await app.getByRole("menuitem", { name: "Open…" }).click();
    await expect.poll(() => mock.view().then((v) => v.path)).toBe("/qa/projects/Other.grooviq");
    await expect.poll(() => live(mock)).toEqual([]);
    await app.keyboard.up("z");
    expect(await noteCalls(mock)).toEqual([
      [1, 48, true],
      [1, 48, false],
    ]);
  });
});

test.describe("stale and genuine stops after Play (fix round 3)", () => {
  test.use({ seed: kitSeed });

  test("a_stale_stopped_frame_after_play_is_ignored", async ({ app, mock }) => {
    const play = app.getByRole("button", { name: "Play" });
    await app.waitForTimeout(100);
    // The stream holds a stopped frame rendered before Play took effect and delivers it late.
    await mock.eval("(m) => m.pauseLive(true)");
    await play.click();
    await expect(play).toHaveAttribute("aria-pressed", "true");
    await nameButton(app, "Kick").focus();
    await app.keyboard.down("z");
    await expect.poll(() => live(mock)).toEqual(["1:48"]);
    await mock.eval("(m) => m.injectLive(false)");
    await mock.eval("(m) => m.pauseLive(false)");
    // The engine plays: the UI says so, and Z keeps sounding.
    await expect.poll(() => mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(true);
    await app.waitForTimeout(400);
    await expect(play).toHaveAttribute("aria-pressed", "true");
    expect(await live(mock)).toEqual(["1:48"]);
    await app.keyboard.up("z");
    await expect.poll(() => live(mock)).toEqual([]);
  });

  test("a_genuine_stop_before_any_playing_frame_is_adopted", async ({ app, mock }) => {
    const play = app.getByRole("button", { name: "Play" });
    await nameButton(app, "Kick").focus();
    await app.keyboard.down("z");
    await expect.poll(() => live(mock)).toEqual(["1:48"]);
    // Play is accepted, but the engine stops at once (the speaker guard): no frame ever says
    // playing. The UI follows the stopped frames after its wait and lets go of Z.
    await mock.eval("(m) => m.pauseLive(true)");
    await play.click();
    await mock.eval("(m) => { m.state.transport.playing = false; m.pauseLive(false); }");
    await expect(play).toHaveAttribute("aria-pressed", "false");
    await expect.poll(() => live(mock)).toEqual([]);
    await app.keyboard.up("z");
    // Space plays again (the UI knows it is stopped).
    await app.keyboard.press("Space");
    await expect.poll(() => mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(true);
  });
});
