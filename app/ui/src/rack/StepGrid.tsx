import { registerGestureCancellation } from "../project/pending";
import { isProjectReplacing } from "../project/replacement";
import type { PatternStamp } from "../api/patterns";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent,
  type PointerEvent,
  type RefObject,
} from "react";
import type { WireNote } from "../api/notes";
import type { ChannelView, Edit, PatternView } from "../api/project";
import type { LiveFrame } from "../live/decode";
import { useProject } from "../project/context";
import { displayTicks } from "../transport/position";
import {
  CELL,
  EDGE,
  GAP,
  GRID_HINT,
  backingScale,
  buildInstances,
  cellAt,
  gridSize,
  nextFocus,
  rollHint,
  stepHint,
  velocityFromDrag,
  type Cell,
  type GridLayout,
} from "./gridModel";
import { readPalette, type GridRenderer, type Palette } from "./palette";
import { subscribeThemeChange } from "../theme/changes";
import { createCanvasRenderer } from "./canvasRenderer";
import { createGlRenderer } from "./glRenderer";
import { createGridWake, subscribeGridDpr } from "./gridWake";
import { rollInstances } from "./rollPreview";

/** A step is a sixteenth: 240 ticks. */
const TICKS_PER_STEP = 240n;
/** Pixels a press must move before it counts as a velocity drag. */
const DRAG_START_PX = 3;
/** What Alt+Up and Alt+Down change the velocity by. */
const VELOCITY_KEY_STEP = 0.1;
const NO_NOTES: WireNote[] = [];

/** How long a lost WebGL context may stay lost before the grid gives up on it and draws in 2D. */
export const RESTORE_WAIT_MS = 2000;

const round2 = (n: number) => Math.round(n * 100) / 100;
const clamp01 = (n: number) => Math.min(1, Math.max(0, n));

let warnedPalette = false;
function paletteOf(root: Element): Palette {
  const palette = readPalette(root);
  if (palette.missing.length > 0 && !warnedPalette) {
    warnedPalette = true;
    console.warn(`The step grid is missing theme tokens: ${palette.missing.join(", ")}.`);
  }
  return palette;
}

interface Props {
  channels: ChannelView[];
  pattern: PatternView;
  bpm: number;
  /** The live stream's latest frame, read once per animation frame for the playhead. */
  latest: RefObject<LiveFrame | null>;
  /** Wakes the grid when the first playing frame arrives. */
  subscribePlaybackStart: (wake: () => void) => () => void;
  /** The output's rate, for the latency shift of the playhead. */
  sampleRate: number;
  /** The pattern's notes: roll channels (`ChannelView.roll`) show them instead of steps. */
  notes?: WireNote[];
  /** A roll channel's row was clicked (or Enter pressed on it): open the piano roll there. */
  onOpenRoll?: (channel: number) => void;
  /** Makes the WebGL2 renderer for the canvas; `null` when WebGL2 or its shaders are not
   * available (tests pass a recording one). */
  createRenderer?: (canvas: HTMLCanvasElement) => GridRenderer | null;
  /** Makes the 2D renderer used instead, always on a fresh canvas: one that has handed out a
   * WebGL2 context cannot give a 2D one. */
  createFallbackRenderer?: (canvas: HTMLCanvasElement) => GridRenderer;
}

/** A velocity being dragged, or committed and waiting for the view that holds it. */
interface Override {
  row: number;
  step: number;
  velocity: number;
}

interface Press {
  origin: PatternStamp;
  cell: Cell;
  y: number;
  start: number;
  lit: boolean;
  moved: boolean;
}

/** What the draw loop reads; React keeps it current without restarting the loop. */
interface Scene {
  cells: (number | null)[][];
  /** Per row, the roll preview's records, or `null` for a step row. */
  roll: (Float32Array | null)[];
  inks: number[];
  layout: GridLayout;
  focus: Cell | null;
  bpm: number;
  sampleRate: number;
}

/** The channel rack's step grid: one canvas for every channel's steps. The cells, playhead and
 * focus are drawn by a `GridRenderer`; React only handles input and sizing. */
