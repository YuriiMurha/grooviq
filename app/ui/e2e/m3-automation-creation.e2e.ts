import type { Page } from "@playwright/test";
import type { ArrangementOperation, ArrangementPage, ArrangementReply, CurvePoint } from "../src/api/arrangement";
import type { ProjectView } from "../src/api/project";
import { expect, presetValues, project, soundsFor, test, type Mock } from "./fixture";

const track = { id: 3, name: "Beat lane", color: 1, mute: false };
const pattern = { id: 4, track: track.id, name: "Beat", startTick: 0, muted: false,
  content: { type: "pattern" as const, pattern: 0, offsetTick: 0, lengthTicks: 3840 } };
const loop = { startTick: 960, endTick: 2880, enabled: true };
const arrangement = { tracks: [track], clips: [pattern], audioAssets: [], loopRegion: loop };
const scene = project(["Kick"], {}, { nextId: 8, arrangement });
scene.channels[0].sampler.fine = -6.25;
scene.channels.push({ ...scene.channels[0], id: 2, name: "Lead", kind: "synth", sample: null,
  sampler: {}, synth: { ...presetValues("Init"), osc1_level: 0.625 }, preset: "Init" });
const empty = { ...scene, arrangement: { tracks: [], clips: [], audioAssets: [], loopRegion: null } };
const replacementPath = "/qa/projects/Replacement automation.grooviq";
test.use({ seed: { project: scene, files: { ...soundsFor(["Kick"]), [replacementPath]: { kind: "project", project: { ...scene, name: "Replacement" } } } } });

function invoke<T>(page: Page, command: string, args: Record<string, unknown> = {}) {
  return page.evaluate(({ command, args }) => (window as unknown as {
    __TAURI_INTERNALS__: { invoke: (command: string, args: Record<string, unknown>) => Promise<unknown> };
  }).__TAURI_INTERNALS__.invoke(command, args), { command, args }) as Promise<T>;
}
const readArrangement = (page: Page) => invoke<ArrangementPage>(page, "arrangement_view", { offset: 0, limit: 256 });
const readProject = (page: Page) => invoke<ProjectView>(page, "project_view");
async function readPoints(page: Page, clip: number) {
  const { revision, generation } = await readArrangement(page);
  return (await invoke<{ points: CurvePoint[] }>(page, "arrangement_curve", { generation, revision, clip })).points;
}
const expectSilent = async (mock: Mock) => {
  const audio = (await mock.calls()).filter(({ cmd }) => ["transport_play", "transport_stop", "channel_note", "channel_param_live", "channel_preview"].includes(cmd));
  expect(audio).toEqual([]);
};

async function openPlaylist(app: Page) {
  await app.keyboard.press("F5");
  const playlist = app.getByRole("region", { name: "Playlist", exact: true });
  const canvas = playlist.getByRole("application", { name: "Playlist clips" });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  const select = async (name: string) => {
    await playlist.getByRole("button", { name: /^Selected playlist clip:/ }).click();
    await playlist.getByRole("listbox", { name: "Selected playlist clip" }).getByRole("option", { name, exact: true }).click();
  };
  const dock = playlist.getByRole("region", { name: "Automation point editor" });
  return { playlist, canvas, select, dock, graph: dock.getByRole("group", { name: "Automation curve" }) };
}

async function openKnob(app: Page, channel: "Kick" | "Lead") {
  const kind = channel === "Kick" ? "sampler" : "synth";
  await app.getByRole("button", { name: `Edit the ${channel} ${kind}`, exact: true }).click();
  const editor = app.getByRole("dialog", { name: `${channel} ${kind}`, exact: true });
  const knob = editor.locator(channel === "Kick" ? '[data-param="fine"]' : '[data-param="osc1_level"]');
  return { editor, knob, close: () => editor.getByRole("button", { name: `Close the ${channel} ${kind}` }).click() };
}

const menuAction = (app: Page) => app.getByRole("menuitem", { name: "Create automation clip" });
const placementDialog = (app: Page) => app.getByRole("dialog", { name: "Automation clip placement" });
async function choose(app: Page, label: string, option: string) {
  const dialog = placementDialog(app);
  await dialog.getByRole("button", { name: new RegExp(`^${label}:`) }).click();
  await dialog.getByRole("listbox", { name: label }).getByRole("option", { name: option, exact: true }).click();
}

