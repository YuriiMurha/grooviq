import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createRef, type RefObject } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { channel, createFakeEngine, project, type FakeEngine } from "../test/fakeEngine";

const engine = vi.hoisted(() => ({ current: null as FakeEngine | null }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => engine.current!.invoke(cmd, args),
}));

import type { ProjectView } from "../api/project";
import { HintBar } from "../hint/HintBar";
import type { LiveFrame } from "../live/decode";
import { Session } from "../test/Session";
import { CELL, EDGE, FLAG_FOCUS, FLAG_PLAYHEAD, GAP, STRIDE, type GridLayout } from "./gridModel";
import type { GridRenderer } from "./palette";
import { StepGrid } from "./StepGrid";

const PITCH = CELL + GAP;
const LEFT = 10;
const TOP = 20;

/** Where the middle of a cell is, in client pixels. */
const at = (row: number, step: number) => ({
  clientX: LEFT + EDGE + step * PITCH + CELL / 2,
  clientY: TOP + EDGE + row * PITCH + CELL / 2,
});

interface Draw {
  instances: Float32Array;
  layout: GridLayout;
}

/** A renderer that only remembers what it was asked to draw. */
function recorder() {
  const draws: Draw[] = [];
  const renderers: (GridRenderer & { dispose: ReturnType<typeof vi.fn> })[] = [];
  const factory = vi.fn((): GridRenderer => {
    const r = {
      draw: (instances: Float32Array, layout: GridLayout) => {
        draws.push({ instances: instances.slice(), layout });
      },
      dispose: vi.fn(),
    };
    renderers.push(r);
    return r;
  });
  return { draws, renderers, factory };
}

/** The velocity a draw shows for a cell (-1 when empty), and its flags. */
const velocityIn = (d: Draw, row: number, step: number) =>
  d.instances[(row * d.layout.steps + step) * STRIDE + 5];
const flagsIn = (d: Draw, row: number, step: number) =>
  d.instances[(row * d.layout.steps + step) * STRIDE + 6];

// Animation frames run only when a test asks.
let frames: FrameRequestCallback[] = [];
const nextFrame = () =>
  act(() => {
    const run = frames;
    frames = [];
    for (const cb of run) cb(performance.now());
  });

const KICK = channel(1, "Kick");
const SNARE = channel(2, "Snare");

function setup(view: ProjectView, opts: { latest?: RefObject<LiveFrame | null>; renderer?: boolean } = {}) {
  engine.current = createFakeEngine(view);
  const rec = recorder();
  const latest = opts.latest ?? createRef<LiveFrame | null>();
  const errors: string[] = [];
  const { unmount } = render(
    <>
      <Session onError={(m) => errors.push(m)}>
        {(v) => (
          <StepGrid
            channels={v.channels}
            pattern={v.pattern}
            bpm={v.bpm}
            latest={latest}
            sampleRate={48000}
            createRenderer={opts.renderer === false ? undefined : rec.factory}
          />
        )}
      </Session>
      <HintBar />
    </>,
  );
  return { rec, latest, errors, unmount, engine: engine.current };
}

const grid = () => screen.findByLabelText("Step grid");

