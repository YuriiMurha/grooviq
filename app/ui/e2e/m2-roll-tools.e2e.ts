// M2 Task 12: the piano roll's musical side: key and scale, slide notes, the velocity lane, ghost
// notes, quantize, copy, paste and duplicate, chord stamps. Assertions read the mock session's
// command log and notes, the roll's counters and the pixels on screen.
import type { Page } from "@playwright/test";
import { brightness, colourDistance, expect, pixelsAt, test } from "./fixture";
import { choosePickerOption, confirmPickerWithArrows, pickerButton } from "./listPickerHelpers";
import type { MockNote } from "./mockTauri";
import { at, boxSelect, canvas, drag, lane, local, nextFrame, openRoll, sessionNotes, shot, stats, token, tool, twoChannels } from "./rollHelpers";

const note = (id: number, tick: number, key: number, o: Partial<MockNote> = {}): MockNote => ({ id, channel: 1, tick, length: 240, key, velocity: 0.8, ...o });
const C_MAJOR = 0b101010110101;
const D_MAJOR = 0b101011010110;

/** D major: C and F are white rows outside the key, D an inside one. */
const KEYED = { key: { root: 2, scale: "major" } };

test.describe("the grid picker", () => {
  test.use({ seed: { project: twoChannels([]) } });

  test("the grid changes note snapping only after confirmation", async ({ app, mock }) => {
    await openRoll(app);
    await confirmPickerWithArrows(app, "Snap", "Step", "1/2 beat", 2, async () => {
      expect(await mock.calls("notes_edit")).toHaveLength(0);
    });
    const point = await at(app, 800, 60);
    await app.mouse.click(point.x, point.y);
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 480, 240, 60]]);
    expect(await mock.calls("notes_edit")).toHaveLength(1);
  });
});

test.describe("key and scale", () => {
  test.use({ seed: { project: twoChannels([], KEYED) } });

  test("rows_outside_the_scale_are_dimmed", async ({ app }, info) => {
    await openRoll(app);
    expect((await stats(app)).scaleMask).toBe(D_MAJOR);
    await nextFrame(app);
    // Two white rows, empty at tick 3000: C (60) is not in D major, D (62) is.
    const [outside, inside] = await pixelsAt(app, canvas(app), [await local(app, 3000, 60), await local(app, 3000, 62)]);
    await shot(app, info, "d-major");
    expect(brightness(outside)).toBeLessThan(brightness(inside) * 0.8);
  });

  test("drawing_with_scale_snap_lands_in_key", async ({ app, mock }) => {
    await openRoll(app);
    // C (60) is outside D major, between B (59) and C# (61): a tie, so it goes down. F (65) is a
    // tie between E (64) and F# (66): down as well.
    const a = await at(app, 10, 60);
    await app.mouse.click(a.x, a.y);
    const b = await at(app, 500, 65);
    await app.mouse.click(b.x, b.y);
    await expect.poll(() => sessionNotes(mock)).toEqual([
      [1, 0, 240, 59],
      [1, 480, 240, 64],
    ]);
    // Painting snaps its row too.
    await tool(app, "Paint").click();
    await drag(app, await at(app, 970, 60), await at(app, 1250, 60));
    await expect.poll(() => sessionNotes(mock).then((n) => n.filter((x) => x[1] >= 960))).toEqual([
      [1, 960, 240, 59],
      [1, 1200, 240, 59],
    ]);
    // With the snap off, notes land where they are drawn.
    await tool(app, "Draw").click();
    await app.getByRole("button", { name: "Snap to scale", exact: true }).click();
    const c = await at(app, 2410, 60);
    await app.mouse.click(c.x, c.y);
    await expect.poll(() => sessionNotes(mock).then((n) => n.filter((x) => x[1] >= 2400))).toEqual([[1, 2400, 240, 60]]);
  });
});

