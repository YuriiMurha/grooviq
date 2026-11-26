import type { Locator, Page } from "@playwright/test";
import type { ProjectView } from "../src/api/project";
import type { ArrangementPage } from "../src/api/arrangement";
import { expect, generatedWav, pixelsAt, project, soundsFor, test, type Mock } from "./fixture";

const audio = "/qa/audio/Break.Take.2.wav";
const saved = "/qa/projects/Finder take.grooviq";
const sourceRate = 8000;
const sourceFrames = 1600;
const bytes = generatedWav({ rate: sourceRate, samples: Array.from({ length: sourceFrames }, () => [-8192, 8192] as const) });
const tracks = [{ id: 2, name: "Audio A", color: 1, mute: false }, { id: 3, name: "Audio B", color: 2, mute: false }];
const scene = project(["Kick"], {}, { nextId: 4, arrangement: { tracks, clips: [], audioAssets: [], loopRegion: null } });
const selectName = "Selected playlist clip";
const clipPicker = (playlist: Locator) => playlist.getByRole("button", { name: /^Selected playlist clip:/ });
const clipOption = (playlist: Locator, name: string) => playlist.getByRole("listbox", { name: selectName }).getByRole("option", { name, exact: true });

async function expectClipOption(playlist: Locator, name: string) {
  await expect(playlist.getByRole("status")).toHaveText("1 clips");
  await clipPicker(playlist).click();
  await expect(clipOption(playlist, name)).toBeVisible();
  await playlist.page().keyboard.press("Escape");
}

test.use({ deviceScaleFactor: 1, seed: { project: scene, files: soundsFor(["Kick"]), audioLog: [] } });

function ipc<T>(app: Page, command: string, args: Record<string, unknown> = {}): Promise<T> {
  return app.evaluate(({ command, args }) => {
    const shell = window as unknown as { __TAURI_INTERNALS__: { invoke(cmd: string, args: Record<string, unknown>): Promise<unknown> } };
    return shell.__TAURI_INTERNALS__.invoke(command, args);
  }, { command, args }) as Promise<T>;
}

const arrangement = (app: Page) => ipc<ArrangementPage>(app, "arrangement_view", { offset: 0, limit: 256 });
const call = (cmd: string, args: Record<string, unknown> = {}) => ({ cmd, args });
const waveform = (generation: number, asset: number, request: number) =>
  call("arrangement_waveform", { generation, asset, request, start: 0, end: sourceFrames, bins: 13 });

const playback = (cmd: string, args: Record<string, unknown>) =>
  cmd === "transport_play" || cmd === "synth_note" ||
  (["channel_note", "test_tone", "channel_preview"].includes(cmd) && args.on === true);
const editCommands = ["audio_import", "audio_relink", "audio_cancel", "audio_cancel_ticket", "arrangement_edit",
  "arrangement_waveform", "arrangement_history", "channel_add", "project_save", "project_save_as", "project_open", "project_new"];

// Native refuses an import or relink without a ticket (#629), and the UI picks a fresh random
// one per call: require its range, then compare every other argument exactly.
function withoutTicket(cmd: string, args: Record<string, unknown>) {
  if (cmd !== "audio_import" && cmd !== "audio_relink") return args;
  const { ticket, ...rest } = args;
  expect(Number.isSafeInteger(ticket) && (ticket as number) > 0).toBe(true);
  return rest;
}

async function edits(mock: Mock) {
  return (await mock.calls()).filter(({ cmd }) => editCommands.includes(cmd)).map(({ cmd, args }) => ({ cmd, args: withoutTicket(cmd, args) }));
}

async function displayedFrames(app: Page) {
  await app.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}

test.afterEach(async ({ app, mock }) => {
  await displayedFrames(app);
  expect((await mock.calls()).filter(({ cmd, args }) => playback(cmd, args))).toEqual([]);
});

async function openPlaylist(app: Page, mock: Mock) {
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await app.keyboard.press("F5");
  const playlist = app.getByRole("region", { name: "Playlist", exact: true });
  const canvas = playlist.getByRole("application", { name: "Playlist clips" });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  // The rack and playlist register separate webview listeners; both must receive the drop.
  await expect.poll(async () => (await mock.calls("plugin:event|listen")).filter(({ args }) =>
    args.event === "tauri://drag-leave").length).toBe(2);
  const box = await canvas.boundingBox();
  if (!box || box.width < 320 || box.height < 128) throw new Error("Playlist tracks are not within the canvas");
  return { playlist, canvas, box, point: { x: box.x + 60, y: box.y + 32 } };
}