test("creates a sampler clip on the selected track at loop start, then edits, undoes and reopens", async ({ app, mock }) => {
  const { canvas, select, dock, graph } = await openPlaylist(app);
  await select("Beat · Track 1 · 0 ticks");
  const before = await readArrangement(app);
  const { knob, close } = await openKnob(app, "Kick");
  await knob.click({ button: "right" });
  await expect(menuAction(app)).toBeFocused();
  await menuAction(app).press("Escape");
  await expect(knob).toBeFocused();
  await knob.press("Shift+F10");
  await menuAction(app).press("Enter");
  await expect(placementDialog(app).getByRole("button", { name: "Automation track: Beat lane" })).toBeVisible();
  await choose(app, "Automation start", "Loop start");
  await placementDialog(app).getByRole("button", { name: "Create clip" }).click();
  await close();

  const seed = { tick: 0, value: -6.25, mode: "linear" };
  const createdTarget = { type: "instrumentParam", channel: 1, instrument: "sampler", param: "fine" };
  await expect(graph).toHaveAttribute("aria-disabled", "false");
  const [create] = await mock.calls("arrangement_edit");
  expect(create.args).toMatchObject({ generation: before.generation, revision: before.revision });
  expect(JSON.parse(String(create.args.editsJson))).toEqual([{ kind: "createClip", track: 3, name: "Fine tune",
    startTick: 960, muted: false, content: { type: "automation", target: createdTarget, priority: 0,
      offsetTick: 0, lengthTicks: 3840, points: [seed] } }]);
  const savedClip = (await readArrangement(app)).clips.find(clip => clip.content.type === "automation");
  expect(savedClip).toMatchObject({ id: 8, content: { target: createdTarget, priority: 9 } });
  const clip = savedClip!.id;
  expect(await readPoints(app, clip)).toEqual([seed]);

  const hold = { ...seed, mode: "hold" };
  await dock.getByRole("combobox", { name: "Interpolation" }).selectOption("hold");
  await expect.poll(() => readPoints(app, clip)).toEqual([hold]);
  await expect(canvas).toHaveAttribute("aria-disabled", "false");

  const target = dock.getByRole("combobox", { name: "Automation target" });
  const priority = dock.getByRole("textbox", { name: "Overlap priority" });
  const apply = dock.getByRole("button", { name: "Apply settings" });
  await target.selectOption("1:sampler:tune");
  await priority.fill("10");
  const beforeSettings = await readArrangement(app);
  await mock.eval('m => m.holdCommand("arrangement_edit")');
  try {
    await priority.press("Enter");
    await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(3);
    await expect(priority).toBeDisabled();
    await expect(apply).toBeFocused();
  } finally {
    await mock.eval('m => m.releaseCommand("arrangement_edit")');
  }
  const changedTarget = { ...createdTarget, param: "tune" };
  await expect.poll(async () => (await readArrangement(app)).clips.find(item => item.id === clip)?.content)
    .toMatchObject({ target: changedTarget, priority: 10, pointCount: 1 });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await expect(apply).toBeFocused();
  const edits = await mock.calls("arrangement_edit");
  expect(edits).toHaveLength(3);
  expect(edits[2].args).toMatchObject({ generation: beforeSettings.generation, revision: beforeSettings.revision });
  expect(JSON.parse(String(edits[2].args.editsJson))).toEqual([{ kind: "setAutomationClip", clip, target: changedTarget, priority: 10 }]);

  await app.keyboard.press("ControlOrMeta+z");
  await expect(priority).toHaveValue("9");
  await expect(target).toHaveValue("1:sampler:fine");
  expect(await readPoints(app, clip)).toEqual([hold]);
  expect(await mock.calls("arrangement_history")).toHaveLength(1);
  await app.keyboard.press("ControlOrMeta+Shift+z");
  await expect(priority).toHaveValue("10");

  const path = "/qa/projects/Automation creation.grooviq";
  await mock.eval(`m => m.queueDialog(${JSON.stringify(path)}, ${JSON.stringify(path)})`);
  await app.keyboard.press("ControlOrMeta+s");
  await expect.poll(async () => (await mock.view()).path).toBe(path);
  await app.keyboard.press("ControlOrMeta+o");
  await expect.poll(async () => (await mock.view()).generation).toBe(before.generation + 1);
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await select("Fine tune · Track 1 · 960 ticks");
  await expect(target).toHaveValue("1:sampler:tune");
  await expect(priority).toHaveValue("10");
  expect(await readPoints(app, clip)).toEqual([hold]);
  expect((await readProject(app)).channels[0].sampler.fine).toBe(-6.25);
  expect((await readProject(app)).channels[0].sampler.tune).toBe(0);
  await expectSilent(mock);

  await app.getByRole("button", { name: "Playback mode: Pattern" }).click();
  await app.getByRole("listbox", { name: "Playback mode" }).getByRole("option", { name: "Song", exact: true }).click();
  await expect(app.getByRole("button", { name: "Playback mode: Song" })).toBeEnabled();
  await app.getByRole("button", { name: "Play", exact: true }).click();
  await expect.poll(async () => (await mock.calls("transport_play")).length).toBe(1);
  await app.getByRole("button", { name: "Stop", exact: true }).click();
  expect((await mock.calls()).filter(({ cmd }) => cmd === "transport_play" || cmd === "transport_stop").map(({ cmd }) => cmd))
    .toEqual(["transport_play", "transport_stop"]);
  expect((await readArrangement(app)).playMode).toBe("song");
  expect(await readPoints(app, clip)).toEqual([hold]);
  expect((await readProject(app)).channels[0].sampler.tune).toBe(0);
});

