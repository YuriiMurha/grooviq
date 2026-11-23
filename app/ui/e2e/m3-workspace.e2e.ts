import type { Page } from "@playwright/test";
import { test, expect, project, soundsFor, clickCell } from "./fixture";
import type { PatternsView, PatternReply } from "../src/api/patterns";
import type { ArrangementPage, ArrangementReply, ProjectHistoryReply } from "../src/api/arrangement";
import type { TrimState, TrimReply } from "../src/api/samplerTrim";

const names = ["A", "B"];
const scene = project(names);
scene.channels[0].frames = 2; scene.channels[1].frames = 2;
scene.channels[1].sampler = { start: 0.6, end: 0.4 };
scene.patterns = [scene.pattern, { ...scene.pattern, id: 3, name: "B", notes: [] }]; scene.nextId = 4;
const file = "/qa/workspace.grooviq";
test.use({ seed: { project: project([]), files: { ...soundsFor(names), [file]: { kind: "project", project: scene } } } });
async function ipc<Result>(page: Page, command: string, args: Record<string, unknown> = {}): Promise<Result> {
  return page.evaluate(({ command, args }) => (window as unknown as {
    __TAURI_INTERNALS__: { invoke: (command: string, args: Record<string, unknown>) => Promise<unknown> }
  }).__TAURI_INTERNALS__.invoke(command, args), { command, args }) as Promise<Result>;
}
const patterns = (page: Page) => ipc<PatternsView>(page, "pattern_list");
const arrangement = (page: Page) => ipc<ArrangementPage>(page, "arrangement_view", { offset: 0, limit: 256 });
async function history(page: Page, undo: boolean) {
  const stamp = await arrangement(page);
  return ipc<ProjectHistoryReply>(page, "arrangement_history", { generation: stamp.generation, revision: stamp.revision, undo });
}
async function select(page: Page, pattern: number) {
  const base = await patterns(page);
  return ipc<PatternReply>(page, "pattern_edit", { generation: base.generation, revision: base.revision, edit: { kind: "select", pattern } });
}
async function note(page: Page, step: number) {
  const base = await patterns(page);
  return ipc(page, "project_edit", { generation: base.generation, stamp: { generation: base.generation, patternId: base.selected, revision: base.view.notesRevision }, edit: { kind: "toggleStep", channel: 1, step } });
}
async function reset(page: Page, channel = 2) {
  const stamp = await ipc<TrimState>(page, "sampler_trim_view", { channel });
  return ipc<TrimReply>(page, "sampler_trim_reset", { channel, stamp: stamp.stamp });
}
function effect(reply: ProjectHistoryReply) { expect(reply.status).toBe("applied"); return reply.status === "applied" ? reply.effect : null; }
type ObservedPattern = { id: number; revision: number; notes: unknown[] };
type WorkspaceState = { generation: number; revision: number; selected: number; dirty: boolean; patterns: ObservedPattern[] };
const snapshot = (mock: { eval: <T>(source: string) => Promise<T> }) =>
  mock.eval<WorkspaceState>("(m) => m.workspaceSnapshot()");