const importCall = (stamp: { generation: number; revision: number }, path = audio, track = 2, startTick = 960, name = "Break.Take.2") =>
  call("audio_import", { ...stamp, path, track, startTick, name });

async function emitDrop(mock: Mock, paths: string[], position: { x: number; y: number }) {
  const drop = JSON.stringify({ paths, position });
  await mock.eval(`m => {
    m.emit("tauri://drag-enter", ${drop});
    m.emit("tauri://drag-over", ${JSON.stringify({ position })});
    m.emit("tauri://drag-drop", ${drop});
  }`);
}

const dropMessages = (app: Page) => app.getByRole("status", { name: "Messages" });
const importArgs = async (mock: Mock) => (await mock.calls("audio_import")).map(({ args }) => withoutTicket("audio_import", args));

test("drops one snapped edit, renders a waveform, and saves and reopens it", async ({ app, mock }) => {
  const { playlist, canvas, point } = await openPlaylist(app, mock);
  const { generation, revision } = await arrangement(app);
  const background = await pixelsAt(app, canvas, [{ x: 56, y: 42 }]);
  const dpr = await app.evaluate(() => devicePixelRatio);
  const position = { x: point.x * dpr, y: point.y * dpr };
  await mock.eval(`m => {
    m.emit("tauri://drag-enter", { paths: [${JSON.stringify(audio)}], position: { x: 1, y: 1 } });
    m.emit("tauri://drag-over", { position: ${JSON.stringify(position)} });
  }`);
  await expect.poll(() => pixelsAt(app, canvas, [{ x: 56, y: 42 }])).not.toEqual(background);
  expect(await edits(mock)).toEqual([]);
  await mock.eval('m => m.emit("tauri://drag-leave", null)');
  await expect.poll(() => pixelsAt(app, canvas, [{ x: 56, y: 42 }])).toEqual(background);
  await mock.dropAudio(audio, point, bytes);
  await expect(clipPicker(playlist)).toHaveAccessibleName("Selected playlist clip: Break.Take.2 · Track 1 · 960 ticks");
  await expect.poll(() => edits(mock)).toEqual([importCall({ generation, revision }), waveform(generation, 4, 1)]);
  await expect.poll(() => pixelsAt(app, canvas, [{ x: 56, y: 42 }])).not.toEqual(background);
  const peakPixels = await pixelsAt(app, canvas, [{ x: 56, y: 39 }, { x: 56, y: 43 }]);
  expect(peakPixels[0]).not.toEqual(peakPixels[1]);
  await mock.eval(`m => m.queueDialog(${JSON.stringify(saved)}, ${JSON.stringify(saved)})`);
  await app.keyboard.press("ControlOrMeta+s");
  await expect.poll(async () => (await mock.view()).path).toBe(saved);
  await app.keyboard.press("ControlOrMeta+o");
  await expect.poll(async () => (await mock.view()).generation).toBe(generation + 1);
  await expectClipOption(playlist, "Break.Take.2 · Track 1 · 960 ticks");
  const reopened = await arrangement(app);
  expect(reopened.audioAssets[0]).toMatchObject({ id: 4, sample: { file: "samples/2-Break.Take.2.wav" } });
  expect(reopened.warnings).toEqual([]);
  await displayedFrames(app);
  expect(await edits(mock)).toEqual([
    importCall({ generation, revision }), waveform(generation, 4, 1),
    call("project_save_as", { path: saved }), call("project_open", { path: saved }), waveform(generation + 1, 4, 2),
  ]);
});

test("undo removes the clip and its asset in one step", async ({ app, mock }) => {
  const { playlist, point } = await openPlaylist(app, mock);
  const { generation, revision } = await arrangement(app);
  await mock.dropAudio(audio, point, bytes);
  await expect(playlist.getByRole("status")).toHaveText("1 clips");
  await expect.poll(() => edits(mock)).toEqual([importCall({ generation, revision }), waveform(generation, 4, 1)]);
  const imported = await arrangement(app);
  await app.keyboard.press("ControlOrMeta+z");
  await expect(playlist.getByRole("status")).toHaveText("0 clips");
  expect(await arrangement(app)).toMatchObject({ generation, clips: [], audioAssets: [] });
  expect(await ipc<ProjectView>(app, "project_view")).toMatchObject({ projectCanUndo: false, projectCanRedo: true });
  await displayedFrames(app);
  expect(await edits(mock)).toEqual([importCall({ generation, revision }), waveform(generation, 4, 1),
    call("arrangement_history", { generation, revision: imported.revision, undo: true })]);
});