export function StepGrid({
  channels,
  pattern,
  bpm,
  latest,
  subscribePlaybackStart,
  sampleRate,
  notes = NO_NOTES,
  onOpenRoll = () => {},
  createRenderer = createGlRenderer,
  createFallbackRenderer = createCanvasRenderer,
}: Props) {
  const { apply, view: projectView } = useProject();
  const originNow = useRef<PatternStamp>({ generation: 0, patternId: 0, revision: 1 });
  useEffect(() => { originNow.current = { generation: projectView?.generation ?? 0, patternId: pattern.id, revision: projectView?.notesRevision ?? 1 }; }, [projectView?.generation, projectView?.notesRevision, pattern.id]);
  const stamp = (): PatternStamp => originNow.current;
  const canvas = useRef<HTMLCanvasElement>(null);
  const [focus, setFocus] = useState<Cell>({ row: 0, step: 0 });
  const [hasFocus, setHasFocus] = useState(false);
  const [dpr, setDpr] = useState(() => window.devicePixelRatio || 1);
  // Set by keyboard moves: the focused cell is scrolled into view (long patterns, many rows).
  const reveal = useRef(false);
  const anchor = useRef<HTMLSpanElement>(null);
  // The canvas had focus when WebGL2 was given up: the new 2D canvas takes it.
  const refocus = useRef(false);

  const rows = channels.length;
  const steps = pattern.steps;
  const cells = channels.map((c) => pattern.cells[String(c.id)] ?? []);
  const focusCell = nextFocus(focus, "", rows, steps);
  const size = gridSize({ rows, steps, cell: CELL, gap: GAP, dpr: 1 });
  const cssWidth = size.width + 2 * EDGE;
  const cssHeight = size.height + 2 * EDGE;
  const scale = backingScale(cssWidth, cssHeight, dpr);
  const layout: GridLayout = { rows, steps, cell: CELL, gap: GAP, dpr: scale };

  const scene = useRef<Scene | null>(null);
  const wake = useRef<ReturnType<typeof createGridWake> | null>(null);
  const markDirty = useCallback(() => wake.current?.markDirty(), []);

  useEffect(() => subscribeGridDpr(window, setDpr), []);
  const override = useRef<Override | null>(null);
  // Set once a velocity edit is applied: the next view holds the value, so the override goes.
  const settled = useRef(false);
  const press = useRef<Press | null>(null);

  const renderer = useRef<GridRenderer | null>(null);
  const palette = useRef<Palette | null>(null);
  const factories = useRef({ gl: createRenderer, fallback: createFallbackRenderer });
  // Once WebGL2 has failed (no renderer, or a context lost for good) the grid draws in 2D on a
  // new canvas element (the `key` below), and stays there.
  const [use2d, setUse2d] = useState(false);

  useEffect(() => {
    factories.current = { gl: createRenderer, fallback: createFallbackRenderer };
  }, [createRenderer, createFallbackRenderer]);

  // Keep the scene current; any change is a redraw. A new view comes with new `channels` and
  // `pattern` objects.
  const focusRow = hasFocus ? focusCell.row : -1;
  const focusStep = hasFocus ? focusCell.step : -1;
  useEffect(() => {
    if (settled.current) {
      override.current = null;
      settled.current = false;
    }
    const sceneLayout = { rows: channels.length, steps: pattern.steps, cell: CELL, gap: GAP, dpr: scale };
    scene.current = {
      cells: channels.map((c) => pattern.cells[String(c.id)] ?? []),
      roll: channels.map((c, i) => (c.roll ? rollInstances(sceneLayout, i, notes, c.id, c.color, focusRow === i) : null)),
      inks: channels.map((c) => c.color),
      layout: sceneLayout,
      focus: focusRow >= 0 ? { row: focusRow, step: focusStep } : null,
      bpm,
      sampleRate,
    };
    markDirty();
  }, [channels, pattern, notes, scale, focusRow, focusStep, bpm, sampleRate, markDirty]);

  useEffect(() => {
    if (!reveal.current || !hasFocus) return;
    reveal.current = false;
    anchor.current?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
  }, [focusRow, focusStep, hasFocus]);

  // The renderer: WebGL2 first, rebuilt when a lost context comes back. If WebGL2 gives no
  // renderer, or a lost context is not back within RESTORE_WAIT_MS, switch to 2D for good.
  useEffect(() => {
    const el = canvas.current;
    if (!el) return;
    let wait: ReturnType<typeof setTimeout> | undefined;
    const giveUp = () => {
      refocus.current = document.activeElement === el;
      setHasFocus(false);
      setUse2d(true);
    };
    const refreshPalette = () => {
      palette.current = paletteOf(document.documentElement);
      markDirty();
    };
    const build = () => {
      clearTimeout(wait);
      refreshPalette();
      const made = use2d ? factories.current.fallback(el) : factories.current.gl(el);
      renderer.current = made;
      if (use2d && refocus.current) {
        refocus.current = false;
        el.focus();
      }
      // Deferred: a state change straight from an effect would render in a cascade.
      if (!made) queueMicrotask(giveUp);
    };
    const onLost = (e: Event) => {
      e.preventDefault();
      renderer.current = null;
      clearTimeout(wait);
      wait = setTimeout(giveUp, RESTORE_WAIT_MS);
    };
    build();
    const unsubscribeTheme = subscribeThemeChange(refreshPalette);
    if (!use2d) {
      el.addEventListener("webglcontextlost", onLost);
      el.addEventListener("webglcontextrestored", build);
    }
    return () => {
      unsubscribeTheme();
      clearTimeout(wait);
      el.removeEventListener("webglcontextlost", onLost);
      el.removeEventListener("webglcontextrestored", build);
      renderer.current?.dispose();
      renderer.current = null;
    };
  }, [use2d, markDirty]);

  useEffect(() => {
    const readPlayhead = () => {
      const frame = latest.current;
      const s = scene.current;
      if (!frame?.playing || !s || s.layout.steps <= 0) return null;
      const ticks = displayTicks(frame.positionTicks, frame.outputLatencyFrames, s.bpm, s.sampleRate);
      return Number((ticks / TICKS_PER_STEP) % BigInt(s.layout.steps));
    };
    const draw = (playhead: number | null) => {
      const s = scene.current;
      const r = renderer.current;
      const p = palette.current;
      if (!s || !r || !p) return false;
      const o = override.current;
      const shownCells = o
        ? s.cells.map((row, i) => {
            if (i !== o.row) return row;
            const copy = row.slice();
            copy[o.step] = o.velocity;
            return copy;
          })
        : s.cells;
      r.draw(buildInstances(s.layout, shownCells, s.inks, s.focus, playhead, s.roll), s.layout, p);
      return true;
    };
    const scheduler = createGridWake(readPlayhead, draw);
    wake.current = scheduler;
    const unsubscribe = subscribePlaybackStart(scheduler.startPlayback);
    scheduler.markDirty();
    return () => {
      unsubscribe();
      wake.current = null;
      scheduler.cancel();
    };
  }, [latest, subscribePlaybackStart]);

  const valueAt = (cell: Cell): number | null => cells[cell.row]?.[cell.step] ?? null;
  const channelAt = (cell: Cell) => channels[cell.row];

  const send = useCallback((edit: Edit, origin?: PatternStamp) => apply(edit, undefined, origin), [apply]);

  const cellFrom = (e: { clientX: number; clientY: number }): Cell | null => {
    const el = canvas.current;
    if (!el) return null;
    const rect = el.getBoundingClientRect();
    return cellAt(layout, e.clientX - rect.left - EDGE, e.clientY - rect.top - EDGE);
  };

  const setHint = (text: string) => canvas.current?.setAttribute("data-hint", text);

  const hintFor = (cell: Cell | null) => {
    const ch = cell && channelAt(cell);
    if (!cell || !ch) return GRID_HINT;
    return ch.roll ? rollHint(ch.name) : stepHint(ch.name, cell.step);
  };

  const moveFocus = (cell: Cell) => {
    reveal.current = true;
    setFocus(cell);
    setHint(hintFor(cell));
  };

  const onPointerDown = (e: PointerEvent<HTMLCanvasElement>) => {
    if (isProjectReplacing()) return;
    if (e.button !== 0) return;
    const cell = cellFrom(e);
    if (!cell) return;
    setFocus(cell);
    // A roll channel's preview opens the piano roll on release; it never edits a step.
    const value = channelAt(cell)?.roll ? null : valueAt(cell);
    press.current = { origin: stamp(), cell, y: e.clientY, start: value ?? 0, lit: value !== null, moved: false };
    markDirty();
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* no pointer capture here */
    }
  };

  const onPointerMove = (e: PointerEvent<HTMLCanvasElement>) => {
    const p = press.current;
    if (!p) {
      setHint(hintFor(cellFrom(e)));
      return;
    }
    if (!p.lit) return;
    const dy = e.clientY - p.y;
    if (!p.moved && Math.abs(dy) < DRAG_START_PX) return;
    p.moved = true;
    settled.current = false;
    override.current = { ...p.cell, velocity: velocityFromDrag(p.start, dy) };
    markDirty();
  };

  const onPointerUp = (e: PointerEvent<HTMLCanvasElement>) => {
    const p = press.current;
    press.current = null;
    if (!p || e.button !== 0) return;
    markDirty();
    const ch = channelAt(p.cell);
    if (!ch) return;
    if (ch.roll) {
      onOpenRoll(ch.id);
      return;
    }
    if (p.moved && override.current) {
      // One edit for the whole drag; the drawn value stays until the view holds it.
      const velocity = round2(override.current.velocity);
      override.current = { ...p.cell, velocity };
      const { row, step } = p.cell;
      void send({ kind: "setStepVelocity", channel: ch.id, step, velocity }, p.origin).then((applied) => {
        if (stamp().patternId !== p.origin.patternId || stamp().generation !== p.origin.generation) return;
        if (override.current?.row !== row || override.current.step !== step) return;
        if (!applied || scene.current?.cells[row]?.[step] === velocity) {
          // Refused (the view is unchanged), or the new view is already in the scene.
          override.current = null;
          markDirty();
        } else {
          settled.current = true;
        }
      });
    } else if (!p.moved) {
      void send({ kind: "toggleStep", channel: ch.id, step: p.cell.step }, p.origin);
    }
  };

  useEffect(() => registerGestureCancellation(() => {
    press.current = null; override.current = null; markDirty();
  }), [markDirty]);

  useEffect(() => {
    press.current = null; override.current = null; markDirty();
  }, [pattern.id, projectView?.generation, markDirty]);

  const onPointerCancel = () => {
    press.current = null;
    override.current = null;
    markDirty();
  };

  const onContextMenu = (e: MouseEvent<HTMLCanvasElement>) => {
    e.preventDefault();
    const cell = cellFrom(e);
    if (!cell) return;
    const ch = channelAt(cell);
    if (ch && !ch.roll && valueAt(cell) !== null) void send({ kind: "clearStep", channel: ch.id, step: cell.step });
  };

  const onKeyDown = (e: KeyboardEvent<HTMLCanvasElement>) => {
    if (rows === 0 || steps === 0) return;
    const ch = channelAt(focusCell);
    // A roll channel's row has no steps: Enter opens the piano roll, step keys do nothing.
    const roll = ch?.roll ?? false;
    const value = roll ? null : valueAt(focusCell);
    if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
      e.preventDefault();
      if (ch && value !== null) {
        const velocity = round2(clamp01(value + (e.key === "ArrowUp" ? VELOCITY_KEY_STEP : -VELOCITY_KEY_STEP)));
        if (velocity !== value) void send({ kind: "setStepVelocity", channel: ch.id, step: focusCell.step, velocity });
      }
      return;
    }
    switch (e.key) {
      case "ArrowUp":
      case "ArrowDown":
      case "ArrowLeft":
      case "ArrowRight":
      case "Home":
      case "End":
        e.preventDefault();
        moveFocus(nextFocus(focusCell, e.key, rows, steps));
        return;
      case "Enter":
        e.preventDefault();
        // A held Enter repeats: one toggle per press.
        if (e.repeat || !ch) return;
        if (roll) onOpenRoll(ch.id);
        else void send({ kind: "toggleStep", channel: ch.id, step: focusCell.step });
        return;
      case "Delete":
      case "Backspace":
        e.preventDefault();
        if (ch && value !== null) void send({ kind: "clearStep", channel: ch.id, step: focusCell.step });
        return;
      default:
        return;
    }
  };

  const focusedChannel = channelAt(focusCell);
  const focusedValue = valueAt(focusCell);
  const status = !hasFocus || !focusedChannel
    ? ""
    : focusedChannel.roll
      ? `${focusedChannel.name}, piano-roll notes`
      : `${focusedChannel.name}, step ${focusCell.step + 1}, ${
          focusedValue === null ? "off" : `on, velocity ${Math.round(focusedValue * 100)}%`
        }`;

  return (
    <div className="step-grid">
      <canvas
        key={use2d ? "2d" : "webgl2"}
        ref={canvas}
        className="step-grid-canvas"
        tabIndex={0}
        aria-label="Step grid"
        aria-describedby="step-grid-keys"
        data-hint={GRID_HINT}
        width={Math.round(cssWidth * scale)}
        height={Math.round(cssHeight * scale)}
        style={{ width: `${cssWidth}px`, height: `${cssHeight}px` }}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerCancel}
        onPointerLeave={() => {
          if (!press.current) setHint(GRID_HINT);
        }}
        onContextMenu={onContextMenu}
        onKeyDown={onKeyDown}
        onFocus={() => setHasFocus(true)}
        onBlur={() => setHasFocus(false)}
      />
      <span
        ref={anchor}
        className="step-grid-anchor"
        aria-hidden
        style={{
          left: EDGE + focusCell.step * (CELL + GAP),
          top: EDGE + focusCell.row * (CELL + GAP),
          width: CELL,
          height: CELL,
        }}
      />
      <span id="step-grid-keys" className="visually-hidden">
        Arrow keys move between steps. Enter adds or removes a hit, Delete clears it, Alt with Up or
        Down changes its velocity.
      </span>
      <span className="visually-hidden" aria-live="polite" data-testid="step-grid-status">
        {status}
      </span>
    </div>
  );
}