test.describe("slide notes", () => {
  test.use({ seed: { project: twoChannels([note(1, 480, 64, { length: 960, velocity: 1 }), note(2, 480, 67, { length: 960, velocity: 1 })]) } });

  test("the_slide_toggle_marks_notes_and_draws_them_differently", async ({ app, mock }, info) => {
    await openRoll(app);
    await nextFrame(app);
    const ink = await token(app, "--ink-4");
    const slideInk = await token(app, "--ink-4b");
    /** The middle of each note and its bottom right corner, where a slide note's edge has leaned away. */
    const looks = async (key: number) => {
      const mid = await local(app, 960, key);
      const v = (await stats(app)).view;
      const corner = { x: mid.x + 480 * v.tickPx - 3, y: mid.y + v.keyPx / 2 - 3 };
      const [m, c] = await pixelsAt(app, canvas(app), [mid, corner]);
      return { mid: m, corner: c };
    };
    const before = await looks(64);
    expect(colourDistance(before.mid, ink)).toBeLessThan(40);
    expect(colourDistance(before.corner, ink)).toBeLessThan(40);

    // Select the lower note only and switch Slide on: it is a slide note in the session.
    await tool(app, "Select").click();
    const p = await at(app, 700, 64);
    await app.mouse.click(p.x, p.y);
    await tool(app, "Slide").click();
    await expect.poll(() => mock.eval<boolean[]>("(m) => m.notes().filter((n) => n.key === 64).map((n) => !!n.slide)")).toEqual([true]);
    expect(await mock.eval<boolean[]>("(m) => m.notes().filter((n) => n.key === 67).map((n) => !!n.slide)")).toEqual([false]);
    // Selected notes carry a ring: let go of the selection to see the notes as they are.
    await app.mouse.click((await at(app, 3000, 60)).x, (await at(app, 3000, 60)).y);
    await nextFrame(app);
    const slid = await looks(64);
    const plain = await looks(67);
    await shot(app, info, "slide");
    // It is in the slide ink, and its right edge leans: the corner shows the row, not a note.
    expect(colourDistance(slid.mid, slideInk)).toBeLessThan(40);
    expect(colourDistance(slid.corner, slideInk)).toBeGreaterThan(60);
    expect(colourDistance(slid.corner, ink)).toBeGreaterThan(60);
    // The other note is untouched.
    expect(colourDistance(plain.mid, ink)).toBeLessThan(40);
    expect(colourDistance(plain.corner, ink)).toBeLessThan(40);

    // Slide stays on: notes drawn now are slide notes too.
    await tool(app, "Draw").click();
    const d = await at(app, 2410, 60);
    await app.mouse.click(d.x, d.y);
    await expect.poll(() => mock.eval<boolean[]>("(m) => m.notes().filter((n) => n.key === 60).map((n) => !!n.slide)")).toEqual([true]);
  });
});