test.describe("when the song has no tracks", () => {
  test.use({ seed: { project: empty, files: soundsFor(["Kick"]) } });

  test("offers an explicit song start and creates a track and synth clip in one Undo step", async ({ app, mock }) => {
    const { canvas, graph } = await openPlaylist(app);
    expect(await mock.eval<number>("m => { m.pauseLive(true); return m.disconnectLive(); }")).toBeGreaterThan(0);
    const { knob, close } = await openKnob(app, "Lead");
    await knob.press("Shift+F10");
    await menuAction(app).press("Enter");
    const dialog = placementDialog(app);
    const create = dialog.getByRole("button", { name: "Create clip" });
    await expect(create).toBeDisabled();
    await choose(app, "Automation track", "Create Track 1");
    await expect(create).toBeDisabled();
    await choose(app, "Automation start", "Song start");
    await create.click();
    await close();
    await expect(graph).toHaveAttribute("aria-disabled", "false");
    const edits = await mock.calls("arrangement_edit");
    expect(edits).toHaveLength(1);
    expect(JSON.parse(String(edits[0].args.editsJson))).toEqual([
      { kind: "createTrack", name: "Track 1", color: 0 },
      { kind: "createClipOnCreatedTrack", trackOperation: 0, name: "Osc 1 level", startTick: 0, muted: false,
        content: { type: "automation", target: { type: "instrumentParam", channel: 2, instrument: "synth", param: "osc1_level" },
          priority: 0, offsetTick: 0, lengthTicks: 3840, points: [{ tick: 0, value: 0.625, mode: "linear" }] } },
    ]);
    await app.keyboard.press("ControlOrMeta+z");
    await expect.poll(async () => (await readArrangement(app)).tracks).toEqual([]);
    await expect(canvas).toHaveAttribute("aria-disabled", "false");
    expect((await readArrangement(app)).clips).toEqual([]);
    expect(await mock.calls("arrangement_history")).toHaveLength(1);
    await expectSilent(mock);
  });
});