test.beforeEach(async ({ app, mock }) => {
  await mock.eval(`(m) => m.queueDialog('${file}')`);
  await app.keyboard.press("ControlOrMeta+o");
  await expect(app.getByRole("region", { name: "Channel rack" })).toBeVisible();
  await expect.poll(async () => (await patterns(app)).generation).toBe(1);
  await expect(app.getByRole("combobox", { name: "Playback mode" })).toHaveValue("pattern");
});
for (const roll of [false, true]) test(`T9 UI global Undo targets inactive B while A stays selected with roll ${roll}`, async ({ app, mock }) => {
  const rail = app.getByRole("complementary", { name: "Patterns" });
  const aName = (await patterns(app)).patterns.find((pattern) => pattern.id === 0)!.name;
  await clickCell(app, 0, 0);
  await expect.poll(async () => (await snapshot(mock)).patterns.find((pattern) => pattern.id === 0)!.notes.length).toBe(1);
  await rail.getByRole("button", { name: "B", exact: true }).click();
  await expect.poll(async () => (await patterns(app)).selected).toBe(3);
  await clickCell(app, 0, 2);
  await expect.poll(async () => (await snapshot(mock)).patterns.find((pattern) => pattern.id === 3)!.notes.length).toBe(1);
  await rail.getByRole("button", { name: aName, exact: true }).click();
  await expect.poll(async () => (await patterns(app)).selected).toBe(0);
  if (roll) {
    await app.keyboard.press("F7");
    await expect(app.getByLabel("Piano roll notes")).toBeVisible();
  }
  await rail.getByRole("heading", { name: "Patterns", exact: true }).click();
  const before = await snapshot(mock);
  const a = before.patterns.find((pattern) => pattern.id === 0)!;
  const b = before.patterns.find((pattern) => pattern.id === 3)!;
  for (const [shortcut, count, advance] of [["ControlOrMeta+z", 0, 1], ["ControlOrMeta+Shift+z", 1, 2]] as const) {
    await app.keyboard.press(shortcut);
    await expect.poll(async () => (await snapshot(mock)).patterns.find((pattern) => pattern.id === 3)!.notes.length).toBe(count);
    const after = await snapshot(mock);
    expect(after.patterns.find((pattern) => pattern.id === 0)).toEqual(a);
    expect(after.patterns.find((pattern) => pattern.id === 3)!.revision).toBe(b.revision + advance);
    expect(after.selected).toBe(0);
    expect(after.generation).toBe(before.generation);
    expect(after.revision).toBe(before.revision + advance);
    expect(after.dirty).toBe(true);
    await expect(rail.getByRole("button", { name: aName, exact: true })).toHaveAttribute("aria-current", "true");
  }
  expect((await snapshot(mock)).patterns.find((pattern) => pattern.id === 3)!.notes).toEqual(b.notes);
  const actual = await mock.eval<{ command: string; effect?: unknown }[]>("(m) => m.workspaceTrace()");
  expect(actual.filter((call) => call.command === "arrangement_history").map((call) => call.effect))
    .toEqual([{ kind: "notes", pattern: 3 }, { kind: "notes", pattern: 3 }]);
  expect(await mock.calls("notes_undo")).toHaveLength(0);
  expect(await mock.calls("notes_redo")).toHaveLength(0);
});
test("T9 legacy history stays selected-pattern scoped and rejects cross-pattern or replaced stamps", async ({ app, mock }) => {
  await note(app, 0);
  await select(app, 3);
  await note(app, 2);
  await select(app, 0);
  const base = await patterns(app);
  const before = await snapshot(mock);
  const inactive = before.patterns.find((pattern) => pattern.id === 3)!;
  const stamp = { generation: base.generation, patternId: 0, revision: base.view.notesRevision };
  await expect(ipc(app, "notes_undo", { stamp: { ...stamp, patternId: 3, revision: inactive.revision } })).rejects.toBeTruthy();
  expect(await snapshot(mock)).toEqual(before);
  await ipc(app, "notes_undo", { stamp });
  const undone = await snapshot(mock);
  expect(undone.patterns.find((pattern) => pattern.id === 0)!.notes).toEqual([]);
  expect(undone.patterns.find((pattern) => pattern.id === 3)).toEqual(inactive);
  expect(undone.selected).toBe(0);
  const next = await patterns(app);
  await ipc(app, "notes_redo", { stamp: { generation: next.generation, patternId: 0, revision: next.view.notesRevision } });
  expect((await snapshot(mock)).patterns.find((pattern) => pattern.id === 0)!.notes)
    .toEqual(before.patterns.find((pattern) => pattern.id === 0)!.notes);
  const globalStamp = await arrangement(app);
  await ipc(app, "project_new");
  const replaced = await snapshot(mock);
  await expect(ipc(app, "notes_undo", { stamp })).rejects.toBeTruthy();
  const refused = await ipc<ProjectHistoryReply>(app, "arrangement_history", {
    generation: globalStamp.generation, revision: globalStamp.revision, undo: true,
  });
  expect(refused.status).toBe("refused");
  expect(await snapshot(mock)).toEqual(replaced);
});
test("T9 picker selection cancels the held gesture before switching its native pattern", async ({ app, mock }) => {
  const grid = app.getByLabel("Step grid");
  const box = (await grid.boundingBox())!;
  const edits = (await mock.calls("project_edit")).length;
  await app.mouse.move(box.x + 18, box.y + 18);
  await app.mouse.down();
  const b = app.getByRole("complementary", { name: "Patterns" }).getByRole("button", { name: "B", exact: true });
  await b.focus();
  await b.press("Enter");
  await expect.poll(async () => (await patterns(app)).selected).toBe(3);
  await app.mouse.up();
  expect(await mock.calls("project_edit")).toHaveLength(edits);
  expect((await snapshot(mock)).patterns.every((pattern) => pattern.notes.length === 0)).toBe(true);
  await clickCell(app, 0, 3);
  await expect.poll(async () => (await snapshot(mock)).patterns.find((pattern) => pattern.id === 3)!.notes.length).toBe(1);
  expect((await snapshot(mock)).patterns.find((pattern) => pattern.id === 0)!.notes).toEqual([]);
});