test.describe("velocity lane", () => {
  const four = [note(1, 0, 60), note(2, 480, 62), note(3, 960, 64), note(4, 1440, 65), note(5, 1440, 67, { channel: 2 })];
  test.use({ seed: { project: twoChannels(four) } });

  test("the_velocity_lane_drags_selected_notes_and_commits_once", async ({ app, mock }, info) => {
    await openRoll(app);
    // Select the first three notes of Bass.
    await boxSelect(app, 10, 66, 1100, 59);
    const box = (await lane(app).boundingBox())!;
    const y = (velocity: number) => box.y + (1 - velocity) * box.height;
    const x = async (tick: number) => (await at(app, tick, 60)).x;
    expect(await mock.calls("notes_edit")).toEqual([]);

    // Drag along the lane to 0.3: nothing is sent until the pointer lets go, then one edit.
    await app.mouse.move(await x(2000), y(0.8));
    await app.mouse.down();
    await app.mouse.move(await x(2200), y(0.5), { steps: 4 });
    await app.mouse.move(await x(2400), y(0.3), { steps: 4 });
    await nextFrame(app);
    expect(await mock.calls("notes_edit")).toEqual([]);
    await shot(app, info, "lane-drag");
    await app.mouse.up();
    await expect.poll(async () => (await mock.calls("notes_edit")).length).toBe(1);
    const [edit] = await mock.calls("notes_edit");
    const sent = edit.args.edit as { update: { id: number; velocity: number }[]; add: unknown[]; delete: unknown[] };
    expect(sent.update.map((n) => n.id).sort()).toEqual([1, 2, 3]);
    for (const n of sent.update) expect(n.velocity).toBeCloseTo(0.3, 1);
    expect(sent.add).toEqual([]);
    expect(sent.delete).toEqual([]);
    // The notes outside the selection kept their velocity.
    const after = await mock.eval<Required<MockNote>[]>("(m) => m.notes()");
    expect(after.filter((n) => n.velocity > 0.7).map((n) => n.id).sort()).toEqual([4, 5]);

    // The bars show it: ink up to the new height only.
    await nextFrame(app);
    const bar = (await at(app, 480, 60)).x - (await canvas(app).boundingBox())!.x;
    const [high, low] = await pixelsAt(app, lane(app), [
      { x: bar + 1, y: box.height * 0.45 },
      { x: bar + 1, y: box.height * 0.9 },
    ]);
    expect(colourDistance(low, high)).toBeGreaterThan(60);

    // Dragged above the lane, the velocity stops at full.
    await app.mouse.move(await x(2000), y(0.5));
    await app.mouse.down();
    await app.mouse.move(await x(2100), box.y - 60, { steps: 4 });
    await app.mouse.up();
    await expect.poll(async () => (await mock.calls("notes_edit")).length).toBe(2);
    const second = (await mock.calls("notes_edit"))[1].args.edit as { update: { velocity: number }[] };
    for (const n of second.update) expect(n.velocity).toBe(1);
  });
});

test.describe("ghost notes", () => {
  // Lead (channel 2) holds a long note on E4; Bass is empty.
  test.use({ seed: { project: twoChannels([note(1, 480, 64, { channel: 2, length: 960, velocity: 1 })]) } });

  test("ghost_notes_show_other_channels_and_are_not_editable", async ({ app, mock }, info) => {
    await openRoll(app);
    await nextFrame(app);
    const ink2 = await token(app, "--ink-2");
    // Away from the grid lines (960 is a beat).
    const points = [await local(app, 1100, 64), await local(app, 3000, 64)];
    const [ghost, bg] = await pixelsAt(app, canvas(app), points);
    await shot(app, info, "ghost");
    // 30 % of Lead's ink over the row: neither the row nor a solid note.
    const want = bg.slice(0, 3).map((c, i) => c * 0.7 + ink2[i] * 0.3);
    expect(colourDistance(ghost, want)).toBeLessThan(25);
    expect(colourDistance(ghost, ink2)).toBeGreaterThan(80);
    expect(colourDistance(ghost, bg)).toBeGreaterThan(15);

    // Not editable: right-click, a drag with the Select tool and a click with Draw leave it be.
    const g = await at(app, 960, 64);
    await app.mouse.click(g.x, g.y, { button: "right" });
    await tool(app, "Select").click();
    await drag(app, g, { x: g.x + 90, y: g.y - 30 });
    await nextFrame(app);
    expect(await mock.calls("notes_edit")).toEqual([]);
    expect(await sessionNotes(mock)).toEqual([[2, 480, 960, 64]]);
    // Draw works through it: a new Bass note appears, the ghost stays.
    await tool(app, "Draw").click();
    await app.mouse.click(g.x, g.y);
    await expect.poll(() => sessionNotes(mock)).toEqual([
      [1, 960, 240, 64],
      [2, 480, 960, 64],
    ]);

    // Switched off, the row is bare again.
    await tool(app, "Select").click();
    await app.mouse.click((await at(app, 3000, 60)).x, (await at(app, 3000, 60)).y);
    await app.getByRole("button", { name: "Ghost notes", exact: true }).click();
    await nextFrame(app);
    const [off] = await pixelsAt(app, canvas(app), [await local(app, 1400, 64)]);
    const [bare] = await pixelsAt(app, canvas(app), [await local(app, 3000, 64)]);
    expect(colourDistance(off, bare)).toBeLessThan(10);
  });
});