test("invalid ranges, linked sources and invalid priorities leave the stored settings and curve intact", async ({ app, mock }) => {
  const { playlist, canvas, select, dock, graph } = await openPlaylist(app);
  const path = "/qa/projects/Automation refusals.grooviq";
  await mock.eval(`m => m.queueDialog(${JSON.stringify(path)})`);
  await app.keyboard.press("ControlOrMeta+s");
  await expect.poll(async () => (await mock.view()).path).toBe(path);
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  const { knob, close } = await openKnob(app, "Kick");
  await knob.click({ button: "right" });
  await menuAction(app).click();
  await choose(app, "Automation track", "Beat lane");
  await choose(app, "Automation start", "Playhead");
  await placementDialog(app).getByRole("button", { name: "Create clip" }).click();
  await close();
  await expect(graph).toHaveAttribute("aria-disabled", "false");
  const target = dock.getByRole("combobox", { name: "Automation target" });
  const priority = dock.getByRole("textbox", { name: "Overlap priority" });
  const apply = dock.getByRole("button", { name: "Apply settings" });
  const alert = playlist.getByRole("alert");
  const refresh = async () => {
    await playlist.getByRole("button", { name: "Refresh playlist" }).click();
    await expect(canvas).toHaveAttribute("aria-disabled", "false");
  };
  const original = await readArrangement(app);
  const clip = original.clips.find(item => item.content.type === "automation")!;
  expect(clip.startTick).toBe(0);
  const points = await readPoints(app, clip.id);
  await priority.fill("4.5");
  await expect(apply).toHaveAttribute("aria-disabled", "true");
  await priority.press("Escape");
  await expect(priority).toHaveValue("9");
  expect(await mock.calls("arrangement_edit")).toHaveLength(1);

  await target.selectOption("1:channelPan");
  await apply.click();
  await expect(alert).toContainText("automation value");
  expect((await readArrangement(app)).clips).toEqual(original.clips);
  expect(await readPoints(app, clip.id)).toEqual(points);
  await app.keyboard.press("ControlOrMeta+s");
  await expect(app.getByRole("status", { name: "Messages" })).toContainText("the project was not saved");
  await refresh();

  await priority.fill("4294967295");
  await mock.eval('m => m.holdCommand("arrangement_edit")');
  try {
    await priority.press("Enter");
    await expect(priority).toBeDisabled();
    await expect(apply).toBeFocused();
  } finally {
    await mock.eval('m => m.releaseCommand("arrangement_edit")');
  }
  await expect(alert).toContainText("exceeds the next available id");
  await expect(apply).toBeFocused();
  expect((await readArrangement(app)).clips).toEqual(original.clips);
  await app.keyboard.press("ControlOrMeta+s");
  await expect(app.getByText(/the project was not saved/)).toHaveCount(2);
  await refresh();

  const { revision, generation } = await readArrangement(app);
  const copy: ArrangementOperation = { kind: "copy", clip: clip.id, track: track.id, startTick: 3840 };
  const copied = await invoke<ArrangementReply>(app, "arrangement_edit", { revision, generation, editsJson: JSON.stringify([copy]) });
  expect(copied.status).toBe("applied");
  await refresh();
  await select("Fine tune · Track 1 · 0 ticks");
  const linked = await readArrangement(app);
  await target.selectOption("1:sampler:tune");
  await apply.click();
  await expect(alert).toContainText("shares its automation source");
  expect((await readArrangement(app)).clips).toEqual(linked.clips);
  expect(await readPoints(app, clip.id)).toEqual(points);
  expect(await readPoints(app, copied.createdIds[0])).toEqual(points);
  expect(await mock.calls("arrangement_history")).toHaveLength(0);
  await expectSilent(mock);
});

test("keeps normal knob editing and drops a command during pending edits and a modal replacement", async ({ app, mock }) => {
  await openPlaylist(app);
  const { editor, knob } = await openKnob(app, "Kick");
  const tune = editor.locator('[data-param="tune"]');
  await mock.eval('m => m.holdCommand("project_edit")');
  // The other knob retains its canonical displayed value during this held edit.
  await tune.press("ArrowRight");
  await expect.poll(async () => (await mock.calls("project_edit")).length).toBe(1);
  await knob.click({ button: "right" });
  await expect(menuAction(app)).toHaveCount(0);
  await mock.eval('m => m.releaseCommand("project_edit")');
  await expect.poll(async () => (await readProject(app)).channels[0].sampler.tune).toBeGreaterThan(0);
  await expect(tune).not.toHaveAttribute("aria-valuenow", "0");
  // A segmented control has no menu, even when its parameter is valid natively.
  await editor.locator('[data-param="voice_mode"]').first().click({ button: "right" });
  await expect(menuAction(app)).toHaveCount(0);

  await knob.press("Shift+F10");
  await menuAction(app).press("Enter");
  await choose(app, "Automation track", "Beat lane");
  await mock.eval(`m => m.queueDialog(${JSON.stringify(replacementPath)})`);
  await app.keyboard.press("ControlOrMeta+o");
  await expect(app.getByRole("button", { name: "Don't save" })).toBeVisible();
  await expect(placementDialog(app)).toHaveCount(0);
  await app.getByRole("button", { name: "Don't save" }).click();
  await expect.poll(async () => (await mock.view()).generation).toBe(1);
  await expect(editor).toHaveCount(0);
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
  expect((await readArrangement(app)).clips).toEqual([pattern]);
  expect((await readProject(app)).channels[0].sampler.fine).toBe(-6.25);
  expect((await readProject(app)).channels[0].sampler.tune).toBe(0);
});

