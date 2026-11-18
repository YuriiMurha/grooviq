import { test, expect, project, soundsFor, clickCell } from "./fixture";
import type { PatternsView, PatternReply } from "../src/api/patterns";
import type { Page } from "@playwright/test";

const kit = ["Kick"];
test.use({ seed: { project: project(kit, { 1: [0] }), files: soundsFor(kit) } });
async function invoke<T>(page: Page, command: string, args: Record<string, unknown> = {}): Promise<T> {
  return page.evaluate(async ({ command, args }) => {
    const shell = window as unknown as { __TAURI_INTERNALS__: { invoke: (name: string, args: Record<string, unknown>) => Promise<unknown> } };
    return shell.__TAURI_INTERNALS__.invoke(command, args);
  }, { command, args }) as Promise<T>;
}
const list = (page: Page) => invoke<PatternsView>(page, "pattern_list");
const edit = (page: Page, state: PatternsView, change: Record<string, unknown>) =>
  invoke<PatternReply>(page, "pattern_edit", { generation: state.generation, revision: state.revision, edit: change });

for (const roll of [false, true]) {
  test(`queued Undo and Redo advance their own revisions with the roll ${roll ? "open" : "closed"}`, async ({ app, mock }) => {
    await clickCell(app, 0, 2);
    await clickCell(app, 0, 4);
    await expect.poll(() => mock.eval("(m) => m.notes().length")).toBe(3);
    if (roll) {
      await app.keyboard.press("F7");
      await expect(app.getByLabel("Piano roll notes")).toBeVisible();
    }
    for (const [command, shortcut, remaining] of [
      ["notes_undo", "ControlOrMeta+z", 1],
      ["notes_redo", "ControlOrMeta+Shift+z", 3],
    ] as const) {
      const before = (await mock.calls(command)).length;
      await mock.eval(`(m) => m.holdCommand('${command}')`);
      await app.keyboard.press(shortcut);
      await expect.poll(async () => (await mock.calls(command)).length).toBe(before + 1);
      await app.keyboard.press(shortcut);
      expect((await mock.calls(command)).length).toBe(before + 1);
      await mock.eval(`(m) => m.releaseCommand('${command}')`);
      await expect.poll(async () => (await mock.calls(command)).length).toBe(before + 2);
      await expect.poll(() => mock.eval("(m) => m.notes().length")).toBe(remaining);
    }
  });
}

test("canonical pattern replies preserve independent notes, undo and monotonic identities", async ({ app }) => {
  const initial = await list(app);
  const duplicated = await edit(app, initial, { kind: "duplicate", pattern: initial.selected });
  expect(duplicated.status).toBe("applied");
  const b = duplicated.state.patterns.find((p) => p.id !== initial.selected)!.id;
  const selected = await edit(app, duplicated.state, { kind: "select", pattern: b });
  expect(selected.status).toBe("applied");
  const stamp = { generation: selected.state.generation, patternId: b, revision: selected.state.view.notesRevision };
  await invoke(app, "project_edit", { generation: stamp.generation, stamp, edit: { kind: "clearStep", channel: 1, step: 0 } });
  const cleared = await list(app);
  expect(cleared.view.pattern.cells["1"][0]).toBeNull();
  const back = await edit(app, cleared, { kind: "select", pattern: initial.selected });
  expect(back.state.view.pattern.cells["1"][0]).toBe(0.8);
  const again = await edit(app, back.state, { kind: "select", pattern: b });
  await invoke(app, "notes_undo", { stamp: { generation: again.state.generation, patternId: b, revision: again.state.view.notesRevision } });
  const undone = await list(app);
  expect(undone.view.pattern.cells["1"][0]).toBe(0.8);
  const refused = await edit(app, initial, { kind: "rename", pattern: initial.selected, name: "stale" });
  expect(refused.status).toBe("refused");
  if (refused.status === "refused") expect(refused.reason).toBe("staleRevision");
  expect(refused.state).toEqual(undone);
  const deleted = await edit(app, undone, { kind: "delete", pattern: b });
  const created = await edit(app, deleted.state, { kind: "create", name: "C" });
  const c = created.state.patterns.find((p) => p.name === "C")!.id;
  expect(c).toBeGreaterThan(b);
  await invoke(app, "project_save_as", { path: "/qa/projects/patterns.grooviq" });
  await invoke(app, "project_new");
  await invoke(app, "project_open", { path: "/qa/projects/patterns.grooviq" });
  const reopened = await list(app);
  expect(reopened.revision).toBe(created.state.revision);
  expect(reopened.generation).toBeGreaterThan(created.state.generation);
  expect(reopened.patterns.map((p) => [p.id,p.name])).toEqual(created.state.patterns.map((p) => [p.id,p.name]));
  expect(reopened.view.pattern.cells["1"][0]).toBe(0.8);
  const next = await edit(app, reopened, { kind:"create", name:"D" });
  expect(next.state.patterns.find((p) => p.name === "D")!.id).toBeGreaterThan(c);
});