test.describe("editing the selection", () => {
  test.use({ seed: { project: twoChannels([note(1, 100, 60, { length: 200 }), note(2, 1130, 62), note(3, 2500, 64), note(4, 100, 70, { channel: 2 })]) } });

  test("alt_q_quantizes_the_selection", async ({ app, mock }) => {
    await openRoll(app);
    await boxSelect(app, 10, 63, 1500, 59);
    await app.keyboard.press("Alt+KeyQ");
    // Starts only: 100 goes to 0, 1130 to 1200; lengths stay; the unselected note is not touched.
    await expect.poll(() => sessionNotes(mock)).toEqual([
      [1, 0, 200, 60],
      [1, 1200, 240, 62],
      [1, 2500, 240, 64],
      [2, 100, 240, 70],
    ]);
    expect(await mock.calls("notes_edit")).toHaveLength(1);
  });

  test("mod_b_duplicates", async ({ app, mock }) => {
    await openRoll(app);
    await boxSelect(app, 10, 63, 1500, 59);
    await app.keyboard.press("ControlOrMeta+KeyB");
    // Notes 1 and 2 span 100..1370: the copies start one bar on, each keeping its place in the bar.
    await expect.poll(() => sessionNotes(mock).then((n) => n.filter((x) => x[0] === 1 && x[1] >= 3840))).toEqual([
      [1, 3940, 200, 60],
      [1, 4970, 240, 62],
    ]);
    // The copies are the selection now: another Mod+B repeats them a bar on again.
    await app.keyboard.press("ControlOrMeta+KeyB");
    await expect.poll(() => sessionNotes(mock).then((n) => n.filter((x) => x[0] === 1 && x[1] >= 7680).length)).toBe(2);
    expect((await sessionNotes(mock)).length).toBe(8);
  });

  test("copy_paste_and_select_all_work", async ({ app, mock }) => {
    await openRoll(app);
    // Mod+A takes every note of the channel, not Lead's; Mod+C remembers them.
    await app.keyboard.press("ControlOrMeta+KeyA");
    await app.keyboard.press("ControlOrMeta+KeyC");
    // Mod+V pastes at the pointer, snapped down to the step grid, keeping the distances.
    const p = await at(app, 3000, 66);
    await app.mouse.move(p.x, p.y);
    await app.keyboard.press("ControlOrMeta+KeyV");
    await expect.poll(() => sessionNotes(mock).then((n) => n.length)).toBe(7);
    const grown = await sessionNotes(mock);
    expect(grown.filter((n) => n[1] >= 2880 && n[0] === 1 && n[1] > 2500)).toEqual([
      [1, 2880, 200, 60],
      [1, 3910, 240, 62],
      [1, 5280, 240, 64],
    ]);
    // Nothing was copied from Lead.
    expect(grown.filter((n) => n[0] === 2)).toHaveLength(1);
    // The pasted notes are selected: Delete removes them.
    await app.keyboard.press("Delete");
    await expect.poll(() => sessionNotes(mock).then((n) => n.length)).toBe(4);
    // Select all and Delete clear Bass and leave Lead's ghost.
    await app.keyboard.press("ControlOrMeta+KeyA");
    await app.keyboard.press("Backspace");
    await expect.poll(() => sessionNotes(mock)).toEqual([[2, 100, 240, 70]]);
  });
});

test.describe("chords", () => {
  test.use({ seed: { project: twoChannels([]) } });

  test("a_chord_stamps_and_strums", async ({ app, mock }) => {
    await openRoll(app);
    const noNoteEdits = async () => expect(await mock.calls("notes_edit")).toHaveLength(0);
    const inversion = pickerButton(app, "Inversion");
    await expect(inversion).toBeDisabled();
    await expect(inversion).toHaveAccessibleDescription("Choose a chord first.");
    await inversion.press("Enter");
    await expect(inversion).toBeFocused();
    await expect(app.getByRole("listbox", { name: "Inversion" })).toBeHidden();
    await confirmPickerWithArrows(app, "Chord", "No chord", "Min7", 8, noNoteEdits);
    await expect(inversion).toBeEnabled();
    await confirmPickerWithArrows(app, "Inversion", "Root position", "1st inversion", 5, noNoteEdits);
    const p = await at(app, 970, 60);
    await app.mouse.click(p.x, p.y);
    // A minor seventh on C, first inversion: the lowest voice goes up an octave. One edit.
    await expect.poll(() => sessionNotes(mock)).toEqual([
      [1, 960, 240, 63],
      [1, 960, 240, 67],
      [1, 960, 240, 70],
      [1, 960, 240, 72],
    ]);
    expect(await mock.calls("notes_edit")).toHaveLength(1);
    // The stamped notes are selected: strum them up, lowest first, 30 ticks a voice.
    await app.getByRole("button", { name: "Strum up", exact: true }).click();
    await expect.poll(() => sessionNotes(mock)).toEqual([
      [1, 960, 240, 63],
      [1, 990, 240, 67],
      [1, 1020, 240, 70],
      [1, 1050, 240, 72],
    ]);
  });
});

