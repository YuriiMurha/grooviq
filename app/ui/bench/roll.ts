// The piano roll bench (`npm run bench`): the real PianoRoll against the fake engine with 10,000
// notes, driven by synthetic pointer and wheel events through the scripted edits of the
// performance acceptance, while every animation frame's timestamp is recorded. The result goes
// to `window.__bench`; bench/roll.bench.ts reads it and holds it to the budget.
import "@fontsource-variable/manrope";
import "../src/theme/riso.css";
import "../src/app.css";
import { createElement } from "react";
import { createRoot } from "react-dom/client";
import { decodeNotes, type NotesAck, type WireNote } from "../src/api/notes";
import type { ProjectView } from "../src/api/project";
import { ProjectContext, type ProjectContextValue } from "../src/project/context";
import type { NotesApi } from "../src/roll/commitQueue";
import { PianoRoll } from "../src/roll/PianoRoll";
import { rollStats } from "../src/roll/stats";
import { channel, createFakeEngine, project } from "../src/test/fakeEngine";

const NOTES = 10_000;
/** Another channel's notes, drawn behind the edited channel's as ghosts. */
const GHOSTS = 3_000;
const STEPS = 256;
const PATTERN_TICKS = STEPS * 240;
const LOW_KEY = 48;
const KEY_SPAN = 36;

export interface BenchResult {
  /** The display's refresh interval: the median of 120 idle animation frame intervals. */
  refreshMs: number;
  p95: number;
  max: number;
  frames: number;
  renderer: string;
  /** What the scripted edits did, to show they ran. */
  selectedForMove: number;
  selectedForResize: number;
  edits: number;
  /** The notes each edit with updates changed: the move, then the resize. */
  updated: number[];
  notes: number;
}

// --- The fake engine and the Tauri shim the notes commands go through. ---
const view: ProjectView = project(
  [channel(1, "Lead", { kind: "synth", preset: "Init", roll: true, color: 0 }), channel(2, "Pad", { kind: "synth", preset: "Init", roll: true, color: 2 })],
  {},
  STEPS,
);
const engine = createFakeEngine(view);
let seed = 2024;
const rand = (n: number) => {
  seed = (seed * 1664525 + 1013904223) >>> 0;
  return seed % n;
};
engine.notes = Array.from({ length: NOTES + GHOSTS }, (_, i) => ({
  id: i + 1,
  channel: i < NOTES ? 1 : 2,
  tick: rand(PATTERN_TICKS - 960),
  length: 60 + rand(900),
  key: LOW_KEY + rand(KEY_SPAN),
  velocity: 0.5 + rand(50) / 100,
  slide: false,
}));
let callbackId = 0;
(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
  invoke: (cmd: string, args?: Record<string, unknown>) =>
    cmd.startsWith("plugin:event|") ? Promise.resolve(++callbackId) : engine.invoke(cmd, args),
  transformCallback: () => ++callbackId,
};
(window as unknown as Record<string, unknown>).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };

const context: ProjectContextValue = {
  view,
  apply: async () => true,
  run: async () => true,
  setView: () => {},
};
const api: NotesApi = {
  notesEdit: (edit) => engine.invoke("notes_edit", { edit }) as Promise<NotesAck>,
  patternNotes: async () => decodeNotes((await engine.invoke("pattern_notes")) as ArrayBuffer),
};

const root = document.getElementById("root")!;
root.style.cssText = "display:flex;flex-direction:column;height:100vh";
createRoot(root).render(createElement(ProjectContext.Provider, { value: context }, createElement(PianoRoll, { channel: 1, api })));

