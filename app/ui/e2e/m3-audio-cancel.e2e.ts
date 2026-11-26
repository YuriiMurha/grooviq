import type { Page } from "@playwright/test";
import type { ArrangementPage } from "../src/api/arrangement";
import type { ProjectView } from "../src/api/project";
import { expect, generatedWav, project, soundsFor, test, type Mock } from "./fixture";

const audio = "/qa/audio/Break.wav";
const track = { id: 2, name: "Audio", color: 1, mute: false };
const scene = project(["Kick"], {}, { nextId: 4,
  arrangement: { tracks: [track], clips: [], audioAssets: [], loopRegion: null } });

test.use({ deviceScaleFactor: 1, seed: { project: scene, files: soundsFor(["Kick"]) } });

type Shell = { invoke(command: string, args?: Record<string, unknown>): Promise<unknown> };
type TestWindow = Window & { __TAURI_INTERNALS__: Shell; audioCancelReplies: boolean[] };

function ipc<T>(app: Page, command: string, args: Record<string, unknown> = {}): Promise<T> {
  return app.evaluate(({ command, args }) =>
    (window as unknown as TestWindow).__TAURI_INTERNALS__.invoke(command, args), { command, args }) as Promise<T>;
}

// Pause delivery after the real mock has published, leaving a user-clickable Cancel control.
function observeCancellation(app: Page, holdImportReply = false) {
  return app.evaluate(holdReply => {
    const w = window as unknown as TestWindow;
    const shell = w.__TAURI_INTERNALS__;
    const invoke = shell.invoke;
    const importReply = holdReply
      ? new Promise<void>(resolve => w.addEventListener("release-audio-import-reply", () => resolve(), { once: true }))
      : undefined;
    w.audioCancelReplies = [];
    shell.invoke = async (command, args) => {
      const reply = await invoke(command, args);
      if (command === "audio_import" && importReply) await importReply;
      if (command === "audio_cancel_ticket") w.audioCancelReplies.push(reply as boolean);
      return reply;
    };
  }, holdImportReply);
}

const cancellationReplies = (app: Page) => app.evaluate(() => (window as unknown as TestWindow).audioCancelReplies);
const arrangement = (app: Page) => ipc<ArrangementPage>(app, "arrangement_view", { offset: 0, limit: 256 });
const projectView = (app: Page) => ipc<ProjectView>(app, "project_view");
const call = (cmd: string, args: Record<string, unknown>) => ({ cmd, args });
const mediaCommands = ["audio_import", "audio_relink", "audio_cancel", "audio_cancel_ticket", "arrangement_edit", "arrangement_history", "channel_add"];
const changes = async (mock: Mock) => (await mock.calls())
  .filter(({ cmd }) => mediaCommands.includes(cmd))
  .map(({ cmd, args }) => call(cmd, args));

async function ticketFor(mock: Mock, command: "audio_import" | "audio_relink") {
  await expect.poll(async () => (await mock.calls(command)).length).toBe(1);
  const [{ args: { ticket } }] = await mock.calls(command);
  expect(typeof ticket === "number" && Number.isSafeInteger(ticket) && ticket > 0).toBe(true);
  return ticket;
}

async function openPlaylist(app: Page, mock: Mock) {
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await app.keyboard.press("F5");
  const playlist = app.getByRole("region", { name: "Playlist", exact: true });
  const canvas = playlist.getByRole("application", { name: "Playlist clips" });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await expect.poll(async () => (await mock.calls("plugin:event|listen")).filter(({ args }) =>
    args.event === "tauri://drag-leave").length).toBe(2);
  const box = await canvas.boundingBox();
  if (!box) throw new Error("Playlist canvas is unavailable");
  return { playlist, canvas, point: { x: box.x + 60, y: box.y + 32 },
    cancelImport: playlist.getByRole("button", { name: "Cancel audio import", exact: true }),
    selection: playlist.getByRole("button", { name: /^Selected playlist clip:/ }) };
}

test.afterEach(async ({ mock }) => {
  expect((await mock.calls()).filter(({ cmd, args }) => cmd === "transport_play" || cmd === "synth_note" ||
    (["channel_note", "test_tone", "channel_preview"].includes(cmd) && args.on === true))).toEqual([]);
});