test.describe("the key picker", () => {
  test.use({ seed: { project: twoChannels([]) } });

  test("the_key_picker_sends_set_project_key", async ({ app, mock }) => {
    await openRoll(app);
    expect((await stats(app)).scaleMask).toBe(0);
    const edits = async () => (await mock.calls("project_edit")).map((c) => c.args.edit);
    const majorEdit = { kind: "setProjectKey", key: { root: 2, scale: "major" } };
    const minorEdit = { kind: "setProjectKey", key: { root: 2, scale: "minor" } };
    const scale = pickerButton(app, "Scale");
    await expect(scale).toBeDisabled();
    await expect(scale).toHaveAccessibleDescription("Choose a key first.");
    await scale.press("Enter");
    await expect(scale).toBeFocused();
    await expect(app.getByRole("listbox", { name: "Scale" })).toBeHidden();
    await confirmPickerWithArrows(app, "Key", "None", "D", 3, async () => expect(await edits()).toEqual([]));
    await expect.poll(edits).toEqual([majorEdit]);
    await expect.poll(async () => (await stats(app)).scaleMask).toBe(D_MAJOR);
    await expect(scale).toBeEnabled();
    await confirmPickerWithArrows(app, "Scale", "Major", "Minor", 1, async () => expect(await edits()).toEqual([majorEdit]));
    await expect.poll(edits).toEqual([majorEdit, minorEdit]);
    await choosePickerOption(app, "Scale", "Minor");
    expect(await edits()).toEqual([majorEdit, minorEdit]);
    // D minor: D E F G A Bb C.
    await expect.poll(async () => (await stats(app)).scaleMask).toBe(0b011010110101);
    await choosePickerOption(app, "Key", "None");
    await expect.poll(async () => (await edits()).at(-1)).toEqual({ kind: "setProjectKey", key: null });
    await expect.poll(async () => (await stats(app)).scaleMask).toBe(0);
    // The scale and the project's key survive a roll that is closed and opened again.
    await choosePickerOption(app, "Key", "C");
    await expect.poll(async () => (await stats(app)).scaleMask).toBe(C_MAJOR);
    await app.keyboard.press("F7");
    await app.keyboard.press("F7");
    await expect.poll(async () => (await stats(app)).scaleMask).toBe(C_MAJOR);
  });
});

