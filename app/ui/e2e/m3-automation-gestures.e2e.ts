import type { Locator, Page } from "@playwright/test";
import type { ArrangementOperation, ArrangementPage, AutomationContent, PatternContent } from "../src/api/arrangement";
import { expect, project, soundsFor, test, type Mock } from "./fixture";

const curve = {
  type: "automation", target: { type: "channelVolume", channel: 1 }, priority: 8, offsetTick: 4800, lengthTicks: 3840,
  points: [
    { tick: 960, value: 0.5, mode: "linear" },
    { tick: 5280, value: 0.75, mode: "hold" },
    { tick: 6240, value: 1.25, mode: "linear" },
    { tick: 9600, value: 1, mode: "linear" },
  ],
} satisfies AutomationContent;
type Clip = { id: number; track: number; name: string; startTick: number; muted: boolean; content: AutomationContent | PatternContent };
const automation = { id: 4, track: 2, name: "Volume curve", startTick: 1920, muted: false, content: curve } satisfies Clip;
const pattern = { id: 5, track: 2, name: "Beat", startTick: 6720, muted: false,
  content: { type: "pattern", pattern: 0, offsetTick: 0, lengthTicks: 1920 },
} satisfies Clip;
const outside = { ...automation, id: 6, track: 3, name: "Other layer", startTick: 9600, content: { ...curve, priority: 9 } };
const originals = [automation, pattern, outside];

test.use({ seed: { project: project(["Kick"], {}, { nextId: 10, arrangement: {
  tracks: [{ id: 2, name: "Track 1", color: 0, mute: false }, { id: 3, name: "Track 2", color: 2, mute: false }],
  clips: originals, audioAssets: [], loopRegion: null,
} }), files: soundsFor(["Kick"]) } });

const clips = (mock: Mock) => mock.eval<Clip[]>("m => m.state.project.arrangement.clips");
const state = (page: Page): Promise<ArrangementPage> => page.evaluate(() =>
  (window as unknown as { __TAURI_INTERNALS__: { invoke: (command: string, args: unknown) => Promise<ArrangementPage> } })
    .__TAURI_INTERNALS__.invoke("arrangement_view", { offset: 0, limit: 256 }));

async function setup(app: Page) {
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await app.keyboard.press("F5");
  const playlist = app.getByRole("region", { name: "Playlist", exact: true });
  const canvas = playlist.getByRole("application", { name: "Playlist clips" });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await expect(playlist.getByRole("status")).toHaveText("3 clips");
  return { playlist, canvas };
}

async function at(canvas: Locator, x: number, y = 40) {
  const box = await canvas.boundingBox();
  if (!box) throw new Error("Missing playlist canvas");
  return [box.x + x, box.y + y] as const;
}

async function startDrag(app: Page, canvas: Locator, fromX: number, toX: number, toY = 40) {
  await app.mouse.move(...await at(canvas, fromX));
  await app.mouse.down();
  await app.mouse.move(...await at(canvas, toX, toY), { steps: 4 });
}

async function chooseAutomation(app: Page, canvas: Locator, playlist: Locator) {
  await app.mouse.click(...await at(canvas, 150));
  await expect(playlist.getByRole("group", { name: "Automation curve" })).toHaveAttribute("aria-disabled", "false");
}

async function expectEdit(mock: Mock, canvas: Locator, before: ArrangementPage, previousCalls: number,
  operations: ArrangementOperation[], expected: Clip[],
) {
  await expect.poll(() => clips(mock)).toEqual(expected);
  const sent = (await mock.calls("arrangement_edit")).slice(previousCalls);
  expect(sent).toHaveLength(1);
  expect(sent[0].args).toMatchObject({ generation: before.generation, revision: before.revision });
  expect(JSON.parse(String(sent[0].args.editsJson))).toEqual(operations);
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
}

async function undo(app: Page, mock: Mock, canvas: Locator, expected: Clip[]) {
  await app.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => clips(mock)).toEqual(expected);
  await expect.poll(async () => (await mock.view()).canUndo).toBe(false);
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
}

test("moves and Alt-copies an automation clip across tracks, with one Undo each", async ({ app, mock }) => {
  const { playlist, canvas } = await setup(app);
  const beforeMove = await state(app);
  await startDrag(app, canvas, 150, 198, 104);
  const label = playlist.locator(".playlist-labels").getByText("Volume curve · automation", { exact: true });
  await expect(label).toHaveCSS("left", "150px");
  await expect(label).toHaveCSS("top", "85px");
  expect(await clips(mock)).toEqual(originals);
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
  await app.mouse.up();
  await expectEdit(mock, canvas, beforeMove, 0, [{ kind: "move", clip: 4, track: 3, startTick: 2880 }],
    [{ ...automation, track: 3, startTick: 2880 }, pattern, outside]);
  await undo(app, mock, canvas, originals);

  const beforeCopy = await state(app);
  await app.keyboard.down("Alt");
  await startDrag(app, canvas, 150, 246, 104);
  await app.mouse.up();
  await app.keyboard.up("Alt");
  await expectEdit(mock, canvas, beforeCopy, 1, [{ kind: "copy", clip: 4, track: 3, startTick: 3840 }],
    [...originals, { ...automation, id: 10, track: 3, startTick: 3840, content: { ...curve, priority: 11 } }]);
  await expect(playlist.getByRole("button", { name: /^Selected playlist clip:/ })).toHaveAccessibleName(
    "Selected playlist clip: Volume curve · Track 2 · 3840 ticks",
  );
  await undo(app, mock, canvas, originals);
});

