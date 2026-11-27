import type { Locator, Page } from "@playwright/test";
import type { ArrangementOperation, ArrangementPage, ArrangementReply, AutomationContent, CurvePoint } from "../src/api/arrangement";
import { expect, project, soundsFor, test } from "./fixture";

const source: readonly CurvePoint[] = [
  { tick: 0, value: -1, mode: "linear" },
  { tick: 480, value: 0, mode: "linear" },
  { tick: 1440, value: 0.5, mode: "hold" },
  { tick: 2160, value: -0.5, mode: "linear" },
];
const content: AutomationContent = {
  type: "automation", target: { type: "channelPan", channel: 1 }, priority: 6,
  offsetTick: 240, lengthTicks: 1920, points: source,
};
const primary = { id: 3, track: 2, name: "Pan sweep", startTick: 0, muted: false, content };
const arrangement = {
  tracks: [{ id: 2, name: "Automation", color: 1, mute: false }],
  clips: [primary], audioAssets: [], loopRegion: null,
};
const scene = project(["Kick"], {}, { nextId: 8, arrangement });
const otherPath = "/qa/projects/Replacement.grooviq";
const otherPoints: CurvePoint[] = [{ tick: 1000, value: -0.25, mode: "hold" }];
const replacement = { ...scene, name: "Replacement", arrangement: {
  ...arrangement, clips: [{ ...primary, content: { ...content, points: otherPoints } }],
} };

test.use({ seed: { project: scene, files: { ...soundsFor(["Kick"]), [otherPath]: { kind: "project", project: replacement } } } });

function invoke<T>(page: Page, command: string, args: Record<string, unknown>) {
  return page.evaluate(({ command, args }) => {
    const { invoke } = (window as unknown as {
      __TAURI_INTERNALS__: { invoke: (command: string, args: Record<string, unknown>) => Promise<unknown> };
    }).__TAURI_INTERNALS__;
    return invoke(command, args);
  }, { command, args }) as Promise<T>;
}

const readArrangement = (page: Page) => invoke<ArrangementPage>(page, "arrangement_view", { offset: 0, limit: 256 });

async function readPoints(page: Page, clip = 3) {
  const { generation, revision } = await readArrangement(page);
  return (await invoke<{ points: CurvePoint[] }>(page, "arrangement_curve", { generation, revision, clip })).points;
}

async function rawEdit(page: Page, operation: ArrangementOperation) {
  const { generation, revision } = await readArrangement(page);
  return invoke<ArrangementReply>(page, "arrangement_edit", {
    generation, revision, editsJson: JSON.stringify([operation]),
  });
}

async function holdInitialCurve(page: Page) {
  // Hold a successful old reply, after the mock has evaluated it. The fixture's
  // command hold runs before evaluation, which would only exercise a rejected read.
  const { generation, revision } = await readArrangement(page);
  await page.evaluate(stamp => {
    const ipc = (window as unknown as { __TAURI_INTERNALS__: {
      invoke: (command: string, args?: Record<string, unknown>) => Promise<unknown>;
    } }).__TAURI_INTERNALS__;
    const invoke = ipc.invoke;
    let release = () => {};
    const held = new Promise<void>(resolve => { release = resolve; });
    Object.assign(window, { releaseOldCurve: release });
    ipc.invoke = async (command, args) => {
      const result = await invoke(command, args);
      if (command === "arrangement_curve" && args?.generation === stamp.generation && args.revision === stamp.revision) await held;
      return result;
    };
  }, { generation, revision });
  return () => page.evaluate(async () => {
    Reflect.get(window, "releaseOldCurve")();
    await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
  });
}

async function openDock(app: Page) {
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await app.keyboard.press("F5");
  const playlist = app.getByRole("region", { name: "Playlist", exact: true });
  const canvas = playlist.getByRole("application", { name: "Playlist clips" });
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  const select = async (name = "Pan sweep · Track 1 · 0 ticks") => {
    await playlist.getByRole("button", { name: /^Selected playlist clip:/ }).click();
    await playlist.getByRole("listbox", { name: "Selected playlist clip" }).getByRole("option", { name, exact: true }).click();
  };
  const dock = playlist.getByRole("region", { name: "Automation point editor" });
  return { playlist, canvas, select, dock, graph: dock.getByRole("group", { name: "Automation curve" }) };
}

async function centre(locator: Locator) {
  await locator.scrollIntoViewIfNeeded();
  const box = await locator.boundingBox();
  if (!box) throw new Error("Automation control is outside the viewport");
  return { x: box.x + box.width / 2, y: box.y + box.height / 2, box };
}

