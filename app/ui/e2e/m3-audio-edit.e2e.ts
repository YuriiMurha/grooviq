import type { Page } from "@playwright/test";
import type { ArrangementOperation, ArrangementPage, ArrangementStamp, AudioContent } from "../src/api/arrangement";
import { expect, generatedWav, project, soundsFor, test, type Mock } from "./fixture";

const sourceRate = 8000;
const sourceFrames = 16000;
const audio: AudioContent = {
  type: "audio", asset: 4, sourceStart: 4000, sourceEnd: 12000,
  gain: 0.75, reverse: false, fadeIn: 100, fadeOut: 200,
};
const reversed = { ...audio, reverse: true };
const clips = [
  { id: 5, track: 2, name: "Break", startTick: 960, muted: false, content: audio },
  { id: 6, track: 2, name: "Reverse", startTick: 3840, muted: false, content: reversed },
  { id: 7, track: 3, name: "Beat", startTick: 960, muted: false,
    content: { type: "pattern" as const, pattern: 0, offsetTick: 0, lengthTicks: 1920 } },
];

// Both browser projects use the same seed. Audio edits require the #733 fixture handler.
test.use({ deviceScaleFactor: 1, seed: {
  project: project(["Kick"], {}, {
    nextId: 8,
    arrangement: {
      tracks: [{ id: 2, name: "Audio", color: 1, mute: false }, { id: 3, name: "Beat", color: 0, mute: false }],
      clips,
      audioAssets: [{ id: 4, sourceRate, sourceFrames, sample: { path: "break.wav", file: "samples/break.wav" },
        originalTempo: null, originalKey: null }],
      loopRegion: null,
    },
    arrangementMedia: { 4: generatedWav({ rate: sourceRate, samples: Array<number>(sourceFrames).fill(0) }) },
  }),
  files: soundsFor(["Kick"]),
  audioLog: [],
} });

const arrangement = (page: Page): Promise<ArrangementPage> => page.evaluate(() =>
  (window as unknown as { __TAURI_INTERNALS__: { invoke: (command: string, args: unknown) => Promise<ArrangementPage> } })
    .__TAURI_INTERNALS__.invoke("arrangement_view", { offset: 0, limit: 256 }));

const call = ({ generation, revision }: ArrangementStamp, operations: ArrangementOperation[]) =>
  ({ cmd: "arrangement_edit", args: { generation, revision, edits: operations } });
type EditCall = ReturnType<typeof call>;

async function editCalls(mock: Mock) {
  return (await mock.calls("arrangement_edit")).map(({ cmd, args: { editsJson, ...stamp } }) => ({
    cmd, args: { ...stamp, edits: JSON.parse(String(editsJson)) as unknown },
  }));
}

async function setup(app: Page, mock: Mock) {
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await app.keyboard.press("F5");
  const playlist = app.getByRole("region", { name: "Playlist", exact: true });
  const canvas = playlist.getByRole("application", { name: "Playlist clips" });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await expect(playlist.getByRole("status")).toHaveText("3 clips");
  const box = await canvas.boundingBox();
  if (!box) throw new Error("Missing playlist canvas");
  const expected: EditCall[] = [];
  const select = async (name: string) => {
    await playlist.getByRole("button", { name: /^Selected playlist clip:/ }).click();
    await playlist.getByRole("listbox", { name: "Selected playlist clip" }).getByRole("option", { name, exact: true }).click();
  };
  const drag = async (x: number, y: number, endX: number, endY = y, alt = false) => {
    if (alt) await app.keyboard.down("Alt");
    await app.mouse.move(box.x + x, box.y + y);
    await app.mouse.down();
    await app.mouse.move(box.x + endX, box.y + endY, { steps: 4 });
    expect(await editCalls(mock)).toEqual(expected);
    await app.mouse.up();
    if (alt) await app.keyboard.up("Alt");
  };
  const edit = async (operations: ArrangementOperation[], act: () => Promise<unknown>) => {
    await expect(canvas).toHaveAttribute("aria-disabled", "false");
    const before = await arrangement(app);
    await act();
    expected.push(call(before, operations));
    await expect.poll(() => editCalls(mock)).toEqual(expected);
    await expect.poll(async () => (await arrangement(app)).revision).toBeGreaterThan(before.revision);
    await expect(canvas).toHaveAttribute("aria-disabled", "false");
    return before;
  };
  const undo = async (before: ArrangementPage) => {
    const { generation, revision } = await arrangement(app);
    // The mock stamps each call with its time; compare only the command and its arguments.
    const historyCalls = async () => (await mock.calls("arrangement_history")).map(({ cmd, args }) => ({ cmd, args }));
    const history = await historyCalls();
    await app.keyboard.press("ControlOrMeta+z");
    await expect.poll(async () => (await arrangement(app)).clips).toEqual(before.clips);
    await expect.poll(historyCalls).toEqual([
      ...history, { cmd: "arrangement_history", args: { generation, revision, undo: true } },
    ]);
    await expect(canvas).toHaveAttribute("aria-disabled", "false");
    expect(await editCalls(mock)).toEqual(expected);
  };
  return { playlist, box, canvas, select, drag, edit, undo, expected };
}

test.afterEach(async ({ mock }) => {
  expect((await mock.calls()).filter(({ cmd, args }) =>
    cmd === "transport_play" || cmd === "synth_note" ||
    (["channel_note", "test_tone", "channel_preview"].includes(cmd) && args.on === true))).toEqual([]);
});

