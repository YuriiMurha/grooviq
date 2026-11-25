import type { Page } from "@playwright/test";
import type { ArrangementOperation, ArrangementPage } from "../src/api/arrangement";
import type { ProjectView } from "../src/api/project";
import { expect, project, soundsFor, test, type Mock } from "./fixture";

const content = { type: "pattern" as const, pattern: 0, offsetTick: 480, lengthTicks: 3840 };
const clips = [
  { id: 3, track: 2, name: "Kick in", startTick: 960, muted: false, content },
  { id: 4, track: 2, name: "Kick out", startTick: 5760, muted: true, content },
  { id: 5, track: 2, name: "Other clip", startTick: 19_200, muted: false, content },
];
const tracks = [{ id: 2, name: "Track 1", color: 0, mute: false }];
test.use({ seed: { project: project(["Kick"], { 1: [0, 4, 8, 12] }, {
  nextId: 6, arrangement: { tracks, clips, audioAssets: [], loopRegion: null },
}), files: soundsFor(["Kick"]) } });

function ipc<T>(page: Page, command: string, args: Record<string, unknown> = {}): Promise<T> {
  return page.evaluate(({ command, args }) => (window as unknown as {
    __TAURI_INTERNALS__: { invoke: (command: string, args: Record<string, unknown>) => Promise<unknown> }
  }).__TAURI_INTERNALS__.invoke(command, args), { command, args }) as Promise<T>;
}
const state = (page: Page) => ipc<ArrangementPage>(page, "arrangement_view", { offset: 0, limit: 256 });
const view = (page: Page) => ipc<ProjectView>(page, "project_view");

