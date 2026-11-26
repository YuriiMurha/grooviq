import type { Locator, Page } from "@playwright/test";
import type { ArrangementPage, AudioContent } from "../src/api/arrangement";
import { expect, generatedWav, project, soundsFor, test } from "./fixture";

const sourceRate = 44100;
const sourceFrames = 88200;
const audio: AudioContent = {
  type: "audio", asset: 4, sourceStart: 22050, sourceEnd: 66150,
  gain: 0.75, fadeIn: 441, fadeOut: 882, reverse: false,
};

test.use({ seed: {
  project: project(["Kick"], {}, {
    nextId: 6,
    arrangement: {
      tracks: [{ id: 2, name: "Audio", color: 1, mute: false }],
      clips: [{ id: 5, track: 2, name: "Break", startTick: 960, muted: false, content: audio }],
      audioAssets: [{ id: 4, sourceRate, sourceFrames, sample: { path: "break.wav", file: "samples/break.wav" },
        originalTempo: 99.5, originalKey: { root: 1, scale: "minor" } }],
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

test.afterEach(async ({ mock }) => {
  expect((await mock.calls()).filter(({ cmd, args }) =>
    cmd === "transport_play" || cmd === "synth_note" ||
    (["channel_note", "test_tone", "channel_preview"].includes(cmd) && args.on === true))).toEqual([]);
});

async function openInspector(app: Page) {
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await app.keyboard.press("F5");
  const playlist = app.getByRole("region", { name: "Playlist", exact: true });
  const canvas = playlist.getByRole("application", { name: "Playlist clips" });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  const select = () => playlist.getByRole("combobox", { name: "Selected playlist clip" }).selectOption("5");
  await select();
  const inspector = playlist.getByRole("group", { name: "Audio clip inspector" });
  return { canvas, select, inspector };
}

const slider = (inspector: Locator, name: string) => inspector.getByRole("slider", { name, exact: true });

test("commits each audio setting once, with one undo, and keeps the settings on reopen", async ({ app, mock }) => {
  const { canvas, select, inspector } = await openInspector(app);
  const gain = inspector.getByRole("slider", { name: "Gain", exact: true });
  const fadeIn = inspector.getByRole("slider", { name: "Fade in", exact: true });
  const fadeOut = inspector.getByRole("slider", { name: "Fade out", exact: true });
  const reverse = inspector.getByRole("button", { name: "Reverse", exact: true });

  const commands = ["arrangement_edit", "arrangement_history", "project_edit", "project_save_as", "project_open"];
  const calls = async () => (await mock.calls()).filter(({ cmd }) => commands.includes(cmd)).map(({ cmd, args }) => {
    if (cmd === "arrangement_edit") {
      const { editsJson, ...stamp } = args;
      return { cmd, args: { ...stamp, edits: JSON.parse(String(editsJson)) as unknown } };
    }
    return { cmd, args };
  });
  const expected: Array<{ cmd: string; args: Record<string, unknown> }> = [];
  const currentContent = async () => (await arrangement(app)).clips[0]?.content;
  const stepHistory = async (undo: boolean, content: AudioContent) => {
    const { generation, revision } = await arrangement(app);
    await canvas.focus();
    await app.keyboard.press(undo ? "ControlOrMeta+z" : "ControlOrMeta+Shift+z");
    expected.push({ cmd: "arrangement_history", args: { generation, revision, undo } });
    await expect.poll(calls).toEqual(expected);
    await expect.poll(currentContent).toEqual(content);
    await expect(canvas).toHaveAttribute("aria-disabled", "false");
  };
  const edit = async (field: "gain" | "fadeIn" | "fadeOut" | "reverse", value: number | boolean, act: () => Promise<void>) => {
    const { generation, revision, clips: [clip] } = await arrangement(app);
    const next = { ...clip.content, [field]: value } as AudioContent;
    await act();
    expected.push({ cmd: "arrangement_edit", args: { generation, revision,
      edits: [{ kind: "resize", clip: 5, startTick: 960, content: next }] } });
    await expect.poll(calls).toEqual(expected);
    await expect.poll(currentContent).toEqual(next);
    await expect(canvas).toHaveAttribute("aria-disabled", "false");
    await stepHistory(true, clip.content as AudioContent);
    await stepHistory(false, next);
  };

  await edit("gain", 2, async () => {
    await gain.press("End");
    expect(await calls()).toEqual(expected);
    await expect(gain).toHaveValue("2");
    await gain.press("Enter");
  });
  await edit("fadeIn", 44100, async () => {
    await fadeIn.press("End");
    expect(await calls()).toEqual(expected);
    const count = (await mock.calls("arrangement_edit")).length;
    await mock.eval('m => m.holdCommand("arrangement_edit")');
    try {
      await fadeIn.press("Enter");
      await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(count + 1);
      await expect(canvas).toHaveAttribute("aria-disabled", "true");
      await expect(fadeIn).toBeFocused();
    } finally {
      await mock.eval('m => m.releaseCommand("arrangement_edit")');
    }
    await expect(canvas).toHaveAttribute("aria-disabled", "false");
    await expect(fadeIn).toBeFocused();
  });
  await edit("fadeOut", 0, async () => {
    await fadeOut.press("Home");
    expect(await calls()).toEqual(expected);
    await fadeOut.press("Tab");
  });
  await edit("reverse", true, () => reverse.click());

  await expect(inspector.getByText("1 s", { exact: true })).toBeVisible();
  await expect(inspector.getByText("2 beats · 0.5 bars", { exact: true })).toBeVisible();
  const tempo = app.getByRole("textbox", { name: "BPM" });
  const { generation, notesRevision } = await mock.view();
  await tempo.fill("137.1");
  await tempo.press("Enter");
  expected.push({ cmd: "project_edit", args: {
    generation, stamp: { generation, patternId: 0, revision: notesRevision }, edit: { kind: "setTempo", bpm: 137.1 },
  } });
  await expect.poll(calls).toEqual(expected);
  await expect(inspector.getByText("2.285 beats · 0.571 bars", { exact: true })).toBeVisible();
  await expect(inspector.getByText("1 s", { exact: true })).toBeVisible();

  const saved = await arrangement(app);
  const path = "/qa/projects/Inspected break.grooviq";
  await mock.eval(`m => m.queueDialog(${JSON.stringify(path)}, ${JSON.stringify(path)})`);
  await app.keyboard.press("ControlOrMeta+s");
  expected.push({ cmd: "project_save_as", args: { path } });
  await expect.poll(async () => (await mock.view()).path).toBe(path);
  await expect.poll(calls).toEqual(expected);

  await fadeOut.press("End");
  await expect(fadeOut).toHaveValue("44100");
  await app.keyboard.press("ControlOrMeta+o");
  expected.push({ cmd: "project_open", args: { path } });
  await expect.poll(async () => (await mock.view()).generation).toBe(saved.generation + 1);
  await expect.poll(async () => (await arrangement(app)).clips).toEqual(saved.clips);
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await select();
  await expect(gain).toHaveValue("2");
  await expect(fadeIn).toHaveValue("44100");
  await expect(fadeOut).toHaveValue("0");
  await expect(reverse).toHaveAttribute("aria-pressed", "true");
  expect(await calls()).toEqual(expected);
});

test("a pointer drag on Gain previews locally and commits once on release", async ({ app, mock }) => {
  const { inspector } = await openInspector(app);
  const gain = inspector.getByRole("slider", { name: "Gain", exact: true });
  const edits = async () => (await mock.calls("arrangement_edit")).map(({ args }) => JSON.parse(String(args.editsJson)) as unknown);
  const { clips: [clip] } = await arrangement(app);
  const box = await gain.boundingBox();
  if (!box) throw new Error("Missing gain slider");
  const y = box.y + box.height / 2;
  if (clip.content.type !== "audio") throw new Error("Expected an audio clip");
  // Press on the thumb at the clip's current gain (0..2 across the track), then move the value
  // the way a drag does. WebKit automation does not move a range input from synthesized mouse
  // moves, so the drag's value changes are dispatched as the input events the browser fires.
  await app.mouse.move(box.x + box.width * (clip.content.gain / 2), y);
  await app.mouse.down();
  for (const value of ["1.5", "2"]) {
    await gain.evaluate((input: HTMLInputElement, next) => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, next);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }, value);
    // The rendered preview follows the drag, not only the DOM value.
    await expect(gain).toHaveAttribute("aria-valuetext", value);
    expect(await edits()).toEqual([]);
  }
  // Leaving the slider while the button is held must not commit either.
  await app.mouse.move(box.x + box.width + 15, y - box.height);
  expect(await edits()).toEqual([]);
  await app.mouse.up();
  await expect.poll(edits).toEqual([[
    { kind: "resize", clip: 5, startTick: 960, content: { ...clip.content, gain: 2 } },
  ]]);
});

test("Escape discards a keyboard draft before Tab can commit it", async ({ app, mock }) => {
  const { inspector } = await openInspector(app);
  const fadeIn = slider(inspector, "Fade in");
  await fadeIn.press("End");
  await expect(fadeIn).toHaveValue("44100");
  await fadeIn.press("Escape");
  await fadeIn.press("Tab");
  await expect(fadeIn).toHaveValue("441");
  expect(await mock.calls("arrangement_edit")).toEqual([]);
});

test("the first slider step after global undo starts from the restored value", async ({ app, mock }) => {
  const { canvas, inspector } = await openInspector(app);
  const fadeIn = slider(inspector, "Fade in");
  await fadeIn.press("End");
  await fadeIn.press("Enter");
  await expect.poll(async () => (await arrangement(app)).clips[0].content).toMatchObject({ fadeIn: 44100 });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await fadeIn.press("Home");
  await expect(fadeIn).toHaveValue("0");
  await app.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await mock.calls("arrangement_history")).length).toBe(1);
  await expect(fadeIn).toHaveValue("441");
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await expect(fadeIn).toBeFocused();
  await app.keyboard.press("ArrowRight");
  await expect(fadeIn).toHaveValue("442");
  await app.keyboard.press("Enter");
  await expect.poll(async () => (await arrangement(app)).clips[0].content).toMatchObject({ fadeIn: 442 });
  expect(await mock.calls("arrangement_edit")).toHaveLength(2);
});

test("a pending project replacement discards the old draft and blocks new slider input", async ({ app, mock }) => {
  const { canvas, inspector } = await openInspector(app);
  const fadeIn = slider(inspector, "Fade in");
  const count = (await mock.calls("pattern_edit")).length;
  await mock.eval('m => m.holdCommand("pattern_edit")');
  try {
    await fadeIn.press("End");
    await expect(fadeIn).toHaveValue("44100");
    await app.keyboard.press("ControlOrMeta+n");
    // Hold replacement in preflight, before it retires the old playlist.
    await expect.poll(async () => (await mock.calls("pattern_edit")).length).toBe(count + 1);
    await expect(canvas).toHaveAttribute("aria-disabled", "false");
    await expect(fadeIn).toHaveValue("441");
    await fadeIn.press("Home");
    await expect(fadeIn).toHaveValue("441");
    await fadeIn.press("Enter");
    expect(await mock.calls("arrangement_edit")).toEqual([]);
  } finally {
    await mock.eval('m => m.releaseCommand("pattern_edit")');
  }
  await expect(inspector).toHaveCount(0);
  expect(await mock.calls("arrangement_edit")).toEqual([]);
});

test("opening a modal discards the slider draft on focus transfer", async ({ app, mock }) => {
  const { inspector } = await openInspector(app);
  const fadeIn = slider(inspector, "Fade in");
  await fadeIn.press("End");
  await expect(fadeIn).toHaveValue("44100");
  await app.keyboard.press("ControlOrMeta+r");
  const dialog = app.getByRole("dialog", { name: "Export WAV" });
  await expect(dialog).toBeVisible();
  await expect(fadeIn).not.toBeFocused();
  await expect(fadeIn).toHaveValue("441");
  expect(await mock.calls("arrangement_edit")).toEqual([]);
});