test("a multi-file drop imports only the first file and reports skipped files", async ({ app, mock }) => {
  const { playlist, point } = await openPlaylist(app, mock);
  const { generation, revision } = await arrangement(app);
  const other = "/qa/audio/Other.wav";
  await mock.registerAudio(audio, bytes);
  await mock.registerAudio(other, bytes);
  await expect(dropMessages(app)).toBeEmpty();
  await emitDrop(mock, [audio, other], point);
  await expectClipOption(playlist, "Break.Take.2 · Track 1 · 960 ticks");
  await expect(dropMessages(app)).not.toBeEmpty();
  await displayedFrames(app);
  expect(await importArgs(mock)).toEqual([importCall({ generation, revision }).args]);
});

test("a refused multi-file drop still reports the skipped files", async ({ app, mock }) => {
  const { playlist, point } = await openPlaylist(app, mock);
  const { generation, revision } = await arrangement(app);
  const other = "/qa/audio/Other.wav";
  await mock.registerAudio(audio, bytes);
  await mock.registerAudio(other, bytes);
  await mock.holdAudio(audio);
  await emitDrop(mock, [audio, other], point);
  await expect.poll(async () => (await arrangement(app)).pendingRequests.length).toBe(1);
  await expect(dropMessages(app)).toBeEmpty();
  await mock.refuseAudio(audio);
  await expect(playlist.getByRole("alert")).toContainText(/decode|sound/i);
  await expect(dropMessages(app)).not.toBeEmpty();
  await displayedFrames(app);
  expect(await importArgs(mock)).toEqual([importCall({ generation, revision }).args]);
});

test("reports a refused playlist drop while an import is held", async ({ app, mock }) => {
  const { playlist, point } = await openPlaylist(app, mock);
  const { generation, revision } = await arrangement(app);
  await mock.holdAudio(audio);
  await mock.dropAudio(audio, point, bytes);
  await expect.poll(async () => (await arrangement(app)).pendingRequests.length).toBe(1);
  await expect(dropMessages(app)).toBeEmpty();
  await mock.dropAudio("/qa/audio/Other.wav", point, bytes);
  await expect(dropMessages(app)).not.toBeEmpty();
  expect(await importArgs(mock)).toEqual([importCall({ generation, revision }).args]);
  await mock.releaseAudio(audio);
  await expectClipOption(playlist, "Break.Take.2 · Track 1 · 960 ticks");
});

test("discarding into New retires a held import before the decode finishes", async ({ app, mock }) => {
  const { playlist, point } = await openPlaylist(app, mock);
  const { generation, revision } = await arrangement(app);
  await mock.holdAudio(audio);
  await mock.dropAudio(audio, point, bytes);
  await expect(playlist.getByRole("status")).toHaveText("Importing audio…");
  await expect.poll(async () => (await arrangement(app)).pendingRequests.length).toBe(1);
  await mock.dropAudio(audio, point, bytes);
  await app.keyboard.press("ControlOrMeta+n");
  await app.getByRole("dialog", { name: "Save changes?" }).getByRole("button", { name: "Don't save" }).click();
  await expect.poll(async () => (await mock.view()).generation).toBe(generation + 1);
  await mock.releaseAudio(audio);
  await expect(playlist.getByRole("status")).toHaveText("0 clips");
  await displayedFrames(app);
  expect(await arrangement(app)).toMatchObject({ generation: generation + 1, clips: [], audioAssets: [], pendingRequests: [] });
  expect(await ipc<ProjectView>(app, "project_view")).toMatchObject({ dirty: false, projectCanUndo: false, projectCanRedo: false });
  expect(await edits(mock)).toEqual([importCall({ generation, revision }), call("project_new")]);
});