test("T9 reduced: native notes/trim order and inactive target preserve canonical selection", async ({ app, mock }) => {
  const observations: { reply: ProjectHistoryReply; state: unknown }[] = [];
  const step = async (undo: boolean) => { const reply = await history(app,undo); observations.push({ reply, state: await mock.eval("(m) => m.workspaceSnapshot()") }); return reply; };
  await note(app, 0); const repaired = await reset(app); expect(repaired.status).toBe("applied");
  await select(app, 3); await note(app, 2); await select(app, 0);
  const selectedRevision = (await patterns(app)).view.notesRevision;
  expect(effect(await step(true))).toEqual({ kind: "notes", pattern: 3 });
  expect((await patterns(app)).selected).toBe(0); expect((await patterns(app)).view.notesRevision).toBe(selectedRevision);
  expect(effect(await step(true))).toEqual({ kind: "samplerTrim", channel: 2 });
  expect(effect(await step(true))).toEqual({ kind: "notes", pattern: 0 });
  for (const expected of [{ kind: "notes", pattern: 0 }, { kind: "samplerTrim", channel: 2 }, { kind: "notes", pattern: 3 }]) expect(effect(await step(false))).toEqual(expected);
  // Full opaque identities remain in this observation; future native parity compares bijections.
  const state = await mock.eval<{selected:number;patterns:{notes:unknown[]}[]}>("(m) => m.workspaceSnapshot()");
  expect(state.selected).toBe(0); expect(state.patterns.reduce((count,pattern) => count+pattern.notes.length,0)).toBe(2);
  expect(observations).toHaveLength(6);
  await test.info().attach("m3-workspace-parity-trace", { body: JSON.stringify(observations), contentType: "application/json" });
});
test("T9 reduced: real picker create duplicate rename select and refusal keep native state", async ({ app, mock }) => {
  const rail = app.getByRole("complementary", { name: "Patterns" });
  await rail.getByRole("button", { name: "New", exact: true }).click();
  await expect(rail.getByRole("button", { name: "New pattern", exact: true })).toBeVisible();
  await rail.getByRole("button", { name: "New pattern", exact: true }).click();
  await rail.getByRole("button", { name: "Rename", exact: true }).click();
  await rail.getByRole("textbox", { name: "Pattern name" }).fill("Melody");
  await rail.getByRole("button", { name: "Save name", exact: true }).click();
  await expect(rail.getByRole("button", { name: "Melody", exact: true })).toHaveAttribute("aria-current", "true");
  const before = await patterns(app); await mock.eval("(m) => m.refuseNext('pattern_edit','busy')");
  await rail.getByRole("button", { name: "Duplicate", exact: true }).click();
  await expect(rail.getByRole("alert")).toContainText("busy"); expect((await patterns(app)).selected).toBe(before.selected);
  await rail.getByRole("button", { name: "Duplicate", exact: true }).click();
  await expect.poll(async () => (await patterns(app)).patterns.length).toBe(before.patterns.length + 1);
  expect(await mock.calls("transport_play")).toHaveLength(0);
});
test("T9 reduced: no-op/read/clean save and native mode persistence do not invent stamps or Play", async ({ app, mock }) => {
  const initial = await arrangement(app); expect(initial.playMode).toBe("pattern"); expect(initial.tracks).toEqual([]);
  expect(effect(await history(app, true))).toEqual({ kind: "empty" });
  const noop = await reset(app, 1); expect(noop.status === "applied" && noop.changed).toBe(false);
  const same = await ipc<ArrangementReply>(app, "arrangement_mode", { generation: initial.generation, revision: initial.revision, mode: "pattern" }); expect(same.changed).toBe(false);
  expect((await arrangement(app)).revision).toBe(initial.revision);
  const mode = app.getByRole("combobox", { name: "Playback mode" }); await mode.selectOption("song");
  await expect(mode).toHaveValue("song");
  const changed = await arrangement(app); expect(changed.revision).toBe(initial.revision + 1);
  await app.keyboard.press("ControlOrMeta+s"); await expect.poll(async () => (await mock.view()).dirty).toBe(false);
  expect((await arrangement(app)).revision).toBe(changed.revision);
  await mock.eval(`(m) => m.queueDialog('${file}')`); await app.keyboard.press("ControlOrMeta+o");
  await expect.poll(async () => (await arrangement(app)).generation).toBe(initial.generation + 1);
  await expect(mode).toHaveValue("song"); expect((await arrangement(app)).revision).toBe(0);
  expect(await mock.calls("transport_play")).toHaveLength(0);
});
test("T9 reduced: history admission refusal preserves real stacks, values, revision and capabilities", async ({ app, mock }) => {
  await reset(app); const before = await mock.eval("(m) => m.workspaceSnapshot()");
  await mock.eval("(m) => m.refuseWorkspaceNext('generated admission refusal')");
  const refused = await history(app, true); expect(refused.status).toBe("refused");
  expect(await mock.eval("(m) => m.workspaceSnapshot()")).toEqual(before);
  expect(effect(await history(app, true))).toEqual({ kind: "samplerTrim", channel: 2 });
});
for (const refused of [false, true]) test(`T9 mode transition fences new gestures and releases after refusal ${refused}`, async ({ app, mock }) => {
  const mode = app.getByRole("combobox", { name: "Playback mode" });
  const before = (await mock.calls("project_edit")).length;
  // A local tempo draft must enter the session queue before the mode's revision is read.
  await app.getByRole("textbox", { name: "BPM" }).fill("137");
  expect((await mock.view()).bpm).toBe(120);
  await mock.eval("(m) => m.holdCommand('arrangement_mode')");
  if (refused) await mock.eval("(m) => m.refuseWorkspaceNext('generated mode admission refusal')");
  await mode.selectOption("song");
  await expect.poll(async () => (await mock.calls("arrangement_mode")).length).toBe(1);
  const edits = (await mock.calls("project_edit")).length;
  expect(edits).toBe(before + 1);
  expect((await mock.view()).bpm).toBe(137);
  const order = (await mock.calls()).filter((call) => call.cmd === "project_edit" || call.cmd === "arrangement_mode");
  expect(order.slice(-2).map((call) => call.cmd)).toEqual(["project_edit", "arrangement_mode"]);
  expect(order.at(-2)?.args.edit).toEqual({ kind: "setTempo", bpm: 137 });
  await clickCell(app, 0, 4);
  expect(await mock.calls("project_edit")).toHaveLength(edits);
  await mock.eval("(m) => m.releaseCommand('arrangement_mode')");
  await expect(mode).toBeEnabled();
  await expect(mode).toHaveValue(refused ? "pattern" : "song");
  if (refused) await expect(app.getByRole("status", { name: "Messages" })).toContainText("generated mode admission refusal");
  await clickCell(app, 0, 4);
  await expect.poll(async () => (await mock.calls("project_edit")).length).toBe(edits + 1);
  expect((await patterns(app)).view.pattern.cells["1"][4]).not.toBeNull();
  expect(await mock.calls("transport_play")).toHaveLength(0);
});
test("T9 reduced: dedicated/global inverses retain optional values, order and identity relations", async ({ app }) => {
  const before = await ipc<TrimState>(app, "sampler_trim_view", { channel: 2 });
  const resetReply = await reset(app); expect(resetReply.status).toBe("applied"); if (resetReply.status !== "applied") throw new Error(resetReply.message);
  const accepted = resetReply.state;
  await ipc(app, "sampler_trim_undo", { channel: 2, stamp: accepted.stamp, token: accepted.undo });
  const undone = await ipc<TrimState>(app, "sampler_trim_view", { channel: 2 });
  expect([undone.start,undone.end]).toEqual([before.start,before.end]); expect(undone.stamp).not.toBe(accepted.stamp); expect(undone.undo).toBeNull();
  expect(effect(await history(app, false))).toEqual({ kind: "samplerTrim", channel: 2 });
  const redone = await ipc<TrimState>(app, "sampler_trim_view", { channel: 2 });
  expect(redone.undo).toBe(accepted.undo); expect(redone.stamp).not.toBe(undone.stamp);
  const stale = await ipc<TrimReply>(app, "sampler_trim_undo", { channel: 2, stamp: accepted.stamp, token: accepted.undo }); expect(stale.status).toBe("refused");
});
test("T9 reduced: observation and unrelated channel preserve identity; away-back/relink retire it", async ({ app }) => {
  const a = await ipc<TrimState>(app, "sampler_trim_view", { channel: 1 }); await reset(app);
  const b = await ipc<TrimState>(app, "sampler_trim_view", { channel: 2 });
  expect(await ipc(app, "sampler_trim_view", { channel: 1 })).toEqual(a);
  expect(await ipc(app, "sampler_trim_view", { channel: 2 })).toEqual(b);
  for (const value of [0.2,0]) { const base=await patterns(app); await ipc(app, "project_edit", { generation:base.generation, edit:{kind:"setInstrumentParam",channel:2,param:"start",value} }); }
  const next = await ipc<TrimState>(app, "sampler_trim_view", { channel: 2 }); expect(next.stamp).not.toBe(b.stamp); expect(next.undo).toBeNull();
  expect(effect(await history(app, true))).toEqual({ kind: "empty" });
  const base = await patterns(app); await ipc(app, "channel_set_sample", { channel:2, path:"/qa/sounds/B.wav", generation:base.generation });
  expect((await ipc<TrimState>(app, "sampler_trim_view", { channel:2 })).stamp).not.toBe(next.stamp);
});
test("T9 reduced: captured reply after replacement returns original result once; admission hold refuses", async ({ app, mock }) => {
  await note(app,0); await mock.eval("(m) => m.holdReply('arrangement_history')");
  const pending = history(app,true);
  await expect.poll(async () => (await mock.eval<{command:string}[]>("(m) => m.workspaceTrace()")).filter((item) => item.command === "arrangement_history").length).toBe(1);
  const oldGeneration = (await patterns(app)).generation;
  await ipc(app,"project_new"); await mock.eval("(m) => m.releaseReply('arrangement_history')");
  const old = await pending; expect(old.generation).toBe(oldGeneration); expect((await patterns(app)).generation).toBe(oldGeneration+1);
  expect((await mock.calls("arrangement_history"))).toHaveLength(1);
  const stamp=await arrangement(app); await mock.eval("(m) => m.holdCommand('arrangement_history')");
  const admission=ipc<ProjectHistoryReply>(app,"arrangement_history",{generation:stamp.generation,revision:stamp.revision,undo:true});
  await expect.poll(async () => (await mock.calls("arrangement_history")).length).toBe(2);
  await ipc(app,"project_new"); await mock.eval("(m) => m.releaseCommand('arrangement_history')");
  expect((await admission).status).toBe("refused");
});
for (const roll of [false,true]) test(`T9 reduced: contextual text and queued native Undo with roll ${roll}`, async ({ app, mock }) => {
  await clickCell(app,0,0);
  if (roll) { await app.keyboard.press("F7"); await expect(app.getByLabel("Piano roll notes")).toBeVisible(); }
  const rail=app.getByRole("complementary",{name:"Patterns"}); await rail.getByRole("button",{name:"Rename",exact:true}).click();
  const input=rail.getByRole("textbox",{name:"Pattern name"}); await input.fill("Text edits");
  const before=(await mock.calls("arrangement_history")).length; await input.press("ControlOrMeta+z");
  expect((await mock.calls("arrangement_history")).length).toBe(before);
  await input.press("Escape"); await app.getByRole("heading",{name:"Patterns",exact:true}).click();
  await mock.eval("(m) => m.holdCommand('arrangement_history')"); await app.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await mock.calls("arrangement_history")).length).toBe(before+1);
  await app.keyboard.press("ControlOrMeta+z"); expect((await mock.calls("arrangement_history")).length).toBe(before+1);
  await mock.eval("(m) => m.releaseCommand('arrangement_history')");
  await expect.poll(async () => (await mock.calls("arrangement_history")).length).toBe(before+2);
});
test("T9 reduced: F5 outer shell preserves F6/F7 and modified/text key lifetime", async ({ app }) => {
  await app.keyboard.press("F5"); await expect(app.getByRole("region",{name:"Playlist",exact:true})).toBeVisible();
  await app.keyboard.press("Shift+F5"); await expect(app.getByRole("region",{name:"Playlist",exact:true})).toBeVisible();
  await app.keyboard.press("F6"); await expect(app.getByRole("region",{name:"Channel rack"})).toBeHidden();
  await app.keyboard.press("F6"); await expect(app.getByRole("region",{name:"Channel rack"})).toBeVisible();
  await app.keyboard.press("F5"); await expect(app.getByRole("region",{name:"Playlist",exact:true})).toBeHidden();
});
test("T9 F5 preserves the open roll, selected channel and requested split", async ({ app }) => {
  await app.keyboard.press("F7");
  const roll = app.getByRole("region", { name: "Piano roll", exact: true });
  const channel = app.getByRole("group", { name: "B", exact: true }).locator(".channel-name");
  await channel.focus();
  await expect(channel).toHaveAttribute("aria-current", "true");
  await expect(roll.locator("header")).toContainText("B");
  const splitter = app.getByRole("separator", { name: "Resize the channel rack and the piano roll" });
  await splitter.focus();
  await splitter.press("ArrowDown");
  const requested = await splitter.getAttribute("aria-valuenow");
  const workspace = await app.locator(".workspace").elementHandle();
  expect(workspace).not.toBeNull();
  for (const visible of [true, false, true, false]) {
    await app.keyboard.press("F5");
    await expect(app.getByRole("region", { name: "Playlist", exact: true })).toHaveCount(visible ? 1 : 0);
    // A remount would reset the chosen channel, split state and the roll's history queue.
    expect(await workspace!.evaluate((node) => node.isConnected)).toBe(true);
    await expect(roll).toBeVisible();
    await expect(channel).toHaveAttribute("aria-current", "true");
    await expect(splitter).toHaveAttribute("aria-valuenow", requested!);
  }
});
test("T9 reduced: absent original trim entries survive inverse and failed Open preserves capabilities", async ({ app, mock }) => {
  // Supply a generated file, then use the actual Open flow rather than mutate live history.
  await mock.eval(`(m) => { const p=JSON.parse(JSON.stringify(m.state.project)); delete p.channels[1].sampler.start;
    m.files.set('/qa/absent.grooviq',{kind:'project',project:p}); m.queueDialog('/qa/absent.grooviq'); }`);
  await app.keyboard.press("ControlOrMeta+o"); await expect.poll(async () => (await patterns(app)).generation).toBe(2);
  const repaired = await reset(app); expect(repaired.status).toBe("applied");
  expect(effect(await history(app,true))).toEqual({kind:"samplerTrim",channel:2});
  expect(await mock.eval("(m) => Object.hasOwn(m.state.project.channels[1].sampler,'start')")).toBe(false);
  await reset(app); const before = await mock.eval("(m) => m.workspaceSnapshot()");
  await mock.eval("(m) => m.refuseNext('project_open','generated failed Open')");
  await expect(ipc(app,"project_open",{path:file})).rejects.toBeTruthy();
  expect(await mock.eval("(m) => m.workspaceSnapshot()")).toEqual(before);
});
test("T9 reduced: channel reuse cannot accept a captured old reset/inverse", async ({ app }) => {
  const old = await reset(app); expect(old.status).toBe("applied"); if(old.status!=='applied') throw new Error(old.message);
  await ipc(app,"project_new"); const empty = await patterns(app);
  await ipc(app,"channel_add",{path:"/qa/sounds/A.wav",generation:empty.generation});
  await ipc(app,"channel_add",{path:"/qa/sounds/B.wav",generation:empty.generation});
  const current = await ipc<TrimState>(app,"sampler_trim_view",{channel:2}); expect(current.stamp).not.toBe(old.state.stamp);
  const refused = await ipc<TrimReply>(app,"sampler_trim_undo",{channel:2,stamp:old.state.stamp,token:old.state.undo});
  expect(refused.status).toBe("refused"); expect(await ipc(app,"sampler_trim_view",{channel:2})).toEqual(current);
});
test("T9 reduced: unsupported arrangement refuses rather than skip to an older note inverse", async ({ app, mock }) => {
  await note(app,0);
  await mock.eval("(m) => { const p=JSON.parse(JSON.stringify(m.state.project)); p.patternClips=[{id:99,pattern:0}]; m.files.set('/qa/unsupported.grooviq',{kind:'project',project:p}); }");
  await ipc(app,"project_open",{path:"/qa/unsupported.grooviq"});
  await expect(arrangement(app)).rejects.toBeTruthy();
  const base=await patterns(app); const before = await mock.eval("(m) => m.workspaceSnapshot()");
  const refused=await ipc<ProjectHistoryReply>(app,"arrangement_history",{generation:base.generation,revision:0,undo:true});
  expect(refused.status).toBe("refused"); expect(await mock.eval("(m) => m.workspaceSnapshot()")).toEqual(before);
});
test("T9 reduced: stale native mode/history stamp refuses without changing state or identity", async ({app,mock}) => {
  const stamp=await arrangement(app); await note(app,0); const before=await mock.eval("(m) => m.workspaceSnapshot()");
  const mode=await ipc<ArrangementReply>(app,"arrangement_mode",{generation:stamp.generation,revision:stamp.revision,mode:"song"});
  expect(mode.status).toBe("refused");
  const refused=await ipc<ProjectHistoryReply>(app,"arrangement_history",{generation:stamp.generation,revision:stamp.revision,undo:true});
  expect(refused.status).toBe("refused"); expect(await mock.eval("(m) => m.workspaceSnapshot()")).toEqual(before);
});