// --- Driving it. ---
const frame = () => new Promise<number>((r) => requestAnimationFrame(r));
const canvas = () => document.querySelector<HTMLCanvasElement>(".roll-canvas")!;
const v = () => rollStats.view!;
/** Client coordinates of (tick, key row), in the middle of the row. */
function at(tick: number, key: number) {
  const r = canvas().getBoundingClientRect();
  return { x: r.left + (tick - v().scrollTick) * v().tickPx, y: r.top + (v().topKey - key - 0.5) * v().keyPx };
}
function pointer(type: string, p: { x: number; y: number }, buttons = 1) {
  canvas().dispatchEvent(
    new PointerEvent(type, { clientX: p.x, clientY: p.y, button: type === "pointermove" ? -1 : 0, buttons, pointerId: 1, bubbles: true, cancelable: true }),
  );
}
function wheel(p: { x: number; y: number }, deltaX: number, deltaY: number, ctrlKey = false) {
  canvas().dispatchEvent(new WheelEvent("wheel", { clientX: p.x, clientY: p.y, deltaX, deltaY, ctrlKey, bubbles: true, cancelable: true }));
}
const toolButton = (name: string) =>
  [...document.querySelectorAll<HTMLButtonElement>(".roll-tool")].find((b) => b.textContent === name)!;
const notes = () => engine.notes as WireNote[];
/** The note a hit test lands on at (tick, key): the one that starts latest. */
function top(tick: number, key: number): WireNote | null {
  let best: WireNote | null = null;
  for (const n of notes()) {
    if (n.channel === 1 && n.key === key && n.tick <= tick && tick < n.tick + n.length && (!best || n.tick >= best.tick)) best = n;
  }
  return best;
}
const inBox = (t0: number, t1: number) => notes().filter((n) => n.channel === 1 && n.tick < t1 && n.tick + n.length > t0).length;

async function drag(from: { x: number; y: number }, path: (t: number) => { x: number; y: number }, ms: number) {
  pointer("pointerdown", from);
  const start = await frame();
  for (let now = start; now - start < ms; now = await frame()) pointer("pointermove", path((now - start) / ms));
  pointer("pointerup", path(1), 0);
  await frame();
}

/** Where the script is, for a bench that stalls. */
const stage = (name: string) => ((window as unknown as { __benchStage: string }).__benchStage = name);