/** Dimmed rows, ghost notes and slide notes, on each renderer. */
for (const renderer of ["webgl2", "canvas2d"] as const) {
  test.describe(`the look on ${renderer}`, () => {
    test.use({
      seed: {
        project: twoChannels(
          [
            note(1, 480, 64, { length: 960, velocity: 1, slide: true }),
            note(2, 480, 67, { length: 960, velocity: 1 }),
            note(3, 480, 69, { channel: 2, length: 960, velocity: 1 }),
          ],
          KEYED,
        ),
      },
    });

    test(`scale_ghosts_and_slides_draw_on_${renderer}`, async ({ app }, info) => {
      if (renderer === "canvas2d") {
        await app.addInitScript(() => {
          const get = HTMLCanvasElement.prototype.getContext;
          HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, type: string, ...rest: unknown[]) {
            return type === "webgl2" ? null : (get as (...a: unknown[]) => unknown).call(this, type, ...rest);
          } as typeof get;
        });
        await app.reload();
        await expect(app.getByRole("button", { name: "Stop", exact: true })).toBeVisible();
      }
      await openRoll(app);
      expect((await stats(app)).renderer).toBe(renderer);
      await nextFrame(app);
      await shot(app, info, renderer);
      const [dimmed, lit] = await pixelsAt(app, canvas(app), [await local(app, 3000, 60), await local(app, 3000, 62)]);
      expect(brightness(dimmed)).toBeLessThan(brightness(lit) * 0.8);

      const ink2 = await token(app, "--ink-2");
      const ink4 = await token(app, "--ink-4");
      const slideInk = await token(app, "--ink-4b");
      const v = (await stats(app)).view;
      const corner = async (key: number) => {
        const m = await local(app, 1440, key);
        return { x: m.x - 3, y: m.y + v.keyPx / 2 - 3 };
      };
      const [ghost, row, slideMid, slideCorner, plainCorner] = await pixelsAt(app, canvas(app), [
        await local(app, 1100, 69),
        await local(app, 3000, 69),
        await local(app, 960, 64),
        await corner(64),
        await corner(67),
      ]);
      const want = row.slice(0, 3).map((c, i) => c * 0.7 + ink2[i] * 0.3);
      expect(colourDistance(ghost, want)).toBeLessThan(25);
      expect(colourDistance(slideMid, slideInk)).toBeLessThan(40);
      expect(colourDistance(slideCorner, slideInk)).toBeGreaterThan(60);
      expect(colourDistance(plainCorner, ink4)).toBeLessThan(40);
    });
  });
}


/** Whether each (tick, key) shows a note of Bass's ink at velocity 0.8 (what a drawn note gets). */
async function inkedAt(page: Page, points: readonly (readonly [number, number])[]) {
  const full = await token(page, "--ink-4");
  const want = full.map((c) => c * (0.45 + 0.55 * 0.8));
  const px = await pixelsAt(page, canvas(page), await Promise.all(points.map(([t, k]) => local(page, t, k))));
  return px.map((c) => colourDistance(c, want) < 45);
}

test.describe("scale snap on paste and group moves", () => {
  test.use({
    seed: { project: twoChannels([note(1, 480, 60), note(2, 480, 64), note(3, 480, 67)], { key: { root: 0, scale: "major" } }) },
  });

  test("a_group_move_snaps_every_voice_into_the_scale", async ({ app, mock }, info) => {
    await openRoll(app);
    await boxSelect(app, 10, 70, 1500, 58);
    // Grab C and hold it two rows up, on D: E would be F# (not in C major) and G would be A.
    const from = await at(app, 540, 60);
    await app.mouse.move(from.x, from.y);
    await app.mouse.down();
    const to = await at(app, 540, 62);
    await app.mouse.move(to.x, to.y, { steps: 4 });
    await nextFrame(app);
    await shot(app, info, "group-move-held");
    // The preview shows the keys the commit will send: D, F and A, and no F#.
    expect(await inkedAt(app, [[540, 62], [540, 65], [540, 69], [540, 66], [540, 60], [540, 64], [540, 67]])).toEqual([true, true, true, false, false, false, false]);
    expect(await mock.calls("notes_edit")).toEqual([]);
    await app.mouse.up();
    await expect.poll(() => sessionNotes(mock)).toEqual([
      [1, 480, 240, 62],
      [1, 480, 240, 65],
      [1, 480, 240, 69],
    ]);
    expect(await mock.calls("notes_edit")).toHaveLength(1);
    await nextFrame(app);
    expect(await inkedAt(app, [[540, 62], [540, 65], [540, 69], [540, 66]])).toEqual([true, true, true, false]);
  });
});