test("edits a trimmed linked curve with one history entry per gesture and saves it", async ({ app, mock }) => {
  await app.setViewportSize({ width: 900, height: 560 });
  const { playlist, canvas, select, dock, graph } = await openDock(app);
  // Loading a file decodes independent curves. Create the link via the native Copy contract.
  const copy = await rawEdit(app, { kind: "copy", clip: 3, track: 2, startTick: 3840 });
  expect(copy.status).toBe("applied");
  expect(copy.createdIds).toHaveLength(1);
  await playlist.getByRole("button", { name: "Refresh playlist" }).click();
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await select();
  await expect(dock).toBeVisible();
  await expect(graph).toHaveAttribute("aria-disabled", "false");
  await expect(dock.getByText("100% right", { exact: true })).toBeVisible();

  const edits = async () => (await mock.calls("arrangement_edit")).slice(1);
  const settle = async (points: readonly CurvePoint[], count: number) => {
    await expect.poll(() => readPoints(app)).toEqual(points);
    await expect(graph).toHaveAttribute("aria-disabled", "false");
    expect(await edits()).toHaveLength(count);
  };
  const [before, first, second, after] = source;
  const active = graph.getByRole("button", { name: /^Point at tick 480,/ });
  const interpolation = dock.getByRole("combobox", { name: "Interpolation" });

  const initialStamp = await readArrangement(app);
  const middle = await centre(graph);
  await app.mouse.dblclick(middle.x, middle.y);
  // The midpoint lies in the linear segment, and the graph midpoint is centre pan.
  await expect.poll(async () => (await readPoints(app)).length).toBe(5);
  const added = (await readPoints(app))[2];
  expect(Math.abs(added.tick - 1200)).toBeLessThanOrEqual(2);
  expect(added.value).toBeCloseTo(0, 1);
  expect(added.mode).toBe("linear");
  await settle([before, first, added, second, after], 1);
  expect((await edits())[0].args).toEqual({
    generation: initialStamp.generation, revision: initialStamp.revision,
    editsJson: JSON.stringify([{ kind: "setAutomationPoints", clip: 3, points: [first, added, second] }]),
  });
  expect(await readPoints(app, copy.createdIds[0])).toEqual([before, first, added, second, after]);

  const { x, y } = await centre(active);
  await app.mouse.move(x, y); await app.mouse.down();
  await app.mouse.move(x + 44, y - 14, { steps: 5 });
  await expect(active).toHaveCount(0);
  expect(await edits()).toHaveLength(1);
  await app.keyboard.press("Escape");
  await app.mouse.up();
  await expect(graph).toBeFocused();
  await expect(active).toBeVisible();
  await settle([before, first, added, second, after], 1);

  await app.mouse.move(x, y); await app.mouse.down();
  await app.mouse.move(x + 48, y - 15, { steps: 8 });
  expect(await edits()).toHaveLength(1);
  await app.mouse.up();
  await expect.poll(async () => (await readPoints(app))[1].tick).toBeGreaterThan(first.tick);
  const moved = (await readPoints(app))[1];
  expect(moved.value).toBeGreaterThan(first.value);
  await settle([before, moved, added, second, after], 2);
  await graph.press("ControlOrMeta+z");
  await settle([before, first, added, second, after], 2);
  await graph.press("ControlOrMeta+Shift+z");
  await settle([before, moved, added, second, after], 2);

  const workspaceHeight = () => app.locator(".workspace-viewport").evaluate(element => element.clientHeight);
  const readyHeight = await workspaceHeight();
  expect(readyHeight).toBeGreaterThan(0);
  await mock.eval('m => m.holdCommand("arrangement_edit")');
  try {
    await interpolation.selectOption("hold");
    await expect(playlist.locator(".playlist-status").getByRole("status")).toHaveText("Updating playlist…");
    await expect.poll(workspaceHeight).toBe(readyHeight);
  } finally {
    await mock.eval('m => m.releaseCommand("arrangement_edit")');
  }
  const held = { ...moved, mode: "hold" } as const;
  await settle([before, held, added, second, after], 3);
  await interpolation.selectOption("linear");
  await settle([before, moved, added, second, after], 4);
  await graph.press("Delete");
  const saved = [before, added, second, after];
  await settle(saved, 5);

  const path = "/qa/projects/Pan sweep.grooviq";
  await mock.eval(`m => m.queueDialog(${JSON.stringify(path)}, ${JSON.stringify(path)})`);
  await app.keyboard.press("ControlOrMeta+s");
  await expect.poll(async () => (await mock.view()).path).toBe(path);
  await app.keyboard.press("ControlOrMeta+o");
  await expect.poll(async () => (await mock.view()).generation).toBe(initialStamp.generation + 1);
  await expect(dock).toHaveCount(0);
  await select();
  await expect(graph).toHaveAttribute("aria-disabled", "false");
  expect(await readPoints(app)).toEqual(saved);
  expect(await readPoints(app, copy.createdIds[0])).toEqual(saved);
  await expect(graph.getByRole("button", { name: new RegExp(`^Point at tick ${added.tick},`) })).toBeVisible();
  expect(await edits()).toHaveLength(5);
});

test("drops a late successful curve read after project replacement reuses the clip ID", async ({ app, mock }) => {
  const { select, dock, graph, canvas } = await openDock(app);
  const release = await holdInitialCurve(app);
  await select();
  await expect.poll(async () => (await mock.calls("arrangement_curve")).length).toBe(1);
  await expect(graph).toHaveAttribute("aria-disabled", "true");
  await mock.eval(`m => m.queueDialog(${JSON.stringify(otherPath)})`);
  await app.keyboard.press("ControlOrMeta+o");
  await expect.poll(async () => (await mock.view()).generation).toBe(1);
  await expect(dock).toHaveCount(0);
  await expect(canvas).toHaveAttribute("aria-disabled", "false");
  await select();
  const point = graph.getByRole("button", { name: /^Point at tick 1000,/ });
  await expect(point).toBeVisible();
  await release();
  await expect(graph.getByRole("button", { name: /^Point at tick 480,/ })).toHaveCount(0);
  await expect(point).toBeVisible();
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
});