test("keeps staged settings while both a held and a queued knob edit settle", async ({ app, mock }) => {
  const { select, dock, graph, canvas } = await openPlaylist(app);
  await select("Beat · Track 1 · 0 ticks");
  const { knob, close } = await openKnob(app, "Kick");
  await knob.press("Shift+F10");
  await menuAction(app).press("Enter");
  await choose(app, "Automation start", "Loop start");
  const priority = dock.getByRole("textbox", { name: "Overlap priority" });
  const apply = dock.getByRole("button", { name: "Apply settings" });
  await mock.eval('m => m.holdCommand("arrangement_curve")');
  try {
    await placementDialog(app).getByRole("button", { name: "Create clip" }).click();
    await close();
    await expect.poll(async () => (await mock.calls("arrangement_curve")).length).toBe(1);
    await expect(canvas).toHaveAttribute("aria-disabled", "false");
    await expect(priority).toBeDisabled();
    await expect(dock.getByRole("combobox", { name: "Automation target" })).toBeDisabled();
    await expect(apply).toHaveAttribute("aria-disabled", "true");
  } finally {
    await mock.eval('m => m.releaseCommand("arrangement_curve")');
  }
  await expect(graph).toHaveAttribute("aria-disabled", "false");
  await priority.fill("10");

  const { editor, knob: fine, close: closeAgain } = await openKnob(app, "Kick");
  const tune = editor.locator('[data-param="tune"]');
  await mock.eval('m => m.holdCommand("project_edit")');
  try {
    await tune.press("ArrowRight");
    await expect.poll(async () => (await mock.calls("project_edit")).length).toBe(1);
    await fine.press("ArrowRight");
    await expect(fine).not.toHaveAttribute("aria-valuenow", "-6.25");
    await apply.press("Enter");
    await expect(priority).toHaveValue("10");
    expect(await mock.calls("arrangement_edit")).toHaveLength(1);
  } finally {
    await mock.eval('m => m.releaseCommand("project_edit")');
  }
  await expect.poll(async () => (await mock.calls("project_edit")).map(call => call.args.edit))
    .toEqual([expect.objectContaining({ param: "tune" }), expect.objectContaining({ param: "fine" })]);
  await expect.poll(async () => (await readProject(app)).channels[0].sampler.fine).toBeGreaterThan(-6.25);
  expect((await readProject(app)).channels[0].sampler.tune).toBeGreaterThan(0);
  await closeAgain();
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await expect(priority).toHaveValue("10");
  const before = await readArrangement(app);
  await apply.press("Enter");
  await expect.poll(async () => (await readArrangement(app)).clips[1].content).toMatchObject({ priority: 10 });
  expect((await mock.calls("arrangement_edit"))[1].args).toMatchObject({ generation: before.generation, revision: before.revision });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");

  await priority.fill("11");
  await app.getByRole("textbox", { name: "BPM", exact: true }).fill("126");
  // Preserve focus on the tempo draft, so the transition itself settles it after settings admission.
  await dock.getByRole("form", { name: "Automation clip settings" }).evaluate((form: HTMLFormElement) => form.requestSubmit());
  await expect.poll(async () => (await mock.view()).bpm).toBe(126);
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  expect(await mock.calls("arrangement_edit")).toHaveLength(2);
  expect((await readArrangement(app)).clips[1].content).toMatchObject({ priority: 10 });

  await expect(graph).toHaveAttribute("aria-disabled", "false");
  await priority.fill("11");
  const { editor: revised, close: closeRevised } = await openKnob(app, "Kick");
  const previous = await readArrangement(app);
  const clip = previous.clips[1].id;
  const lastTune = (await readProject(app)).channels[0].sampler.tune;
  const sameCount = [{ tick: 0, value: -4, mode: "linear" }] satisfies CurvePoint[];
  await mock.eval('m => m.holdCommand("project_edit")');
  try {
    await revised.locator('[data-param="tune"]').press("ArrowRight");
    await expect.poll(async () => (await mock.calls("project_edit")).length).toBe(4);
    await apply.press("Enter");
    await expect(priority).toHaveValue("11");
    const updated = await invoke<ArrangementReply>(app, "arrangement_edit", {
      generation: previous.generation, revision: previous.revision,
      editsJson: JSON.stringify([{ kind: "setAutomationPoints", clip, points: sameCount }]),
    });
    expect(updated.status).toBe("applied");
  } finally {
    await mock.eval('m => m.releaseCommand("project_edit")');
  }
  await expect.poll(async () => (await readProject(app)).channels[0].sampler.tune).toBeGreaterThan(lastTune);
  await closeRevised();
  await expect(graph).toHaveAttribute("aria-disabled", "false");
  await expect(priority).toHaveValue("10");
  expect(await readPoints(app, clip)).toEqual(sameCount);
  expect(await mock.calls("arrangement_edit")).toHaveLength(3);
});