test("trims the left edge and the reversed right edge, then slices at the grid", async ({ app, mock }) => {
  const { playlist, box, select, drag, edit, undo } = await setup(app, mock);
  await select("Break · Track 1 · 960 ticks");
  const leftContent = { ...audio, sourceStart: 8000 };
  const beforeLeft = await edit([{ kind: "resize", clip: 5, startTick: 1920, content: leftContent }],
    () => drag(50, 42, 98));
  expect((await arrangement(app)).clips[0]).toMatchObject({ startTick: 1920, content: leftContent });
  await undo(beforeLeft);

  await select("Reverse · Track 1 · 3840 ticks");
  const rightContent = { ...reversed, sourceStart: 8000 };
  const beforeRight = await edit([{ kind: "resize", clip: 6, startTick: 3840, content: rightContent }],
    () => drag(286, 42, 238));
  expect((await arrangement(app)).clips[1]).toMatchObject({ startTick: 3840, content: rightContent });
  await undo(beforeRight);

  await playlist.getByRole("button", { name: "Slice", exact: true }).click();
  const beforeSlice = await edit([{ kind: "slice", clip: 6, tick: 4800, audioFirstFrames: 4000 }],
    () => app.mouse.click(box.x + 240, box.y + 42));
  const fragments = (await arrangement(app)).clips.filter(clip => clip.name === "Reverse");
  expect(fragments).toMatchObject([
    { id: 6, startTick: 3840, content: { ...reversed, sourceStart: 8000, fadeOut: 0 } },
    { startTick: 4800, content: { ...reversed, sourceEnd: 8000, fadeIn: 0 } },
  ]);
  await undo(beforeSlice);
});

test("moves and Alt-copies audio, then saves and reopens a trimmed clip", async ({ app, mock }) => {
  const { select, drag, edit, undo, expected } = await setup(app, mock);
  const beforeMove = await edit([{ kind: "move", clip: 5, track: 3, startTick: 1920 }],
    () => drag(90, 42, 138, 106));
  expect((await arrangement(app)).clips[0]).toMatchObject({ track: 3, startTick: 1920, content: audio });
  await undo(beforeMove);
  const beforeCopy = await edit([{ kind: "copy", clip: 5, track: 2, startTick: 1920 }],
    () => drag(90, 42, 138, 42, true));
  expect((await arrangement(app)).clips.at(-1)).toMatchObject({ track: 2, startTick: 1920, content: audio });
  await undo(beforeCopy);

  await select("Break · Track 1 · 960 ticks");
  await edit([{ kind: "resize", clip: 5, startTick: 1920, content: { ...audio, sourceStart: 8000 } }],
    () => drag(50, 42, 98));
  const saved = await arrangement(app);
  const path = "/qa/projects/Edited break.grooviq";
  await mock.eval(`m => m.queueDialog(${JSON.stringify(path)}, ${JSON.stringify(path)})`);
  await app.keyboard.press("ControlOrMeta+s");
  await expect.poll(async () => (await mock.view()).path).toBe(path);
  await app.keyboard.press("ControlOrMeta+o");
  await expect.poll(async () => (await mock.view()).generation).toBe(saved.generation + 1);
  await expect.poll(async () => (await arrangement(app)).clips).toEqual(saved.clips);
  expect(await editCalls(mock)).toEqual(expected);
});

test("edits a mixed selection in playlist order with one undo per batch", async ({ app, mock }) => {
  const { playlist, drag, edit, undo } = await setup(app, mock);
  await playlist.getByRole("button", { name: "Select", exact: true }).click();
  const selectGroup = async () => {
    await drag(5, 8, 150, 126);
    await expect(playlist.getByText("2 selected", { exact: true })).toBeVisible();
  };
  await selectGroup();
  const beforeMove = await edit([{ kind: "move", clip: 5, track: 2, startTick: 1920 },
    { kind: "move", clip: 7, track: 3, startTick: 1920 }], () => drag(90, 42, 138));
  expect((await arrangement(app)).clips.map(clip => clip.startTick)).toEqual([1920, 3840, 1920]);
  await undo(beforeMove);

  await selectGroup();
  const beforeCopy = await edit([{ kind: "copy", clip: 5, track: 2, startTick: 1920 },
    { kind: "copy", clip: 7, track: 3, startTick: 1920 }], () => drag(90, 42, 138, 42, true));
  expect((await arrangement(app)).clips.slice(-2).map(clip => clip.content)).toEqual([audio, clips[2].content]);
  await undo(beforeCopy);

  await selectGroup();
  const beforeMute = await edit([{ kind: "setClip", clip: 5, name: "Break", muted: true },
    { kind: "setClip", clip: 7, name: "Beat", muted: true }],
  () => playlist.getByRole("button", { name: "Mute selected clips" }).click());
  expect((await arrangement(app)).clips.map(clip => clip.muted)).toEqual([true, false, true]);
  await undo(beforeMute);

  await selectGroup();
  const beforeDuplicate = await edit([{ kind: "copy", clip: 5, track: 2, startTick: 2880 },
    { kind: "copy", clip: 7, track: 3, startTick: 2880 }],
  () => playlist.getByRole("button", { name: "Duplicate clips" }).click());
  expect((await arrangement(app)).clips.slice(-2).map(clip => clip.startTick)).toEqual([2880, 2880]);
  await undo(beforeDuplicate);

  await selectGroup();
  const beforeDelete = await edit([{ kind: "deleteClip", clip: 5 }, { kind: "deleteClip", clip: 7 }],
    () => app.keyboard.press("Delete"));
  expect((await arrangement(app)).clips.map(clip => clip.id)).toEqual([6]);
  await undo(beforeDelete);
});