async function run(): Promise<BenchResult> {
  stage("loading");
  while (!rollStats.view || !engine.calls.some((c) => c.cmd === "pattern_notes")) await frame();
  for (let i = 0; i < 30; i++) await frame();

  stage("refresh");
  // The refresh interval, measured idle.
  const idle: number[] = [];
  let last = await frame();
  for (let i = 0; i < 120; i++) {
    const t = await frame();
    idle.push(t - last);
    last = t;
  }
  idle.sort((a, b) => a - b);
  const refreshMs = idle[60];

  // Every frame interval from here on, except across the pauses where the script itself
  // searches the notes for a point to grab (its work, not the roll's).
  const intervals: number[] = [];
  let recording = true;
  let paused = false;
  let previous: number | null = null;
  const record = (t: number) => {
    if (paused) previous = null;
    else {
      if (previous !== null) intervals.push(t - previous);
      previous = t;
    }
    if (recording) requestAnimationFrame(record);
  };
  requestAnimationFrame(record);
  const search = async <T,>(find: () => T): Promise<T> => {
    paused = true;
    await frame();
    const found = find();
    await frame();
    paused = false;
    await frame();
    return found;
  };
  const editsBefore = engine.calls.filter((c) => c.cmd === "notes_edit").length;

  stage("move");
  // 1. Select about 1,000 notes with a box and drag them around for 2 s.
  toolButton("Select").click();
  await frame();
  const boxEnd = PATTERN_TICKS / 10;
  const topRow = LOW_KEY + KEY_SPAN;
  await drag(at(1, topRow), (t) => {
    const a = at(1, topRow);
    const b = at(boxEnd, LOW_KEY - 1);
    return { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t };
  }, 300);
  const selectedForMove = inBox(1, boxEnd);
  // A point on a note (the one a hit test lands on), well away from its right edge.
  const grabPoint = await search(() => {
    for (let tick = 1000; tick < boxEnd - 1000; tick += 37) {
      for (let key = LOW_KEY; key < topRow; key++) {
        const n = top(tick, key);
        if (n && n.tick + n.length - tick > 200) return { tick, key };
      }
    }
    throw new Error("no note to grab");
  });
  const from = at(grabPoint.tick, grabPoint.key);
  // Round about, ending two beats on and two rows up: one 1,000-note commit at release.
  await drag(from, (t) => ({ x: from.x + 160 * Math.sin(t * Math.PI * 2) + 2 * 960 * v().tickPx * t, y: from.y - 60 * Math.sin(t * Math.PI) - 2 * v().keyPx * t }), 2000);

  stage("draw");
  // 2. Draw 50 notes, one a frame, above the seeded ones.
  toolButton("Draw").click();
  await frame();
  for (let i = 0; i < 50; i++) {
    const p = at(240 * (2 + i), topRow + 2 + (i % 3));
    pointer("pointerdown", p);
    pointer("pointerup", p, 0);
    await frame();
  }

  stage("resize");
  // 3. Scroll to the middle of the pattern, select about 200 notes and resize them from an edge.
  const mid = PATTERN_TICKS / 2;
  while (v().scrollTick < mid) {
    canvas().dispatchEvent(new WheelEvent("wheel", { deltaY: 120, shiftKey: true, bubbles: true, cancelable: true }));
    await frame();
  }
  toolButton("Select").click();
  await frame();
  const span = PATTERN_TICKS / 50;
  const t0 = v().scrollTick + 100;
  await drag(at(t0, topRow), (t) => {
    const a = at(t0, topRow);
    const b = at(t0 + span, LOW_KEY - 1);
    return { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t };
  }, 300);
  const selectedForResize = inBox(t0, t0 + span);
  // A point in the resize zone of the note a hit test lands on, a note inside the box.
  const gripAt = await search(() => {
    for (let tick = t0 + 200; tick < t0 + span - 200; tick += 7) {
      for (let key = LOW_KEY; key < topRow; key++) {
        const n = top(tick, key);
        const left = n ? n.tick + n.length - tick : 0;
        if (n && left >= 5 && left <= 40 && n.length > 200) return { tick, key };
      }
    }
    throw new Error("no note edge to grab");
  });
  const grip = at(gripAt.tick, gripAt.key);
  await drag(grip, (t) => ({ x: grip.x + 120 * t, y: grip.y }), 1000);

  stage("zoom");
  // 4. Zoom out and in about the pointer, then scroll back and up and down.
  const centre = at(v().scrollTick + 2000, LOW_KEY + 10);
  for (let i = 0; i < 40; i++) {
    wheel(centre, 0, 40, true);
    await frame();
  }
  for (let i = 0; i < 40; i++) {
    wheel(centre, 0, -40, true);
    await frame();
  }
  for (let i = 0; i < 60; i++) {
    wheel(centre, -40, i < 30 ? 20 : -20);
    await frame();
  }

  // A few frames after the last edit, for its commit to land.
  for (let i = 0; i < 10; i++) await frame();
  recording = false;
  await frame();
  const sorted = [...intervals].sort((a, b) => a - b);
  const gl = canvas().getContext("webgl2");
  const info = gl?.getExtension("WEBGL_debug_renderer_info");
  return {
    refreshMs,
    p95: sorted[Math.floor(sorted.length * 0.95)],
    max: sorted[sorted.length - 1],
    frames: intervals.length,
    renderer: gl ? String(gl.getParameter(info ? info.UNMASKED_RENDERER_WEBGL : gl.RENDERER)) : "none",
    selectedForMove,
    selectedForResize,
    edits: engine.calls.filter((c) => c.cmd === "notes_edit").length - editsBefore,
    updated: engine.calls
      .filter((c) => c.cmd === "notes_edit")
      .map((c) => (c.args?.edit as { update: unknown[] }).update.length)
      .filter((n) => n > 0),
    notes: notes().length,
  };
}

run().then(
  (r) => ((window as unknown as { __bench: BenchResult }).__bench = r),
  (e: unknown) => ((window as unknown as { __benchError: string }).__benchError = String(e)),
);