test.describe("paste into the key", () => {
  test.use({ seed: { project: twoChannels([note(1, 0, 61)], { key: { root: 0, scale: "major" } }) } });

  test("paste_snaps_to_the_scale", async ({ app, mock }) => {
    await openRoll(app);
    // C# is outside C major. Copy it, paste at 960 with the snap on: it lands on C.
    await app.keyboard.press("ControlOrMeta+KeyA");
    await app.keyboard.press("ControlOrMeta+KeyC");
    const p = await at(app, 960, 66);
    await app.mouse.move(p.x, p.y);
    await app.keyboard.press("ControlOrMeta+KeyV");
    await expect.poll(() => sessionNotes(mock)).toEqual([
      [1, 960, 240, 60],
      [1, 0, 240, 61],
    ].sort((a, b) => a[3] - b[3] || a[1] - b[1]));
    // With the snap off the same clip keeps its key.
    await app.getByRole("button", { name: "Snap to scale", exact: true }).click();
    const q = await at(app, 1920, 66);
    await app.mouse.move(q.x, q.y);
    await app.keyboard.press("ControlOrMeta+KeyV");
    await expect.poll(() => sessionNotes(mock).then((n) => n.length)).toBe(3);
    expect((await sessionNotes(mock)).filter((n) => n[1] === 1920).map((n) => n[3])).toEqual([61]);
  });
});

test.describe("a dragged chord stamp", () => {
  test.use({ seed: { project: twoChannels([]) } });

  test("a_dragged_chord_stamp_is_one_edit_and_a_preview_until_release", async ({ app, mock }, info) => {
    await openRoll(app);
    await choosePickerOption(app, "Chord", "Major");
    const from = await at(app, 970, 60);
    await app.mouse.move(from.x, from.y);
    await app.mouse.down();
    const to = await at(app, 1450, 62);
    await app.mouse.move(to.x, to.y, { steps: 5 });
    await nextFrame(app);
    await shot(app, info, "stamp-held");
    // Held: the chord is on screen at the new place (D, F#, A at 1440), not at the old one, and
    // nothing has gone to the engine.
    expect(await inkedAt(app, [[1500, 62], [1500, 66], [1500, 69], [1000, 60], [1000, 64]])).toEqual([true, true, true, false, false]);
    expect(await mock.calls("notes_edit")).toEqual([]);
    await app.mouse.up();
    // Released: one edit, an add of the three notes where they were let go.
    await expect.poll(() => sessionNotes(mock)).toEqual([
      [1, 1440, 240, 62],
      [1, 1440, 240, 66],
      [1, 1440, 240, 69],
    ]);
    const edits = await mock.calls("notes_edit");
    expect(edits).toHaveLength(1);
    const sent = edits[0].args.edit as { add: unknown[]; update: unknown[]; delete: unknown[] };
    expect([sent.add.length, sent.update.length, sent.delete.length]).toEqual([3, 0, 0]);
  });
});