test("T9 reduced: absent start with nondefault end uses a real global inverse and redo", async ({ app, mock }) => {
  const initialGeneration = (await patterns(app)).generation;
  await mock.eval(`(m) => { const p=JSON.parse(JSON.stringify(m.state.project)); delete p.channels[1].sampler.start;
    p.channels[1].sampler.end=0.5; m.files.set('/qa/real-absence.grooviq',{kind:'project',project:p}); m.queueDialog('/qa/real-absence.grooviq'); }`);
  await app.keyboard.press("ControlOrMeta+o");
  await expect.poll(async () => (await patterns(app)).generation).toBe(initialGeneration+1);
  expect((await arrangement(app)).revision).toBe(0);
  const initial=await ipc<TrimState>(app,"sampler_trim_view",{channel:2});
  expect([initial.start,initial.end,initial.undo]).toEqual([0,0.5,null]);
  expect(await mock.eval("(m) => Object.hasOwn(m.state.project.channels[1].sampler,'start')")).toBe(false);
  const repaired=await reset(app); expect(repaired.status).toBe("applied");
  if(repaired.status!=="applied") throw new Error(repaired.message);
  expect(repaired.changed).toBe(true); expect(repaired.state.undo).not.toBeNull(); expect(repaired.state.stamp).not.toBe(initial.stamp);
  expect((await arrangement(app)).revision).toBe(1);
  const observations: unknown[]=[{phase:"reset",trim:repaired.state,state:await mock.eval("(m) => m.workspaceSnapshot()")}];
  const undone=await history(app,true); expect(effect(undone)).toEqual({kind:"samplerTrim",channel:2});
  observations.push({reply:undone,state:await mock.eval("(m) => m.workspaceSnapshot()")});
  expect(await mock.eval("(m) => Object.hasOwn(m.state.project.channels[1].sampler,'start')")).toBe(false);
  expect(await mock.eval("(m) => new Uint32Array(new Float32Array([m.state.project.channels[1].sampler.end]).buffer)[0]")).toBe(1056964608);
  const undoState=await ipc<TrimState>(app,"sampler_trim_view",{channel:2});
  expect(undoState.undo).toBeNull(); expect(undoState.stamp).not.toBe(repaired.state.stamp); expect((await arrangement(app)).revision).toBe(2);
  const redone=await history(app,false); expect(effect(redone)).toEqual({kind:"samplerTrim",channel:2});
  observations.push({reply:redone,state:await mock.eval("(m) => m.workspaceSnapshot()")});
  const redoState=await ipc<TrimState>(app,"sampler_trim_view",{channel:2});
  expect([redoState.start,redoState.end]).toEqual([0,1]); expect(redoState.undo).toBe(repaired.state.undo); expect(redoState.stamp).not.toBe(undoState.stamp);
  expect(await mock.eval("(m) => Object.hasOwn(m.state.project.channels[1].sampler,'start')")).toBe(true);
  expect((await arrangement(app)).revision).toBe(3); expect((await patterns(app)).generation).toBe(initialGeneration+1);
  const before=await mock.eval("(m) => m.workspaceSnapshot()");
  const refused=await ipc<TrimReply>(app,"sampler_trim_undo",{channel:2,stamp:repaired.state.stamp,token:repaired.state.undo});
  expect(refused.status).toBe("refused"); expect(await mock.eval("(m) => m.workspaceSnapshot()")).toEqual(before);
  await test.info().attach("m3-workspace-absence-trace",{body:JSON.stringify(observations),contentType:"application/json"});
});
