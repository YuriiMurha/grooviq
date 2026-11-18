// The mock engine keeps the shell's contract (#121, #187): recording pairs a release with its
// own press, a rack preview is a live note like any other, the shell's note bounds and ids,
// and the settings and autosave state it reports. Driven through the command layer, as the UI's
// `invoke` would call it.
import type { Page } from "@playwright/test";
import { expect, test } from "./fixture";
import type { MockNote } from "./mockTauri";
import type { ProjectView } from "../src/api/project";
import { twoChannels } from "./rollHelpers";

/** Calls a shell command straight from the page. */
const call = <T = unknown>(page: Page, cmd: string, args: Record<string, unknown> = {}) =>
  page.evaluate(
    ([cmd, args]) =>
      (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__.invoke(cmd, args),
    [cmd, args] as const,
  ) as Promise<T>;

/** The error a command rejects with. */
const refusal = (page: Page, cmd: string, args: Record<string, unknown>) =>
  page.evaluate(
    ([cmd, args]) =>
      (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__.invoke(cmd, args).then(
        () => null,
        (e: unknown) => String(e),
      ),
    [cmd, args] as const,
  );

const hold = (page: Page, ticks: number) => page.evaluate((t) => (window as unknown as { __grooviqMock: { hold: (t: number) => void } }).__grooviqMock.hold(t), ticks);

/** The session's notes as [tick, length, key, velocity], by tick. */
const recorded = (page: Page) =>
  page
    .evaluate(() => (window as unknown as { __grooviqMock: { notes: () => Required<MockNote>[] } }).__grooviqMock.notes())
    .then((ns) => ns.map((n) => [n.tick, n.length, n.key, n.velocity]).sort((a, b) => a[0] - b[0]));

/** Recording, playing, the clock pinned at 0. */
async function take(page: Page) {
  await call(page, "record_set", { on: true });
  await call(page, "transport_play");
  await hold(page, 0);
}

const note = (tick: number, length: number, key = 60) => ({ channel: 1, tick, length, key, velocity: 0.8, slide: false });
type Ack = { revision: number; addedIds: number[]; reload: boolean };
/** A deliberate edit uses the current canonical pattern origin. */
const edit = async (page: Page, add: object[], del: number[] = []) => {
  const { revision: baseRevision, generation, patternId } = await patternStamp(page);
  return call<Ack>(page, "notes_edit", { edit: { generation, patternId, baseRevision, add, update: [], delete: del } });
};
const patternStamp = async (page: Page) => {
  const view = await call<ProjectView>(page, "project_view");
  return { generation: view.generation, patternId: view.pattern.id, revision: view.notesRevision };
};
/** Undo in the project the mock holds now. */
const undo = async (page: Page) => call(page, "notes_undo", { stamp: await patternStamp(page) });

test.describe("recording pairs each release with its own press", () => {
  test.use({ seed: { project: twoChannels([]) } });

  // A typed C4 (velocity 0.5) from 0, the rack's preview of C4 (0.8) from 120; the first
  // release at 240, the second at 480.
  for (const innerFirst of [true, false]) {
    test(`a_typed_key_and_a_rack_preview_on_one_key_${innerFirst ? "inner" : "outer"}_released_first`, async ({ app }) => {
      await take(app);
      const typed = await call<number>(app, "channel_note", { channel: 1, key: 60, velocity: 0.5, on: true });
      await hold(app, 120);
      const preview = await call<number>(app, "channel_preview", { channel: 1, on: true });
      const releaseTyped = () => call(app, "channel_note", { channel: 1, key: 60, velocity: 0, on: false, press: typed });
      const releasePreview = () => call(app, "channel_preview", { channel: 1, on: false, press: preview });
      await hold(app, 240);
      await (innerFirst ? releasePreview() : releaseTyped());
      await hold(app, 480);
      await (innerFirst ? releaseTyped() : releasePreview());
      expect(await recorded(app)).toEqual(
        innerFirst
          ? [
              [0, 480, 60, 0.5],
              [120, 120, 60, 0.8],
            ]
          : [
              [0, 240, 60, 0.5],
              [120, 360, 60, 0.8],
            ],
      );
    });
  }

  test("a_rack_preview_records_like_a_key", async ({ app }) => {
    await take(app);
    const preview = await call<number>(app, "channel_preview", { channel: 1, on: true });
    expect(await app.evaluate(() => (window as unknown as { __grooviqMock: { live: () => string[] } }).__grooviqMock.live())).toEqual(["1:60"]);
    await hold(app, 240);
    await call(app, "channel_preview", { channel: 1, on: false, press: preview });
    expect(await recorded(app)).toEqual([[0, 240, 60, 0.8]]);
  });

  test("a_release_after_its_take_ended_leaves_the_next_takes_press_alone", async ({ app }) => {
    await take(app);
    const first = await call<number>(app, "channel_note", { channel: 1, key: 60, velocity: 0.5, on: true });
    await hold(app, 300);
    // Record off ends the take (the first press closes at 300); a new take starts.
    await call(app, "record_set", { on: false });
    await call(app, "record_set", { on: true });
    await hold(app, 400);
    const second = await call<number>(app, "channel_note", { channel: 1, key: 60, velocity: 0.9, on: true });
    await hold(app, 500);
    await call(app, "channel_note", { channel: 1, key: 60, velocity: 0, on: false, press: first });
    await hold(app, 600);
    await call(app, "channel_note", { channel: 1, key: 60, velocity: 0, on: false, press: second });
    expect(await recorded(app)).toEqual([
      [0, 300, 60, 0.5],
      [400, 200, 60, 0.9],
    ]);
  });

  test("keys_outside_0_to_127_are_refused_and_velocity_is_clamped", async ({ app }) => {
    await take(app);
    expect(await refusal(app, "channel_note", { channel: 1, key: 128, velocity: 0.5, on: true })).toContain("128");
    const loud = await call<number>(app, "channel_note", { channel: 1, key: 61, velocity: 1.5, on: true });
    const quiet = await call<number>(app, "channel_note", { channel: 1, key: 62, velocity: -1, on: true });
    await hold(app, 240);
    await call(app, "channel_note", { channel: 1, key: 61, velocity: 0, on: false, press: loud });
    await call(app, "channel_note", { channel: 1, key: 62, velocity: 0, on: false, press: quiet });
    expect(await recorded(app)).toEqual([
      [0, 240, 61, 1],
      [0, 240, 62, 0],
    ]);
    // A release names a key in range too.
    expect(await refusal(app, "channel_note", { channel: 1, key: 128, velocity: 0, on: false, press: loud })).toContain("128");
  });
});

test.describe("note ids and bounds", () => {
  test.use({ seed: { project: twoChannels([]) } });

  test("ids_are_never_handed_out_twice", async ({ app }) => {
    const [a] = (await edit(app, [note(0, 240)])).addedIds;
    // Deleted and added in one edit: the new note gets a new id.
    const [b] = (await edit(app, [note(0, 240)], [a])).addedIds;
    expect(b).toBeGreaterThan(a);
    // Undone: the next note still gets a new id; redo is gone, so b never comes back.
    await undo(app);
    const [c] = (await edit(app, [note(480, 240)])).addedIds;
    expect(c).toBeGreaterThan(b);
    // Undone back to the start: still never an id handed out before.
    await undo(app);
    await undo(app);
    const [d] = (await edit(app, [note(960, 240)])).addedIds;
    expect(d).toBeGreaterThan(c);
  });

  test("tick_and_length_clamp_to_the_shells_bounds_and_ask_for_a_reload", async ({ app }) => {
    const far = await edit(app, [note(100_000, 100_000)]);
    expect(far.reload).toBe(true);
    const inRange = await edit(app, [note(61_439, 61_440, 61)]);
    expect(inRange.reload).toBe(false);
    expect(await recorded(app)).toEqual([
      [61_439, 61_440, 60, 0.8],
      [61_439, 61_440, 61, 0.8],
    ]);
  });
});

test.describe("settings the shell keeps", () => {
  test("audio_select_and_audio_set_guard_are_remembered", async ({ app }) => {
    expect(await call(app, "audio_settings")).toEqual({ output: "default", guard: true });
    await call(app, "audio_set_guard", { on: false });
    await call(app, "audio_select", { id: "null" });
    expect(await call(app, "audio_settings")).toEqual({ output: "null", guard: false });
    // An unknown output is refused and changes nothing.
    expect(await refusal(app, "audio_select", { id: "nope" })).not.toBeNull();
    expect(await call(app, "audio_settings")).toEqual({ output: "null", guard: false });
    await call(app, "audio_select", { id: "default" });
    expect(await call(app, "audio_settings")).toEqual({ output: "default", guard: false });
  });

  test("autosave_status_is_quiet_by_default", async ({ app }) => {
    expect(await call(app, "autosave_status")).toEqual({ disabled: null, failure: null });
  });
});

test.describe("autosave trouble at startup", () => {
  test.use({ seed: { autosave: { disabled: "Autosave is off in this window.", failure: "Autosave is failing." } } });

  test("the_ui_warns_of_a_disabled_and_a_failing_autosave", async ({ app }) => {
    const warnings = app.getByRole("status", { name: "Messages" }).locator(".toast.warn");
    await expect(warnings).toHaveCount(2);
    // A successful autosave ends the failure; the disabled state stays.
    await app.evaluate(() => (window as unknown as { __grooviqMock: { autosave: () => void } }).__grooviqMock.autosave());
    expect(await call(app, "autosave_status")).toEqual({ disabled: "Autosave is off in this window.", failure: null });
  });
});

test.describe("recording when the pattern's note ids run out", () => {
  // One id left (session.rs `collect_recorded`: u32::MAX is never handed out).
  const lastId = twoChannels([]);
  lastId.pattern.nextNoteId = 0xffff_fffe;
  test.use({ seed: { project: lastId } });

  test("keeps_what_fits_switches_recording_off_and_counts_the_rest", async ({ app, mock }) => {
    const stops = await app.evaluate(async () => {
      const w = window as unknown as {
        __TAURI_INTERNALS__: { invoke: (c: string, a?: unknown) => Promise<unknown>; transformCallback: (cb: (e: { payload: unknown }) => void) => number };
        __grooviqMock: { hold: (t: number) => void };
      };
      const t = w.__TAURI_INTERNALS__;
      const got: unknown[] = [];
      const handler = t.transformCallback((e) => got.push(e.payload));
      await t.invoke("plugin:event|listen", { event: "recording-stopped", target: { kind: "Any" }, handler });
      await t.invoke("record_set", { on: true });
      await t.invoke("transport_play");
      w.__grooviqMock.hold(0);
      const press = (key: number) => t.invoke("channel_note", { channel: 1, key, velocity: 0.8, on: true });
      const release = (key: number, p: unknown) => t.invoke("channel_note", { channel: 1, key, velocity: 0, on: false, press: p });
      const a = await press(60);
      w.__grooviqMock.hold(240);
      await release(60, a);
      // Two more held; the first let go finds no id left, the other is still down.
      const b = await press(61);
      const c = await press(62);
      w.__grooviqMock.hold(480);
      await release(61, b);
      w.__grooviqMock.hold(720);
      await release(62, c);
      return got;
    });
    expect(stops).toEqual([expect.objectContaining({ reason: "noteIds", dropped: 2 })]);
    expect(await mock.eval("(m) => m.state.recording")).toBe(false);
    expect(await recorded(app)).toEqual([[0, 240, 60, 0.8]]);
    await expect(app.getByRole("status", { name: "Messages" }).locator(".toast.warn")).toHaveCount(1);
    await expect(app.getByRole("button", { name: "Record", exact: true })).toHaveAttribute("aria-pressed", "false");
  });
});


test("play_while_playing_preserves_the_clock_and_held_position", async ({ app, mock }) => {
  await call(app, "transport_play");
  await hold(app, 4320);
  const before = await mock.eval("(m) => ({ ...m.state.transport })");
  await call(app, "transport_play");
  expect(await mock.eval("(m) => ({ ...m.state.transport })")).toEqual(before);
  expect(await mock.eval("(m) => m.ticks()")).toBe(4320);
  await call(app, "transport_stop");
  await call(app, "transport_play");
  expect(await mock.eval("(m) => m.state.transport.playing")).toBe(true);
  expect(await mock.eval("(m) => m.state.transport.held")).toBeNull();
  expect(await mock.eval("(m) => m.state.transport.startTicks")).toBe(0);
});


test.describe("autosave initialization failure", () => {
  const disabled = "Autosave and recovery could not start: startup-probe. Your changes are not being backed up; save the project to keep them.";
  test.use({ seed: { autosave: { disabled } } });

  test("retains unavailable status and warns once at startup", async ({ app }) => {
    const warnings = app.getByRole("status", { name: "Messages" }).locator(".toast.warn");
    await expect(warnings).toHaveCount(1);
    await expect(warnings).toContainText([disabled]);
    for (let i = 0; i < 3; i++) {
      expect(await call(app, "autosave_status")).toEqual({ disabled, failure: null });
    }
    await expect(warnings).toHaveCount(1);
  });
});