test.describe("held gestures", () => {
  test.use({ seed: { project: twoChannels([]) } });

  const gestures = [
    { name: "a_drawn_note", chord: "", from: [970, 60], to: [1450, 62], held: [[1500, 62]], gone: [[1000, 60]] },
    { name: "a_stamped_chord", chord: "Major", from: [970, 60], to: [1450, 62], held: [[1500, 62], [1500, 66], [1500, 69]], gone: [[1000, 60]] },
  ] as const;
  for (const g of gestures) {
    test(`held_notes_survive_an_engine_refresh_${g.name}`, async ({ app, mock }) => {
      await openRoll(app);
      if (g.chord) await choosePickerOption(app, "Chord", g.chord);
      const from = await at(app, g.from[0], g.from[1]);
      await app.mouse.move(from.x, from.y);
      await app.mouse.down();
      const to = await at(app, g.to[0], g.to[1]);
      await app.mouse.move(to.x, to.y, { steps: 4 });
      await nextFrame(app);
      expect(await inkedAt(app, [...g.held])).toEqual(g.held.map(() => true));
      // Another channel gets a note while the pointer is down: the engine announces it and the roll fetches.
      const fetches = (await mock.calls("pattern_notes")).length;
      await mock.eval(`(m) => m.addNote({ channel: 2, tick: 2400, length: 240, key: 70, velocity: 0.8 })`);
      await expect.poll(async () => (await mock.calls("pattern_notes")).length).toBeGreaterThan(fetches);
      await nextFrame(app);
      await nextFrame(app);
      // The held notes are still on screen, where the gesture has them, and nothing was sent.
      expect(await inkedAt(app, [...g.held])).toEqual(g.held.map(() => true));
      expect(await mock.calls("notes_edit")).toEqual([]);
      await app.mouse.up();
      // A held gesture keeps its starting revision; the concurrent edit wins atomically.
      expect(await mock.calls("notes_edit")).toHaveLength(0);
      await expect(app.getByRole("status", { name: "Messages" })).toContainText("pattern changed");
      await expect.poll(() => sessionNotes(mock).then((n) => n.filter((x) => x[0] === 1).length)).toBe(0);
      expect((await sessionNotes(mock)).filter((n) => n[0] === 2)).toHaveLength(1);
    });
  }

  test("held_notes_survive_an_engine_refresh_a_paint_stroke", async ({ app, mock }) => {
    await openRoll(app);
    await tool(app, "Paint").click();
    const from = await at(app, 10, 64);
    await app.mouse.move(from.x, from.y);
    await app.mouse.down();
    const to = await at(app, 3 * 240 + 10, 64);
    await app.mouse.move(to.x, to.y, { steps: 6 });
    await nextFrame(app);
    const cells: [number, number][] = [[100, 64], [340, 64], [580, 64], [820, 64]];
    expect(await inkedAt(app, cells)).toEqual([true, true, true, true]);
    const fetches = (await mock.calls("pattern_notes")).length;
    await mock.eval(`(m) => m.addNote({ channel: 2, tick: 2400, length: 240, key: 70, velocity: 0.8 })`);
    await expect.poll(async () => (await mock.calls("pattern_notes")).length).toBeGreaterThan(fetches);
    await nextFrame(app);
    await nextFrame(app);
    expect(await inkedAt(app, cells)).toEqual([true, true, true, true]);
    expect(await mock.calls("notes_edit")).toEqual([]);
    await app.mouse.up();
    expect(await mock.calls("notes_edit")).toHaveLength(0);
    await expect(app.getByRole("status", { name: "Messages" })).toContainText("pattern changed");
    await expect.poll(() => sessionNotes(mock).then((n) => n.filter((x) => x[0] === 1).length)).toBe(0);
    expect((await sessionNotes(mock)).filter((n) => n[0] === 2)).toHaveLength(1);
  });

  test("a_paint_stroke_is_one_edit_and_a_cancelled_one_leaves_nothing", async ({ app, mock }) => {
    await openRoll(app);
    await tool(app, "Paint").click();
    const stroke = async (release: "up" | "cancel", row: number) => {
      const from = await at(app, 10, row);
      await app.mouse.move(from.x, from.y);
      await app.mouse.down();
      const to = await at(app, 3 * 240 + 10, row);
      await app.mouse.move(to.x, to.y, { steps: 6 });
      await nextFrame(app);
      // Held: on screen, nothing sent.
      expect(await inkedAt(app, [[100, row], [820, row]])).toEqual([true, true]);
      expect(await mock.calls("notes_edit")).toEqual([]);
      if (release === "up") await app.mouse.up();
      else {
        // The browser cancels the pointer (a system gesture): the roll gets pointercancel, and the
        // button is let go afterwards.
        await canvas(app).dispatchEvent("pointercancel");
        await app.mouse.up();
      }
      await nextFrame(app);
    };
    // Cancelled: the stroke is taken back, and nothing was ever sent.
    await stroke("cancel", 64);
    expect(await inkedAt(app, [[100, 64], [820, 64]])).toEqual([false, false]);
    expect(await mock.calls("notes_edit")).toEqual([]);
    expect(await sessionNotes(mock)).toEqual([]);
    // Released: one edit with all four cells.
    await app.mouse.move(0, 0);
    await stroke("up", 66);
    await expect.poll(async () => (await mock.calls("notes_edit")).length).toBe(1);
    expect(((await mock.calls("notes_edit"))[0].args.edit as { add: unknown[] }).add).toHaveLength(4);
    expect(await sessionNotes(mock)).toHaveLength(4);
  });
});
