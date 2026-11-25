// The mock engine keeps the shell's contract (#121, #187): recording pairs a release with its
// own press, a rack preview is a live note like any other, the shell's note bounds and ids,
// and the settings and autosave state it reports. Driven through the command layer, as the UI's
// `invoke` would call it.
import type { Page } from "@playwright/test";
import { expect, generatedWav, project, test } from "./fixture";
import type { ArrangementPage, ArrangementReply } from "../src/api/arrangement";
import { decodeWaveform } from "../src/api/waveform";
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
  test.use({ seed: { audioLog: [], autosave: { disabled: "Autosave is off in this window.", failure: "Autosave is failing." } } });

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
  test.use({ seed: { audioLog: [], project: lastId } });

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

test.describe("MIDI ownership", () => {
  test.use({ seed: { project: twoChannels([]) } });
  for (const zeroVelocityOn of [false, true]) {
    test(`note_off_releases_every_repeated_MIDI_press_${zeroVelocityOn}`, async ({ app, mock }) => {
      await call(app, "channel_select", { channel: 1 });
      await take(app);
      await mock.eval("(m) => { m.midiNote(60, 127, true); m.midiNote(60, 64, true); }");
      await hold(app, 240);
      await mock.eval(`(m) => m.midiNote(60, 0, ${zeroVelocityOn})`);
      expect(await mock.eval("(m) => m.live()")).toEqual([]);
      expect(await recorded(app)).toEqual([[0, 240, 60, 1], [0, 240, 60, 64 / 127]]);
    });
  }

  test("selection_releases_MIDI_only_and_stale_key_up_leaves_new_producers_alone", async ({ app, mock }) => {
    await call(app, "channel_select", { channel: 1 });
    await take(app);
    const midi = await mock.eval<number>("(m) => m.midiNote(60, 127, true)");
    const again = await mock.eval<number>("(m) => m.midiNote(60, 64, true)");
    const typed = await call<number>(app, "channel_note", { channel: 1, key: 60, velocity: 0.5, on: true });
    expect(new Set([midi, again, typed]).size).toBe(3);
    await hold(app, 120);
    await call(app, "channel_select", { channel: 1 });
    expect(await recorded(app)).toEqual([]);
    await hold(app, 240);
    await call(app, "channel_select", { channel: 2 });
    expect(await mock.eval("(m) => m.live()")).toEqual(["1:60"]);
    expect(await recorded(app)).toEqual([[0, 240, 60, 1], [0, 240, 60, 64 / 127]]);
    await mock.eval("(m) => m.midiNote(60, 0, false)");
    expect(await mock.eval("(m) => m.live()")).toEqual(["1:60"]);
    await hold(app, 480);
    await call(app, "channel_note", { channel: 1, key: 60, velocity: 0, on: false, press: typed });
    expect(await mock.eval("(m) => m.live()")).toEqual([]);
    await call(app, "channel_select", { channel: null });
    expect(await mock.eval("(m) => m.midiNote(60, 127, true)")).toBeNull();
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
  test.use({ seed: { audioLog: [], autosave: { disabled } } });

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

test.describe("arrangement media contract", () => {
  const path = "/qa/audio/generated.wav";
  const badPath = "/qa/audio/refused.wav";
  const replacementPath = "/qa/audio/replacement.wav";
  const savePath = "/qa/media.grooviq";
  const clipName = "  Take 01 / edit  ";
  const initial = project([], {}, {
    nextId: 2,
    arrangement: { tracks: [{ id: 1, name: "Audio", color: 0, mute: false }], clips: [], audioAssets: [], loopRegion: null },
  });
  const view = (page: Page) => call<ArrangementPage>(page, "arrangement_view", { offset: 0, limit: 32 });
  const stamp = ({ generation, revision }: ArrangementPage) => ({ generation, revision });
  const importArgs = (state: ArrangementPage, audioPath = path) => ({
    ...stamp(state), startTick: 240, track: 1, name: clipName, path: audioPath, ticket: 12,
  });
  test.use({ seed: { project: initial }, deviceScaleFactor: 2 });

  test("native file drops broadcast physical enter, over and drop coordinates", async ({ app, mock }) => {
    // Tauri manager/webview.rs::on_webview_event forwards PhysicalPosition, and
    // src/rack/fileDrop.ts subscribes independently of a position-aware consumer.
    await app.evaluate(() => {
      const shell = (window as unknown as { __TAURI_INTERNALS__: {
        invoke: (command: string, args: unknown) => Promise<unknown>;
        transformCallback: (handler: (event: { payload: { position: { x: number; y: number } } }) => void) => number;
      } }).__TAURI_INTERNALS__;
      const rack: unknown[] = [], playlist: unknown[] = [];
      const register = (event: string, handler: Parameters<typeof shell.transformCallback>[0]) =>
        shell.invoke("plugin:event|listen", { event, handler: shell.transformCallback(handler) });
      return Promise.all(["enter", "over", "drop"].flatMap(type => {
        const event = `tauri://drag-${type}`;
        return [
          register(event, ({ payload }) => rack.push({ type, ...payload })),
          register(event, ({ payload }) => playlist.push({
            type, x: payload.position.x / devicePixelRatio, y: payload.position.y / devicePixelRatio,
          })),
        ];
      })).then(() => {
        // Store the arrays by reference so the next browser action can deliver the events.
        Object.assign(window, { __audioDropEvents: { rack, playlist, dpr: devicePixelRatio } });
      });
    });
    await mock.dropAudio(path, { x: 311, y: 127 });
    expect(await app.evaluate(() => Reflect.get(window, "__audioDropEvents"))).toEqual({
      dpr: 2,
      rack: [
        { type: "enter", paths: [path], position: { x: 622, y: 254 } },
        { type: "over", position: { x: 622, y: 254 } },
        { type: "drop", paths: [path], position: { x: 622, y: 254 } },
      ],
      playlist: ["enter", "over", "drop"].map(type => ({ type, x: 311, y: 127 })),
    });
  });

  test("import uses the caller name and adds an asset and clip in one history step", async ({ app, mock }) => {
    await mock.registerAudio(path, generatedWav());
    const before = await view(app);
    // project/src/session/media.rs::publish_media allocates the asset before the clip.
    const reply = await call<ArrangementReply>(app, "audio_import", importArgs(before));
    expect(reply).toEqual({ ...stamp(before), revision: 1, status: "applied", request: 1,
      createdIds: [2, 3], changed: true, reload: false, message: null, warnings: [] });
    const applied = await view(app);
    expect(applied.audioAssets).toEqual([{ id: 2, sample: { path: "generated.wav", file: null },
      sourceRate: 32000, sourceFrames: 8, originalTempo: null, originalKey: null }]);
    expect(applied.clips).toEqual([{ id: 3, track: 1, name: clipName, startTick: 240, muted: false,
      content: { type: "audio", asset: 2, sourceStart: 0, sourceEnd: 8, gain: 1, reverse: false, fadeIn: 0, fadeOut: 0 } }]);
    expect(await call(app, "arrangement_history", { ...stamp(applied), undo: true }))
      .toEqual({ generation: 0, revision: 2, status: "applied", effect: { kind: "arrangement" } });
    expect(await view(app)).toEqual({ ...before, revision: 2 });
    expect(await call(app, "arrangement_history", { ...stamp(await view(app)), undo: false }))
      .toEqual({ generation: 0, revision: 3, status: "applied", effect: { kind: "arrangement" } });
    expect(await view(app)).toEqual({ ...applied, revision: 3 });
  });

  test("save and open keep embedded audio with its clip after the original disappears", async ({ app, mock }) => {
    await mock.registerAudio(path, generatedWav());
    await call(app, "audio_import", importArgs(await view(app)));
    const applied = await view(app);
    await call(app, "project_save_as", { path: savePath });
    await call(app, "project_new");
    await mock.eval(`(m) => m.files.delete(${JSON.stringify(path)})`);
    await call(app, "project_open", { path: savePath });
    const reopened = await view(app);
    expect(reopened).toEqual({ ...applied, generation: 2, revision: 0, audioAssets: [
      { ...applied.audioAssets[0], sample: { path: "generated.wav", file: "samples/1-generated.wav" } },
    ] });
    const bytes = await mock.eval<number[]>(`async () => Array.from(new Uint8Array(await window.__TAURI_INTERNALS__.invoke(
      "arrangement_waveform", { generation: 2, request: 41, asset: 2, start: 0, end: 8, bins: 1 })))`);
    const waveform = decodeWaveform(Uint8Array.from(bytes).buffer);
    expect(waveform).toMatchObject({ generation: 2n, request: 41n, asset: 2, frames: 8, rate: 32000 });
    expect(Array.from(waveform.peaks)).toEqual([-1, 32767 / 32768, -1, 32767 / 32768]);
  });

  test("an admitted import exposes its pending request and cannot publish into a replacement", async ({ app, mock }) => {
    await mock.registerAudio(path, generatedWav());
    await mock.holdAudio(path);
    const before = await view(app);
    const args = importArgs(before);
    await mock.eval(`() => { window.__audioReply = window.__TAURI_INTERNALS__.invoke("audio_import", ${JSON.stringify(args)}); }`);
    expect(await view(app)).toEqual({ ...before, pendingRequests: [1] });
    await call(app, "project_new");
    const replacement = await view(app);
    expect(replacement.pendingRequests).toEqual([]);
    await mock.releaseAudio(path);
    // Native publication rechecks arrangement_stamp even after the worker returns success.
    expect(await mock.eval("() => window.__audioReply")).toEqual({ generation: 1, revision: 0, status: "refused",
      request: 1, createdIds: [], changed: false, reload: true, message: "Project replaced; reload arrangement", warnings: [] });
    expect(await view(app)).toEqual(replacement);
  });

  test("a held import rechecks the revision within the same project before committing", async ({ app, mock }) => {
    await mock.registerAudio(path, generatedWav());
    await mock.holdAudio(path);
    const before = await view(app);
    await mock.eval(`() => { window.__audioReply = window.__TAURI_INTERNALS__.invoke("audio_import", ${JSON.stringify(importArgs(before))}); }`);
    expect((await view(app)).pendingRequests).toEqual([1]);
    await call(app, "arrangement_edit", { ...stamp(before),
      editsJson: JSON.stringify([{ kind: "createTrack", name: "Second", color: 1 }]) });
    const edited = await view(app);
    expect(edited.revision).toBe(1);
    await mock.releaseAudio(path);
    expect(await mock.eval("() => window.__audioReply")).toMatchObject({ ...stamp(edited), request: 1,
      status: "refused", message: "Stale arrangement revision; reload arrangement" });
    expect(await view(app)).toEqual({ ...edited, pendingRequests: [] });
  });

  test("decode refusal is selected by path and leaves the project and history alone", async ({ app, mock }) => {
    await mock.registerAudio(path, generatedWav());
    await mock.registerAudio(badPath, null);
    await mock.holdAudio(badPath);
    const before = await view(app);
    await mock.eval(`() => { window.__audioReply = window.__TAURI_INTERNALS__.invoke("audio_import", ${JSON.stringify(importArgs(before, badPath))}); }`);
    expect((await view(app)).pendingRequests).toEqual([1]);
    await mock.releaseAudio(badPath);
    expect(await mock.eval("() => window.__audioReply")).toEqual({ ...stamp(before), status: "refused", request: 1,
      createdIds: [], changed: false, reload: true, warnings: [],
      message: 'Media preparation failed: Decode("This file is not a sound Grooviq can open; use WAV, AIFF, FLAC, MP3 or OGG.")' });
    expect(await view(app)).toEqual(before);
    expect((await mock.view()).dirty).toBe(false);
    expect(await call(app, "arrangement_history", { ...stamp(before), undo: true }))
      .toEqual({ ...stamp(before), status: "applied", effect: { kind: "empty" } });
    await mock.holdAudio(path);
    await mock.eval(`() => { window.__audioReply = window.__TAURI_INTERNALS__.invoke("audio_import", ${JSON.stringify(importArgs(before))}); }`);
    expect((await view(app)).pendingRequests).toEqual([2]);
    await mock.refuseAudio(path);
    expect(await mock.eval("() => window.__audioReply")).toMatchObject({ status: "refused", request: 2 });
    expect(await view(app)).toEqual(before);
    expect(await call(app, "audio_import", importArgs(before))).toMatchObject({ status: "applied", request: 3 });
  });

  test("relink preserves provenance, has its own undo step and invalidates a held waveform", async ({ app, mock }) => {
    const provenance = { id: 2, sample: { path: "lost.wav", file: null }, sourceFrames: 8, sourceRate: 32000,
      originalTempo: 123, originalKey: { root: 2, scale: "major" } };
    await mock.eval(`(m) => m.files.set("/qa/missing.grooviq", { kind: "project", project: ${JSON.stringify({
      ...initial, nextId: 3, arrangement: { ...initial.arrangement, audioAssets: [provenance] },
    })} })`);
    await call(app, "project_open", { path: "/qa/missing.grooviq" });
    const missing = await view(app);
    const request = { generation: missing.generation, asset: 2, request: 777, start: 1, end: 7, bins: 2 };
    expect(await refusal(app, "arrangement_waveform", request)).toBe("Audio is missing; relink it");

    await mock.registerAudio(path, generatedWav());
    await mock.holdAudio(path);
    await mock.eval(`() => { window.__audioReply = window.__TAURI_INTERNALS__.invoke("audio_relink", ${JSON.stringify({
      ...stamp(missing), asset: 2, path, ticket: 26,
    })}); }`);
    expect((await view(app)).pendingRequests).toEqual([1]);
    await mock.releaseAudio(path);
    expect(await mock.eval("() => window.__audioReply")).toMatchObject({ status: "applied", request: 1, revision: 1, createdIds: [], warnings: [] });
    expect((await view(app)).audioAssets).toEqual([{ ...provenance, sample: { path: "generated.wav", file: null } }]);

    // engine/src/waveform.rs::encode echoes the identity and partitions [start, end) by integer division.
    const bytes = (args: typeof request) => mock.eval<number[]>(`async () => Array.from(new Uint8Array(await window.__TAURI_INTERNALS__.invoke(
      "arrangement_waveform", ${JSON.stringify(args)})))`);
    const originalPacket = await bytes(request);
    const decoded = decodeWaveform(Uint8Array.from(originalPacket).buffer);
    expect(decoded).toMatchObject({ asset: 2, generation: BigInt(missing.generation), request: 777n, rate: 32000,
      frames: 8, start: 1, end: 7, bins: 2 });
    expect(Array.from(decoded.peaks)).toEqual([-0.5, 0.25, -0.25, 0.5, -0.25, 0.75, -0.75, 0.25]);

    await mock.holdWaveform(request.request);
    await mock.eval(`() => { window.__waveformReply = window.__TAURI_INTERNALS__.invoke(
      "arrangement_waveform", ${JSON.stringify(request)}).catch(String); }`);
    await mock.registerAudio(replacementPath, generatedWav({ rate: 48000, samples: [[100, -100], [200, -200]] }));
    expect(await call(app, "audio_relink", { ...stamp(await view(app)), asset: 2, path: replacementPath }))
      .toMatchObject({ status: "applied", request: 2, revision: 2, createdIds: [], warnings: [
        "Audio asset 2: decoded geometry differs from saved provenance; trim duration is preserved and unavailable decoded frames are silent.",
      ] });
    expect((await view(app)).audioAssets).toEqual([{ ...provenance, sample: { path: "replacement.wav", file: null } }]);
    await mock.releaseWaveform(request.request);
    expect(await mock.eval("() => window.__waveformReply")).toBe("Waveform source replaced; reload");
    expect(await refusal(app, "arrangement_waveform", request)).toBe("Invalid waveform range or bin count");

    await call(app, "arrangement_history", { ...stamp(await view(app)), undo: true });
    expect(await bytes(request)).toEqual(originalPacket);
    expect((await view(app)).audioAssets[0].sample.path).toBe("generated.wav");
    await call(app, "arrangement_history", { ...stamp(await view(app)), undo: true });
    const undone = await view(app);
    expect(undone.audioAssets).toEqual([provenance]);
    expect(undone.warnings).toEqual(missing.warnings);
    expect(await refusal(app, "arrangement_waveform", request)).toBe("Audio is missing; relink it");
  });
});