test("trims both edges with the full stamped source and undoes each trim", async ({ app, mock }) => {
  const { playlist, canvas } = await setup(app);
  await chooseAutomation(app, canvas, playlist);

  const label = playlist.locator(".playlist-labels").getByText("Volume curve · automation", { exact: true });
  const beforeLeft = await state(app);
  await startDrag(app, canvas, 98, 146);
  await expect(label).toHaveCSS("left", "150px");
  expect(await clips(mock)).toEqual(originals);
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
  await app.mouse.up();
  const left = { ...curve, offsetTick: 5760, lengthTicks: 2880 };
  await expectEdit(mock, canvas, beforeLeft, 0, [{ kind: "resize", clip: 4, startTick: 2880, content: left }],
    [{ ...automation, startTick: 2880, content: left }, pattern, outside]);
  await undo(app, mock, canvas, originals);

  await expect(playlist.getByRole("group", { name: "Automation curve" })).toHaveAttribute("aria-disabled", "false");
  const beforeRight = await state(app);
  await startDrag(app, canvas, 286, 238);
  await expect(label).toHaveCSS("max-width", "132px");
  await app.mouse.up();
  const right = { ...curve, lengthTicks: 2880 };
  await expectEdit(mock, canvas, beforeRight, 1, [{ kind: "resize", clip: 4, startTick: 1920, content: right }],
    [{ ...automation, content: right }, pattern, outside]);
  await undo(app, mock, canvas, originals);
});

test("waits for the complete curve before allowing a trim but can still slice", async ({ app, mock }) => {
  const { playlist, canvas } = await setup(app);
  await mock.eval("m => m.holdCommand('arrangement_curve')");
  await app.mouse.click(...await at(canvas, 150));
  await expect(playlist.getByRole("group", { name: "Automation curve" })).toHaveAttribute("aria-disabled", "true");
  await startDrag(app, canvas, 98, 146);
  await app.mouse.up();
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
  expect(await clips(mock)).toEqual(originals);

  const before = await state(app);
  await playlist.getByRole("button", { name: "Slice", exact: true }).click();
  await app.mouse.click(...await at(canvas, 192));
  await expectEdit(mock, canvas, before, 0, [{ kind: "slice", clip: 4, tick: 3840, audioFirstFrames: null }], split);
  await mock.eval("m => m.releaseCommand('arrangement_curve')");
  await expect(playlist.getByRole("group", { name: "Automation curve" })).toHaveAttribute("aria-disabled", "false");
});

const split = [
  { ...automation, content: { ...curve, lengthTicks: 1920 } },
  pattern,
  outside,
  { ...automation, id: 10, startTick: 3840, content: { ...curve, offsetTick: 6720, lengthTicks: 1920 } },
];

test("slices only inside, preserves linked fragments, and saves and reopens them", async ({ app, mock }) => {
  const { playlist, canvas } = await setup(app);
  await playlist.getByRole("button", { name: "Slice", exact: true }).click();
  const before = await state(app);
  await app.mouse.click(...await at(canvas, 98));
  await app.mouse.click(...await at(canvas, 286));
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
  expect(await clips(mock)).toEqual(originals);

  await app.mouse.move(...await at(canvas, 192));
  await app.mouse.down();
  await expect(playlist.locator(".playlist-slice-guide")).toHaveCSS("left", "192px");
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
  await app.mouse.up();
  const op = [{ kind: "slice", clip: 4, tick: 3840, audioFirstFrames: null }] satisfies ArrangementOperation[];
  await expectEdit(mock, canvas, before, 0, op, split);
  await expect(playlist.getByRole("button", { name: /^Selected playlist clip:/ })).toHaveAccessibleName(
    "Selected playlist clip: Volume curve · Track 1 · 1920 ticks",
  );
  await undo(app, mock, canvas, originals);
  await app.keyboard.press("ControlOrMeta+Shift+z");
  await expect.poll(() => clips(mock)).toEqual(split);
  await expect(canvas).toHaveAttribute("aria-disabled", "false");

  const path = "/qa/projects/automation-gestures.grooviq";
  await mock.eval(`m => m.queueDialog('${path}')`);
  await app.keyboard.press("ControlOrMeta+s");
  await expect.poll(async () => (await mock.view()).path).toBe(path);
  await expect.poll(async () => (await mock.view()).dirty).toBe(false);
  const generation = (await state(app)).generation;
  await mock.eval(`m => m.queueDialog('${path}')`);
  await app.keyboard.press("ControlOrMeta+o");
  await expect.poll(async () => (await state(app)).generation).toBeGreaterThan(generation);
  expect(await clips(mock)).toEqual(split);

  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  // The right fragment starts at the left fragment's end. A right extension would overlap it.
  await playlist.getByRole("button", { name: "Draw", exact: true }).click();
  await chooseAutomation(app, canvas, playlist);
  const beforeRefusal = await state(app);
  await startDrag(app, canvas, 190, 238);
  await app.mouse.up();
  await expect(playlist.getByRole("alert")).toContainText("remain disjoint");
  const sent = await mock.calls("arrangement_edit");
  expect(sent).toHaveLength(2);
  expect(JSON.parse(String(sent[1].args.editsJson))).toEqual([
    { kind: "resize", clip: 4, startTick: 1920, content: { ...curve, lengthTicks: 2880 } },
  ]);
  expect(await state(app)).toMatchObject({ generation: beforeRefusal.generation, revision: beforeRefusal.revision });
  expect(await clips(mock)).toEqual(split);
  expect((await mock.view()).canUndo).toBe(false);
});