beforeEach(() => {
  frames = [];
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
    frames.push(cb);
    return frames.length;
  });
  vi.stubGlobal("cancelAnimationFrame", () => {});
  vi.spyOn(HTMLCanvasElement.prototype, "getBoundingClientRect").mockReturnValue({
    left: LEFT,
    top: TOP,
    x: LEFT,
    y: TOP,
    width: 16 * PITCH - GAP,
    height: 2 * PITCH - GAP,
    right: LEFT + 16 * PITCH - GAP,
    bottom: TOP + 2 * PITCH - GAP,
    toJSON: () => ({}),
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("StepGrid", () => {
  it("click_toggles_a_step", async () => {
    const { engine } = setup(project([KICK, SNARE]));
    const canvas = await grid();
    fireEvent.pointerDown(canvas, { ...at(1, 4), button: 0, pointerId: 1 });
    fireEvent.pointerUp(canvas, { ...at(1, 4), button: 0, pointerId: 1 });
    await waitFor(() => expect(engine.edits()).toEqual([{ kind: "toggleStep", channel: 2, step: 4 }]));
    // A click in a gap does nothing.
    const gap = { clientX: LEFT + EDGE + CELL + 1, clientY: TOP + EDGE + 5, button: 0, pointerId: 1 };
    fireEvent.pointerDown(canvas, gap);
    fireEvent.pointerUp(canvas, gap);
    await act(async () => {});
    expect(engine.edits()).toHaveLength(1);
  });

  it("the canvas is sized by its cells and the device pixel ratio", async () => {
    vi.stubGlobal("devicePixelRatio", 2);
    setup(project([KICK, SNARE]));
    const canvas = (await grid()) as HTMLCanvasElement;
    // The grid, and an edge around it for the rings and the shadow.
    const width = 16 * PITCH - GAP + 2 * EDGE;
    const height = 2 * PITCH - GAP + 2 * EDGE;
    expect(canvas.style.width).toBe(`${width}px`);
    expect(canvas.style.height).toBe(`${height}px`);
    expect(canvas.width).toBe(width * 2);
    expect(canvas.height).toBe(height * 2);
    expect(canvas.tabIndex).toBe(0);
  });

  it("right_click_clears_a_step", async () => {
    const { engine } = setup(project([KICK], { 1: [0.8] }));
    const canvas = await grid();
    // The right button's press and release do not toggle; its context menu clears.
    fireEvent.pointerDown(canvas, { ...at(0, 0), button: 2, pointerId: 1 });
    fireEvent.pointerUp(canvas, { ...at(0, 0), button: 2, pointerId: 1 });
    const menu = fireEvent.contextMenu(canvas, { ...at(0, 0), button: 2 });
    expect(menu).toBe(false); // the browser's menu is prevented
    await waitFor(() => expect(engine.edits()).toEqual([{ kind: "clearStep", channel: 1, step: 0 }]));
    // An empty step has nothing to clear.
    fireEvent.contextMenu(canvas, { ...at(0, 1), button: 2 });
    await act(async () => {});
    expect(engine.edits()).toHaveLength(1);
  });

  it("dragging_a_lit_step_up_raises_its_velocity_and_commits_once_on_release", async () => {
    const { engine, rec } = setup(project([KICK], { 1: [null, null, 0.5] }));
    const canvas = await grid();
    nextFrame();
    const start = at(0, 2);
    fireEvent.pointerDown(canvas, { ...start, button: 0, pointerId: 1 });
    fireEvent.pointerMove(canvas, { clientX: start.clientX, clientY: start.clientY - 25, pointerId: 1, buttons: 1 });
    nextFrame();
    expect(velocityIn(rec.draws.at(-1)!, 0, 2)).toBeCloseTo(0.75);
    // Moving off the cell keeps dragging the same step.
    fireEvent.pointerMove(canvas, { clientX: start.clientX + 80, clientY: start.clientY - 40, pointerId: 1, buttons: 1 });
    nextFrame();
    expect(velocityIn(rec.draws.at(-1)!, 0, 2)).toBeCloseTo(0.9);
    expect(engine.edits()).toEqual([]);

    fireEvent.pointerUp(canvas, { clientX: start.clientX + 80, clientY: start.clientY - 40, pointerId: 1 });
    await waitFor(() => expect(engine.edits()).toHaveLength(1));
    const [edit] = engine.edits();
    expect(edit).toMatchObject({ kind: "setStepVelocity", channel: 1, step: 2 });
    expect((edit as { velocity: number }).velocity).toBeCloseTo(0.9);
    // Still one edit once things settle; the drawn value stays.
    await act(async () => {});
    nextFrame();
    expect(engine.edits()).toHaveLength(1);
    expect(velocityIn(rec.draws.at(-1)!, 0, 2)).toBeCloseTo(0.9);
  });

  it("dragging an empty step does nothing until release, which adds it", async () => {
    const { engine } = setup(project([KICK]));
    const canvas = await grid();
    fireEvent.pointerDown(canvas, { ...at(0, 3), button: 0, pointerId: 1 });
    fireEvent.pointerMove(canvas, { ...at(0, 3), clientY: at(0, 3).clientY - 30, pointerId: 1, buttons: 1 });
    fireEvent.pointerUp(canvas, { ...at(0, 3), pointerId: 1 });
    await waitFor(() => expect(engine.edits()).toEqual([{ kind: "toggleStep", channel: 1, step: 3 }]));
  });

  it("enter_toggles_the_focused_step_and_delete_clears_it", async () => {
    const { engine } = setup(project([KICK, SNARE]));
    const canvas = await grid();
    act(() => canvas.focus());
    fireEvent.keyDown(canvas, { key: "ArrowDown" });
    fireEvent.keyDown(canvas, { key: "ArrowRight" });
    fireEvent.keyDown(canvas, { key: "ArrowRight" });
    fireEvent.keyDown(canvas, { key: "Enter" });
    await waitFor(() => expect(engine.edits()).toEqual([{ kind: "toggleStep", channel: 2, step: 2 }]));
    // Lit now (velocity 0.8): Alt+Up raises it by 0.1, Alt+Down lowers it.
    await waitFor(() => expect(engine.view.pattern.cells["2"][2]).toBe(0.8));
    fireEvent.keyDown(canvas, { key: "ArrowUp", altKey: true });
    await waitFor(() => expect(engine.edits()).toHaveLength(2));
    expect(engine.edits()[1]).toMatchObject({ kind: "setStepVelocity", channel: 2, step: 2 });
    expect((engine.edits()[1] as { velocity: number }).velocity).toBeCloseTo(0.9);
    fireEvent.keyDown(canvas, { key: "Delete" });
    await waitFor(() => expect(engine.edits()).toHaveLength(3));
    expect(engine.edits()[2]).toEqual({ kind: "clearStep", channel: 2, step: 2 });
    await waitFor(() => expect(engine.view.pattern.cells["2"][2]).toBeNull());
    // Backspace clears too; on an empty step it sends nothing.
    fireEvent.keyDown(canvas, { key: "Backspace" });
    fireEvent.keyDown(canvas, { key: "ArrowUp", altKey: true });
    await act(async () => {});
    expect(engine.edits()).toHaveLength(3);
  });

  it("the_hint_names_the_channel_and_step_under_the_pointer", async () => {
    setup(project([KICK, SNARE]));
    const canvas = await grid();
    fireEvent.pointerMove(canvas, { ...at(1, 10), pointerId: 1 });
    const hint =
      "Snare, step 11. Click to add a hit, right-click to clear it, drag up or down to change its velocity.";
    expect(canvas.getAttribute("data-hint")).toBe(hint);
    await waitFor(() => expect(screen.getByLabelText("Hint").textContent).toBe(hint));
    fireEvent.pointerMove(canvas, { ...at(0, 0), pointerId: 1 });
    await waitFor(() =>
      expect(screen.getByLabelText("Hint").textContent).toBe(
        "Kick, step 1. Click to add a hit, right-click to clear it, drag up or down to change its velocity.",
      ),
    );
  });

  it("the_live_region_announces_the_focused_step", async () => {
    setup(project([KICK], { 1: [null, null, null, null, 0.8] }));
    const canvas = await grid();
    const region = screen.getByTestId("step-grid-status");
    expect(region.getAttribute("aria-live")).toBe("polite");
    act(() => canvas.focus());
    for (let i = 0; i < 4; i++) fireEvent.keyDown(canvas, { key: "ArrowRight" });
    expect(region.textContent).toBe("Kick, step 5, on, velocity 80%");
    fireEvent.keyDown(canvas, { key: "ArrowRight" });
    expect(region.textContent).toBe("Kick, step 6, off");
  });

  it("redraws_only_when_the_playhead_step_changes", async () => {
    const latest = createRef<LiveFrame | null>() as { current: LiveFrame | null };
    const frame = (ticks: number, playing = true, latency = 0): LiveFrame => ({
      peak: [0, 0],
      playing,
      tone: false,
      frames: 0n,
      faults: 0,
      guardHits: 0,
      outputLatencyFrames: latency,
      positionTicks: BigInt(ticks),
    });
    const { rec } = setup(project([KICK]), { latest });
    await grid();
    nextFrame();
    const first = rec.draws.length;
    expect(first).toBeGreaterThan(0);
    nextFrame();
    nextFrame();
    expect(rec.draws.length).toBe(first); // nothing changed

    latest.current = frame(240 + 10);
    nextFrame();
    expect(rec.draws.length).toBe(first + 1);
    expect(flagsIn(rec.draws.at(-1)!, 0, 1) & FLAG_PLAYHEAD).toBe(FLAG_PLAYHEAD);
    latest.current = frame(240 + 200); // same step
    nextFrame();
    expect(rec.draws.length).toBe(first + 1);

    // The output latency moves the column back to what is heard: 0.1 s at 120 BPM = 192 ticks.
    latest.current = frame(480 + 10, true, 4800);
    nextFrame();
    expect(rec.draws.length).toBe(first + 1);
    latest.current = frame(480 + 200, true, 4800);
    nextFrame();
    expect(rec.draws.length).toBe(first + 2);
    expect(flagsIn(rec.draws.at(-1)!, 0, 2) & FLAG_PLAYHEAD).toBe(FLAG_PLAYHEAD);

    // The pattern loops: step 16 of a 16-step pattern is step 0 again.
    latest.current = frame(16 * 240 + 5);
    nextFrame();
    expect(flagsIn(rec.draws.at(-1)!, 0, 0) & FLAG_PLAYHEAD).toBe(FLAG_PLAYHEAD);

    // Stopped: no column.
    latest.current = frame(0, false);
    nextFrame();
    const stopped = rec.draws.at(-1)!;
    for (let s = 0; s < 16; s++) expect(flagsIn(stopped, 0, s) & FLAG_PLAYHEAD).toBe(0);
    const count = rec.draws.length;
    nextFrame();
    expect(rec.draws.length).toBe(count);
  });

  it("focus shows only while the grid has keyboard focus", async () => {
    const { rec } = setup(project([KICK]));
    const canvas = await grid();
    nextFrame();
    expect(flagsIn(rec.draws.at(-1)!, 0, 0) & FLAG_FOCUS).toBe(0);
    act(() => canvas.focus());
    nextFrame();
    expect(flagsIn(rec.draws.at(-1)!, 0, 0) & FLAG_FOCUS).toBe(FLAG_FOCUS);
    act(() => canvas.blur());
    nextFrame();
    expect(flagsIn(rec.draws.at(-1)!, 0, 0) & FLAG_FOCUS).toBe(0);
  });

  it("a_lost_webgl_context_is_rebuilt", async () => {
    const { rec } = setup(project([KICK]));
    const canvas = await grid();
    nextFrame();
    expect(rec.factory).toHaveBeenCalledTimes(1);
    const lost = new Event("webglcontextlost", { cancelable: true });
    act(() => {
      canvas.dispatchEvent(lost);
    });
    expect(lost.defaultPrevented).toBe(true);
    // Nothing is drawn while the context is gone.
    const before = rec.draws.length;
    act(() => {
      fireEvent.pointerMove(canvas, { ...at(0, 0), pointerId: 1 });
    });
    nextFrame();
    expect(rec.draws.length).toBe(before);

    act(() => {
      canvas.dispatchEvent(new Event("webglcontextrestored"));
    });
    expect(rec.factory).toHaveBeenCalledTimes(2);
    nextFrame();
    expect(rec.draws.length).toBe(before + 1);
  });

  it("the renderer is disposed when the grid goes away", async () => {
    const { rec, unmount } = setup(project([KICK]));
    await grid();
    expect(rec.renderers).toHaveLength(1);
    unmount();
    expect(rec.renderers[0].dispose).toHaveBeenCalled();
  });

  it("falls_back_to_canvas_2d_without_webgl2", async () => {
    const calls: string[] = [];
    const ctx2d = new Proxy(
      {},
      {
        get: (_t, prop) => (typeof prop === "string" ? (...args: unknown[]) => calls.push(`${prop}(${args.length})`) : undefined),
        set: () => true,
      },
    );
    const getContext = vi
      .spyOn(HTMLCanvasElement.prototype, "getContext")
      .mockImplementation(((kind: string) => (kind === "2d" ? ctx2d : null)) as never);
    setup(project([KICK], { 1: [1] }), { renderer: false });
    await grid();
    nextFrame();
    expect(getContext.mock.calls.map((c) => c[0])).toEqual(["webgl2", "2d"]);
    expect(calls.some((c) => c.startsWith("clearRect"))).toBe(true);
    expect(calls.some((c) => c.startsWith("fill"))).toBe(true);
  });
});