test("a multi-file drop retired by New reports nothing about skipped files", async ({ app, mock }) => {
  const { playlist, point } = await openPlaylist(app, mock);
  const { generation, revision } = await arrangement(app);
  const other = "/qa/audio/Other.wav";
  await mock.registerAudio(audio, bytes);
  await mock.registerAudio(other, bytes);
  await mock.holdAudio(audio);
  await emitDrop(mock, [audio, other], point);
  await expect.poll(async () => (await arrangement(app)).pendingRequests.length).toBe(1);
  await app.keyboard.press("ControlOrMeta+n");
  await app.getByRole("dialog", { name: "Save changes?" }).getByRole("button", { name: "Don't save" }).click();
  await expect.poll(async () => (await mock.view()).generation).toBe(generation + 1);
  await mock.releaseAudio(audio);
  await expect(playlist.getByRole("status")).toHaveText("0 clips");
  await displayedFrames(app);
  await expect(dropMessages(app)).toBeEmpty();
  expect(await edits(mock)).toEqual([importCall({ generation, revision }), call("project_new")]);
});

test("a refused decode preserves the project, history and selection", async ({ app, mock }) => {
  const { playlist, point } = await openPlaylist(app, mock);
  const before = await arrangement(app);
  const view = await ipc<ProjectView>(app, "project_view");
  await mock.holdAudio(audio);
  await mock.dropAudio(audio, point, bytes);
  await expect.poll(async () => (await arrangement(app)).pendingRequests.length).toBe(1);
  await mock.refuseAudio(audio);
  await expect(playlist.getByRole("alert")).toContainText(/decode|sound/i);
  await expect(playlist.getByRole("status")).toHaveText("0 clips");
  expect(await arrangement(app)).toEqual(before);
  expect(await ipc<ProjectView>(app, "project_view")).toEqual(view);
  await expect(clipPicker(playlist)).toHaveAccessibleName("Selected playlist clip: None");
  await displayedFrames(app);
  expect(await edits(mock)).toEqual([importCall({ generation: before.generation, revision: before.revision })]);
});

const asset = { id: 4, sample: { path: "stale.wav", file: "samples/1-stale.wav" },
  sourceRate, sourceFrames, originalTempo: null, originalKey: null };
const clip = { id: 5, track: 2, name: "Stored audio", startTick: 960, muted: false,
  content: { type: "audio" as const, asset: 4, sourceStart: 0, sourceEnd: sourceFrames, gain: 1, reverse: false, fadeIn: 0, fadeOut: 0 } };
const storedArrangement = { tracks, clips: [clip], audioAssets: [asset], loopRegion: null };

test("relinking to the same path refetches the cached waveform", async ({ app, mock }) => {
  const { playlist, point } = await openPlaylist(app, mock);
  const { generation, revision } = await arrangement(app);
  await mock.dropAudio(audio, point, bytes);
  await expect.poll(() => edits(mock)).toEqual([importCall({ generation, revision }), waveform(generation, 4, 1)]);
  const before = await arrangement(app);
  // Report the existing source as missing for one refresh, keeping its sample reference
  // and the previously loaded waveform bucket so that relink must invalidate the cache.
  await app.evaluate(() => {
    const shell = (window as unknown as { __TAURI_INTERNALS__: {
      invoke: (command: string, args: Record<string, unknown>) => Promise<unknown>;
    } }).__TAURI_INTERNALS__;
    const invoke = shell.invoke;
    shell.invoke = async (command, args) => {
      if (command !== "arrangement_view") return invoke(command, args);
      shell.invoke = invoke;
      const page = await invoke(command, args) as ArrangementPage;
      return { ...page, warnings: ["Audio asset 4 is missing; relink it to hear its clips."] };
    };
  });
  await playlist.getByRole("button", { name: "Refresh playlist" }).click();
  const relink = playlist.getByRole("button", { name: "Relink missing audio" });
  await expect(relink).toBeEnabled();
  await mock.registerAudio(audio, generatedWav({ rate: sourceRate, samples: Array.from({ length: sourceFrames }, () => [0, 0] as const) }));
  await mock.eval(`m => m.queueDialog(${JSON.stringify(audio)})`);
  await relink.click();
  await expect(relink).toHaveCount(0);
  expect((await arrangement(app)).audioAssets).toEqual(before.audioAssets);
  await expect.poll(() => edits(mock)).toEqual([
    importCall({ generation, revision }), waveform(generation, 4, 1),
    call("audio_relink", { generation, revision: before.revision, asset: 4, path: audio }), waveform(generation, 4, 2),
  ]);
});