test("lets the device owner finish a live wheel gesture before creating on another knob", async ({ app, mock }) => {
  const { select, graph } = await openPlaylist(app);
  await select("Beat · Track 1 · 0 ticks");
  const { editor, knob, close } = await openKnob(app, "Kick");
  const tune = editor.locator('[data-param="tune"]');
  const openedWhileWheeling = await editor.evaluate(async element => {
    const tuneControl = element.querySelector('[data-param="tune"]')!;
    const fineControl = element.querySelector('[data-param="fine"]')!;
    tuneControl.dispatchEvent(new WheelEvent("wheel", { bubbles: true, cancelable: true, deltaY: -100 }));
    // One input task guarantees the wheel timer has not enqueued a project edit yet.
    fineControl.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, cancelable: true, key: "F10", shiftKey: true }));
    await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    return document.querySelector('[aria-label="Parameter actions"]') !== null;
  });
  expect(openedWhileWheeling).toBe(false);
  await expect.poll(async () => (await readProject(app)).channels[0].sampler.tune).toBeGreaterThan(0);
  await expect(tune).not.toHaveAttribute("aria-valuenow", "0");
  expect((await mock.calls("project_edit")).map(call => call.args.edit)).toEqual([expect.objectContaining({ param: "tune" })]);
  const before = await readArrangement(app);
  await knob.press("Shift+F10");
  await menuAction(app).press("Enter");
  await choose(app, "Automation start", "Loop start");
  await placementDialog(app).getByRole("button", { name: "Create clip" }).click();
  await close();
  await expect(graph).toHaveAttribute("aria-disabled", "false");
  const [created] = await mock.calls("arrangement_edit");
  expect(created.args).toMatchObject({ generation: before.generation, revision: before.revision });
  expect(await mock.calls("project_edit")).toHaveLength(1);
});

test.describe("in a compact workspace with a long target name", () => {
  test.use({ viewport: { width: 900, height: 560 }, seed: { project: {
    ...scene,
    channels: [{ ...scene.channels[0], name: "Generated sampler target ".repeat(8).trim() }, scene.channels[1]],
    arrangement: { ...arrangement, clips: [pattern, { id: 6, track: track.id, name: "Pan sweep", startTick: 0, muted: false,
      content: { type: "automation", target: { type: "channelPan", channel: 1 }, priority: 7, offsetTick: 0, lengthTicks: 3840,
        points: [{ tick: 0, value: 0, mode: "linear" }] } }] },
  }, files: soundsFor(["Kick"]) } });

  test("keeps the workspace, target and priority controls reachable", async ({ app }) => {
    const { select, dock, graph } = await openPlaylist(app);
    await select("Pan sweep · Track 1 · 0 ticks");
    await expect(graph).toHaveAttribute("aria-disabled", "false");
    const target = dock.getByRole("combobox", { name: "Automation target" });
    const priority = dock.getByRole("textbox", { name: "Overlap priority" });
    const apply = dock.getByRole("button", { name: "Apply settings" });
    const targetFits = await target.evaluate(element => {
      const dock = element.closest(".automation-dock")!.getBoundingClientRect();
      const select = element.getBoundingClientRect();
      return select.width > 0 && select.right <= dock.right;
    });
    expect(targetFits).toBe(true);
    expect(await app.locator(".workspace-viewport").evaluate(element => element.clientHeight)).toBeGreaterThan(0);
    await target.selectOption("1:channelVolume");
    await priority.fill("8");
    await apply.click();
    await expect.poll(async () => (await readArrangement(app)).clips[1].content).toMatchObject({ priority: 8 });
  });
});