test("cancels while reading and leaves the arrangement, history and selection intact", async ({ app, mock }) => {
  const { playlist, canvas, point, cancelImport, selection } = await openPlaylist(app, mock);
  const before = await arrangement(app);
  const view = await projectView(app);
  const { generation, revision } = before;
  await observeCancellation(app);
  await mock.holdAudioRead(audio);
  await mock.dropAudio(audio, point);
  const ticket = await ticketFor(mock, "audio_import");
  await expect(selection).toBeDisabled();
  expect(await arrangement(app)).toEqual(before);
  await cancelImport.press("Enter");
  await expect.poll(() => cancellationReplies(app)).toEqual([true]);
  await expect(playlist).toHaveAttribute("aria-busy", "true");
  await mock.releaseAudioRead(audio);
  await expect(playlist).toHaveAttribute("aria-busy", "false");
  await expect(canvas).toBeFocused();
  await expect(playlist.getByRole("alert")).toHaveCount(0);
  await expect(selection).toBeEnabled();
  await expect(selection).toHaveAccessibleName("Selected playlist clip: None");
  expect(await arrangement(app)).toEqual(before);
  expect(await projectView(app)).toEqual(view);
  expect(await changes(mock)).toEqual([
    call("audio_import", { generation, revision, ticket, path: audio, track: 2, startTick: 960, name: "Break" }),
    call("audio_cancel_ticket", { ticket }),
  ]);
});

test("keeps an import that has already published and undoes it in one step", async ({ app, mock }) => {
  const { playlist, point, cancelImport, selection } = await openPlaylist(app, mock);
  const { generation, revision } = await arrangement(app);
  await observeCancellation(app, true);
  await mock.dropAudio(audio, point);
  const ticket = await ticketFor(mock, "audio_import");
  await expect.poll(async () => (await arrangement(app)).clips.length).toBe(1);
  const imported = await arrangement(app);
  await cancelImport.click();
  await expect.poll(() => cancellationReplies(app)).toEqual([false]);
  expect(await arrangement(app)).toEqual(imported);
  await app.evaluate(() => window.dispatchEvent(new Event("release-audio-import-reply")));
  await expect(selection).toHaveAccessibleName("Selected playlist clip: Break · Track 1 · 960 ticks");
  await expect(playlist.getByRole("alert")).toHaveCount(0);
  await app.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await arrangement(app)).clips).toEqual([]);
  expect(await arrangement(app)).toMatchObject({ audioAssets: [] });
  expect(await projectView(app)).toMatchObject({ projectCanUndo: false, projectCanRedo: true });
  expect(await changes(mock)).toEqual([
    call("audio_import", { generation, revision, ticket, path: audio, track: 2, startTick: 960, name: "Break" }),
    call("audio_cancel_ticket", { ticket }),
    call("arrangement_history", { generation, revision: imported.revision, undo: true }),
  ]);
});

test.describe("a missing asset", () => {
  const asset = { id: 4, sample: { path: "missing.wav", file: null }, sourceRate: 8000, sourceFrames: 1600, originalTempo: null, originalKey: null };
  const clip = { id: 5, track: 2, name: "Stored audio", startTick: 960, muted: false,
    content: { type: "audio" as const, asset: 4, sourceStart: 0, sourceEnd: 1600, gain: 1, reverse: false, fadeIn: 0, fadeOut: 0 } };
  test.use({ seed: { project: { ...scene, nextId: 6,
    arrangement: { ...scene.arrangement, tracks: [track], clips: [clip], audioAssets: [asset], loopRegion: null } }, files: soundsFor(["Kick"]) } });

  test("cancels a relink while its decoder is held", async ({ app, mock }) => {
    const { playlist, selection } = await openPlaylist(app, mock);
    const before = await arrangement(app);
    const view = await projectView(app);
    const { generation, revision } = before;
    await selection.click();
    await playlist.getByRole("listbox", { name: "Selected playlist clip" }).getByRole("option", { name: "Stored audio · Track 1 · 960 ticks", exact: true }).click();
    await observeCancellation(app);
    await mock.registerAudio(audio, generatedWav());
    await mock.holdAudio(audio);
    await mock.eval(`m => m.queueDialog(${JSON.stringify(audio)})`);
    const relink = playlist.getByRole("button", { name: "Relink missing audio" });
    await relink.click();
    const ticket = await ticketFor(mock, "audio_relink");
    await expect.poll(async () => (await arrangement(app)).pendingRequests.length).toBe(1);
    await playlist.getByRole("button", { name: "Cancel audio relink", exact: true }).click();
    await expect.poll(() => cancellationReplies(app)).toEqual([true]);
    await mock.releaseAudio(audio);
    await expect(relink).toBeEnabled();
    await expect(playlist.getByRole("alert")).toHaveCount(0);
    await expect(selection).toHaveAccessibleName("Selected playlist clip: Stored audio · Track 1 · 960 ticks");
    expect(await arrangement(app)).toEqual(before);
    expect(await projectView(app)).toEqual(view);
    expect(await changes(mock)).toEqual([
      call("audio_relink", { generation, revision, asset: 4, path: audio, ticket }),
      call("audio_cancel_ticket", { ticket }),
    ]);
  });
});