test.describe("a missing audio asset", () => {
  test.use({ seed: { project: { ...scene, nextId: 6, arrangement: storedArrangement }, files: soundsFor(["Kick"]) } });

  test("relinks the selected asset and fetches its waveform", async ({ app, mock }) => {
    const { playlist } = await openPlaylist(app, mock);
    const before = await arrangement(app);
    expect(before.warnings).toEqual(["Audio asset 4 is missing; relink it to hear its clips."]);
    await clipPicker(playlist).click();
    await clipOption(playlist, "Stored audio · Track 1 · 960 ticks").click();
    await mock.registerAudio(audio, bytes);
    await mock.eval(`m => m.queueDialog(${JSON.stringify(audio)})`);
    const relink = playlist.getByRole("button", { name: "Relink missing audio" });
    await relink.click();
    await expect(relink).toHaveCount(0);
    await expect.poll(() => edits(mock)).toEqual([
      call("audio_relink", { generation: before.generation, revision: before.revision, asset: 4, path: audio }),
      waveform(before.generation, 4, 1),
    ]);
    expect(await arrangement(app)).toMatchObject({ warnings: [], audioAssets: [{ ...asset, sample: { path: "Break.Take.2.wav", file: null } }] });
    await displayedFrames(app);
    expect(await edits(mock)).toEqual([
      call("audio_relink", { generation: before.generation, revision: before.revision, asset: 4, path: audio }),
      waveform(before.generation, 4, 1),
    ]);
  });
});

test.describe("waveform source changes during an in-flight request", () => {
  test.use({ seed: { project: { ...scene, nextId: 6, arrangement: storedArrangement, arrangementMedia: { 4: bytes } }, files: soundsFor(["Kick"]) } });

  test("retries the visible bucket after the native source-replaced response", async ({ app, mock }) => {
    await mock.holdWaveform(1);
    const before = await arrangement(app);
    await openPlaylist(app, mock);
    await expect.poll(() => edits(mock)).toEqual([waveform(before.generation, 4, 1)]);
    const replacement = "/qa/new/stale.wav";
    await mock.registerAudio(replacement, generatedWav({ rate: sourceRate, samples: Array.from({ length: sourceFrames }, () => [0, 0] as const) }));
    const args = { generation: before.generation, revision: before.revision, asset: 4, path: replacement };
    expect(await ipc(app, "audio_relink", { ...args, ticket: 1 })).toMatchObject({ status: "applied" });
    await mock.releaseWaveform(1);
    await expect.poll(() => edits(mock)).toEqual([
      waveform(before.generation, 4, 1), call("audio_relink", args), waveform(before.generation, 4, 2),
    ]);
    await displayedFrames(app);
    expect(await edits(mock)).toEqual([
      waveform(before.generation, 4, 1), call("audio_relink", args), waveform(before.generation, 4, 2),
    ]);
  });
});

test.describe("retina input", () => {
  test.use({ deviceScaleFactor: 2 });

  for (const { platform, scale } of [{ platform: "MacIntel", scale: 1 }, { platform: "Win32", scale: 2 }]) {
    test.describe(platform, () => {
      test.beforeEach(async ({ page }) => {
        await page.addInitScript(value => Object.defineProperty(navigator, "platform", { value }), platform);
      });

      test("places the clip on the second track under a DPR 2 pointer", async ({ app, mock }) => {
        const { box, playlist } = await openPlaylist(app, mock);
        const { generation, revision } = await arrangement(app);
        expect(await app.evaluate(() => [navigator.platform, window.devicePixelRatio])).toEqual([platform, 2]);
        await mock.registerAudio(audio, bytes);
        await emitDrop(mock, [audio], { x: (box.x + 108) * scale, y: (box.y + 104) * scale });
        await expectClipOption(playlist, "Break.Take.2 · Track 2 · 1920 ticks");
        await displayedFrames(app);
        expect(await importArgs(mock)).toEqual([importCall({ generation, revision }, audio, 3, 1920).args]);
      });
    });
  }
});

test("a drop outside the playlist goes to the rack", async ({ app, mock }) => {
  await openPlaylist(app, mock);
  const rack = app.getByRole("region", { name: "Channel rack" });
  const box = await rack.boundingBox();
  if (!box) throw new Error("Channel rack is missing");
  const { generation } = await mock.view();
  await mock.dropAudio(audio, { x: box.x + 12, y: box.y + 24 }, bytes);
  await expect(rack.getByRole("group", { name: "Break.Take.2" })).toBeVisible();
  expect(await arrangement(app)).toMatchObject({ clips: [], audioAssets: [] });
  await displayedFrames(app);
  expect(await edits(mock)).toEqual([call("channel_add", { generation, path: audio })]);
});