test("a step held across selection is refused without clearing the new pattern", async ({ app, mock }) => {
  const state = await list(app);
  const created = await edit(app, state, { kind: "create", name: "B" });
  const b = created.state.patterns.find((p) => p.name === "B")!.id;
  // The real canvas captures A's stamp at pointer-down; the IPC selects B while it is held.
  const canvas = app.getByLabel("Step grid");
  const box = await canvas.boundingBox();
  expect(box).not.toBeNull();
  await app.mouse.move(box!.x + 18, box!.y + 18);
  await app.mouse.down();
  const selected = await edit(app, created.state, { kind: "select", pattern: b });
  await app.mouse.up();
  await expect.poll(async () => (await mock.calls("project_edit")).length).toBeGreaterThan(0);
  expect((await list(app)).view.pattern.cells["1"]).toEqual(Array(16).fill(null));
  expect((await list(app)).selected).toBe(selected.state.selected);
});

test("a refused queued roll edit aborts Save and Open before any file command", async ({ app, mock }) => {
  await app.keyboard.press("F7");
  await expect(app.getByLabel("Piano roll notes")).toBeVisible();
  await mock.eval("(m) => m.refuseNext('notes_edit', 'expanded song budget')");
  const canvas = app.getByLabel("Piano roll notes");
  const box = await canvas.boundingBox();
  await app.mouse.click(box!.x + 240, box!.y + box!.height / 2);
  await expect.poll(async () => (await mock.calls("notes_edit")).length).toBeGreaterThan(0);
  await mock.eval("(m) => m.queueDialog('/qa/projects/refused')");
  await app.keyboard.press("ControlOrMeta+s");
  await expect(app.getByRole("status", { name: "Messages" })).toContainText("could not be applied");
  expect(await mock.calls("project_save_as")).toHaveLength(0);
  expect(await mock.calls("project_open")).toHaveLength(0);
  // A second refused gesture must also abort an actual Open attempt, including Don't save.
  await mock.eval("(m) => { m.refuseNext('notes_edit', 'expanded song budget'); m.files.set('/qa/projects/next.grooviq', { kind:'project', project:JSON.parse(JSON.stringify(m.state.project)) }); }");
  await app.mouse.click(box!.x + 280, box!.y + box!.height / 2);
  await expect.poll(async () => (await mock.calls("notes_edit")).length).toBeGreaterThan(1);
  await mock.eval("(m) => m.queueDialog('/qa/projects/next.grooviq')");
  await app.keyboard.press("ControlOrMeta+o");
  const discard = app.getByRole("button", { name: "Don't save", exact: true });
  if (await discard.isVisible()) await discard.click();
  await expect(app.getByRole("status", { name: "Messages" })).toContainText("could not be applied");
  expect(await mock.calls("project_open")).toHaveLength(0);
});