async function selectGroup(page: Page) {
  await expect(page.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await page.keyboard.press("F5");
  const playlist = page.getByRole("region", { name: "Playlist", exact: true });
  const canvas = playlist.getByRole("application", { name: "Playlist clips" });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  const box = await canvas.boundingBox();
  if (!box) throw new Error("Missing playlist canvas");
  await playlist.getByRole("button", { name: "Select", exact: true }).click();
  await page.mouse.click(box.x + 380, box.y + 40);
  await page.keyboard.down("Shift");
  await page.mouse.click(box.x + 140, box.y + 40);
  await page.keyboard.up("Shift");
  const selected = playlist.getByText(/^[0-9]+ selected$/);
  await expect(selected).toHaveText("2 selected");
  const before = await state(page);
  expect(before.clips).toEqual(clips);
  expect(await view(page)).toMatchObject({ projectCanUndo: false, projectCanRedo: false, dirty: false });
  return { playlist, canvas, selected, before };
}

const cases = [
  {
    action: "delete",
    operations: [{ kind: "deleteClip", clip: 3 }, { kind: "deleteClip", clip: 4 }],
    after: [clips[2]],
    selectedCount: 0,
  },
  {
    action: "mute",
    operations: [
      { kind: "setClip", clip: 3, name: "Kick in", muted: true },
      { kind: "setClip", clip: 4, name: "Kick out", muted: true },
    ],
    after: [{ ...clips[0], muted: true }, clips[1], clips[2]],
    selectedCount: 2,
  },
  {
    action: "duplicate",
    operations: [
      { kind: "copy", clip: 3, track: 2, startTick: 9600 },
      { kind: "copy", clip: 4, track: 2, startTick: 14_400 },
    ],
    after: [...clips, { ...clips[0], id: 6, startTick: 9600 }, { ...clips[1], id: 7, startTick: 14_400 }],
    selectedCount: 2,
  },
] satisfies { action: string; operations: ArrangementOperation[]; after: typeof clips; selectedCount: number }[];

async function invokeAction(group: Awaited<ReturnType<typeof selectGroup>>, action: string) {
  if (action === "delete") await group.canvas.press("Delete");
  else await group.playlist.getByRole("button", { name: action === "mute" ? "Mute selected clips" : "Duplicate clips", exact: true }).click();
}

function editArgs(before: ArrangementPage, operations: ArrangementOperation[]) {
  return [{ generation: before.generation, revision: before.revision, editsJson: JSON.stringify(operations) }];
}
const argsFor = async (mock: Mock, command: string) => (await mock.calls(command)).map(({ args }) => args);
const savedArrangement = async (mock: Mock, path: string) => {
  const saved = await mock.eval<{ project: { arrangement: { clips: typeof clips; tracks: typeof tracks } } } | null>(
    `m => m.file(${JSON.stringify(path)})`,
  );
  return saved?.project.arrangement ?? null;
};

for (const { action, operations, after, selectedCount } of cases) {
  test(`group ${action} sends one ordered batch and one Undo restores the group`, async ({ app, mock }) => {
    const group = await selectGroup(app);
    await invokeAction(group, action);
    await expect.poll(async () => (await state(app)).clips).toEqual(after);
    await expect(group.canvas).toHaveAttribute("aria-disabled", "false");
    await expect(group.selected).toHaveText(`${selectedCount} selected`);
    const applied = await state(app);

    await app.keyboard.press("ControlOrMeta+z");
    await expect.poll(async () => (await state(app)).clips).toEqual(group.before.clips);
    await expect(group.canvas).toHaveAttribute("aria-disabled", "false");
    await expect(group.selected).toHaveText("2 selected");
    expect((await state(app)).tracks).toEqual(tracks);
    expect(await view(app)).toMatchObject({ projectCanUndo: false, projectCanRedo: true });
    expect(await argsFor(mock, "arrangement_edit")).toEqual(editArgs(group.before, operations));
    const undoCalls = (await argsFor(mock, "arrangement_history")).map(({ generation, revision, undo }) => ({ generation, revision, undo }));
    expect(undoCalls).toEqual([
      { generation: applied.generation, revision: applied.revision, undo: true },
    ]);
  });

  test(`queued Save includes the entire accepted group ${action}`, async ({ app, mock }) => {
    const group = await selectGroup(app);
    const path = "/qa/projects/accepted-group.grooviq";
    await mock.eval(`m => { m.holdCommand("arrangement_edit"); m.queueDialog(${JSON.stringify(path)}); }`);
    await invokeAction(group, action);
    await expect.poll(() => argsFor(mock, "arrangement_edit")).toEqual(editArgs(group.before, operations));
    await app.keyboard.press("ControlOrMeta+s");
    await expect.poll(async () => (await mock.calls("plugin:dialog|save")).length).toBe(1);
    expect((await state(app)).clips).toEqual(group.before.clips);

    await mock.eval('m => m.releaseCommand("arrangement_edit")');
    await expect.poll(async () => (await view(app)).path).toBe(path);
    expect(await savedArrangement(mock, path)).toMatchObject({ tracks, clips: after });
    expect((await state(app)).clips).toEqual(after);
    expect(await argsFor(mock, "arrangement_edit")).toEqual(editArgs(group.before, operations));
    expect(await argsFor(mock, "project_save_as")).toEqual([{ path }]);
    expect(await argsFor(mock, "project_save")).toEqual([]);
  });

  test(`refused group ${action} leaves the arrangement intact and vetoes the queued Save`, async ({ app, mock }) => {
    const group = await selectGroup(app);
    const refusedPath = "/qa/projects/refused-group.grooviq";
    const recoveryPath = "/qa/projects/recovered-group.grooviq";
    await mock.eval(`m => { m.holdCommand("arrangement_edit"); m.refuseWorkspaceNext("Generated group refusal"); m.queueDialog(${JSON.stringify(refusedPath)}); }`);
    await invokeAction(group, action);
    await expect.poll(() => argsFor(mock, "arrangement_edit")).toEqual(editArgs(group.before, operations));
    await app.keyboard.press("ControlOrMeta+s");
    await expect.poll(async () => (await mock.calls("plugin:dialog|save")).length).toBe(1);
    await mock.eval('m => m.releaseCommand("arrangement_edit")');
    await expect(group.playlist.getByRole("alert")).toContainText("Generated group refusal");
    await expect(app.getByText(/the project was not saved/)).toBeVisible();
    expect(await state(app)).toEqual(group.before);
    expect(await view(app)).toMatchObject({ projectCanUndo: false, projectCanRedo: false, dirty: false });
    await expect(group.selected).toHaveText("2 selected");

    // Complete a later save through the same queue before asserting the first never wrote.
    await group.playlist.getByRole("button", { name: "Refresh playlist" }).click();
    await expect(group.canvas).toHaveAttribute("aria-disabled", "false");
    await mock.eval(`m => m.queueDialog(${JSON.stringify(recoveryPath)})`);
    await app.keyboard.press("ControlOrMeta+s");
    await expect.poll(async () => (await view(app)).path).toBe(recoveryPath);
    expect(await savedArrangement(mock, recoveryPath)).toMatchObject({ tracks, clips });
    expect(await savedArrangement(mock, refusedPath)).toBeNull();
    expect(await argsFor(mock, "arrangement_edit")).toEqual(editArgs(group.before, operations));
    expect(await argsFor(mock, "project_save_as")).toEqual([{ path: recoveryPath }]);
    expect(await argsFor(mock, "project_save")).toEqual([]);
  });
}