test("a late same-clip read cannot overwrite a newer revision", async ({ app, mock }) => {
  const { playlist, select, graph } = await openDock(app);
  const release = await holdInitialCurve(app);
  await select();
  await expect.poll(async () => (await mock.calls("arrangement_curve")).length).toBe(1);
  expect((await rawEdit(app, { kind: "setAutomationPoints", clip: 3, points: otherPoints })).status).toBe("applied");
  await playlist.getByRole("button", { name: "Refresh playlist" }).click();
  const current = graph.getByRole("button", { name: /^Point at tick 1000,/ });
  await expect(current).toBeVisible();
  await release();
  await expect(current).toBeVisible();
  await expect(graph.getByRole("button", { name: /^Point at tick 480,/ })).toHaveCount(0);
  await expect(graph).toHaveAttribute("aria-disabled", "false");
});

test("a refused edit in the open project blocks Save and recovers after Refresh", async ({ app, mock }) => {
  const { playlist, select, graph } = await openDock(app);
  const path = "/qa/projects/Original pan.grooviq";
  await mock.eval(`m => m.queueDialog(${JSON.stringify(path)})`);
  await app.keyboard.press("ControlOrMeta+s");
  await expect.poll(async () => (await mock.view()).path).toBe(path);
  await select();
  const point = graph.getByRole("button", { name: /^Point at tick 480,/ });
  await expect(point).toBeVisible();

  await mock.eval(`m => {
    m.holdCommand("arrangement_edit");
    m.refuseWorkspaceNext("Generated automation edit refusal");
  }`);
  await point.press("ArrowUp");
  await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(1);
  await app.keyboard.press("ControlOrMeta+s");
  await mock.eval('m => m.releaseCommand("arrangement_edit")');
  await expect(playlist.getByRole("alert")).toContainText("Generated automation edit refusal");
  await expect(app.getByRole("status", { name: "Messages" })).toContainText("the project was not saved");
  await expect(graph).toHaveAttribute("aria-disabled", "true");
  expect(await readPoints(app)).toEqual(source);
  expect(await mock.view()).toMatchObject({ dirty: false, projectCanUndo: false });
  expect(await mock.calls("project_save")).toHaveLength(0);

  await playlist.getByRole("button", { name: "Refresh playlist" }).click();
  await expect(graph).toHaveAttribute("aria-disabled", "false");
  await point.press("ArrowUp");
  await expect.poll(async () => (await readPoints(app))[1].value).toBeGreaterThan(source[1].value);
  await expect(graph).toHaveAttribute("aria-disabled", "false");
  expect(await mock.calls("arrangement_edit")).toHaveLength(2);
  await app.keyboard.press("ControlOrMeta+s");
  await expect.poll(async () => (await mock.calls("project_save")).length).toBe(1);
  await expect.poll(async () => (await mock.view()).dirty).toBe(false);
});

test("keyboard add and selection work, and opening a modal cancels a captured drag", async ({ app, mock }) => {
  const { select, dock, graph } = await openDock(app);
  await select();
  await expect(graph).toHaveAttribute("aria-disabled", "false");
  await dock.getByRole("button", { name: "Add point", exact: true }).press("Enter");
  const inserted = { tick: 960, value: 0.25, mode: "linear" } as const;
  const expected = [source[0], source[1], inserted, source[2], source[3]];
  await expect.poll(() => readPoints(app)).toEqual(expected);
  await expect(graph).toHaveAttribute("aria-disabled", "false");

  await graph.press("End");
  await expect(graph.getByRole("button", { name: /^Point at tick 1440,/ })).toHaveAttribute("aria-pressed", "true");
  await graph.press("PageUp");
  const point = graph.getByRole("button", { name: /^Point at tick 960,/ });
  await expect(point).toHaveAttribute("aria-pressed", "true");
  const { x, y } = await centre(point);
  await app.mouse.move(x, y); await app.mouse.down();
  await app.mouse.move(x + 35, y + 10, { steps: 4 });
  await expect(point).toHaveCount(0);

  await app.keyboard.press("ControlOrMeta+o");
  const dialog = app.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await app.mouse.up();
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(point).toBeVisible();
  expect(await readPoints(app)).toEqual(expected);
  expect(await mock.calls("arrangement_edit")).toHaveLength(1);
  expect(await mock.calls("project_open")).toHaveLength(0);

  await point.click({ button: "right" });
  await expect.poll(() => readPoints(app)).toEqual(source);
  expect(await mock.calls("arrangement_edit")).toHaveLength(2);
});

test.afterEach(async ({ mock }) => {
  expect(await mock.calls("transport_play")).toHaveLength(0);
});