test("a marquee subset moves, copies, nudges, mutes, duplicates and deletes in mixed batches", async ({ app, mock }) => {
  const { playlist, canvas } = await setup(app);
  await playlist.getByRole("button", { name: "Select", exact: true }).click();
  const selectSubset = async () => {
    await app.mouse.move(...await at(canvas, 70, 10));
    await app.mouse.down();
    await app.mouse.move(...await at(canvas, 435, 62), { steps: 5 });
    await app.mouse.up();
    await expect(playlist.getByText("2 selected", { exact: true })).toBeVisible();
  };
  await selectSubset();

  const beforeMove = await state(app);
  await startDrag(app, canvas, 150, 198);
  await app.mouse.up();
  const moved = [{ ...automation, startTick: 2880 }, { ...pattern, startTick: 7680 }, outside];
  const moveOps = [
    { kind: "move", clip: 4, track: 2, startTick: 2880 },
    { kind: "move", clip: 5, track: 2, startTick: 7680 },
  ] satisfies ArrangementOperation[];
  await expectEdit(mock, canvas, beforeMove, 0, moveOps, moved);
  await undo(app, mock, canvas, originals);

  const beforeNudge = await state(app);
  await app.keyboard.press("ArrowRight");
  await expectEdit(mock, canvas, beforeNudge, 1, moveOps, moved);
  await undo(app, mock, canvas, originals);

  const beforeCopy = await state(app);
  await app.keyboard.down("Alt");
  await startDrag(app, canvas, 150, 198);
  await app.mouse.up();
  await app.keyboard.up("Alt");
  await expectEdit(mock, canvas, beforeCopy, 2, [
    { kind: "copy", clip: 4, track: 2, startTick: 2880 },
    { kind: "copy", clip: 5, track: 2, startTick: 7680 },
  ], [...originals, { ...moved[0], id: 10, content: { ...curve, priority: 11 } }, { ...moved[1], id: 12 }]);
  await undo(app, mock, canvas, originals);

  await selectSubset();
  await playlist.getByRole("button", { name: "Slice", exact: true }).click();
  await app.mouse.click(...await at(canvas, 192));
  expect(await mock.calls("arrangement_edit")).toHaveLength(3);
  expect(await clips(mock)).toEqual(originals);

  const beforeMute = await state(app);
  await playlist.getByRole("button", { name: "Mute selected clips" }).click();
  await expectEdit(mock, canvas, beforeMute, 3, [
    { kind: "setClip", clip: 4, name: "Volume curve", muted: true },
    { kind: "setClip", clip: 5, name: "Beat", muted: true },
  ], [{ ...automation, muted: true }, { ...pattern, muted: true }, outside]);
  await undo(app, mock, canvas, originals);

  const beforeDuplicate = await state(app);
  await playlist.getByRole("button", { name: "Duplicate clips", exact: true }).click();
  await expectEdit(mock, canvas, beforeDuplicate, 4, [
    { kind: "copy", clip: 4, track: 2, startTick: 8640 },
    { kind: "copy", clip: 5, track: 2, startTick: 13440 },
  ], [...originals, { ...automation, id: 13, startTick: 8640, content: { ...curve, priority: 14 } },
    { ...pattern, id: 15, startTick: 13440 }]);
  await undo(app, mock, canvas, originals);

  await playlist.getByRole("button", { name: "Select", exact: true }).click();
  await selectSubset();
  const beforeDelete = await state(app);
  await app.keyboard.press("Delete");
  await expectEdit(mock, canvas, beforeDelete, 5, [
    { kind: "deleteClip", clip: 4 }, { kind: "deleteClip", clip: 5 },
  ], [outside]);
  await undo(app, mock, canvas, originals);
});
