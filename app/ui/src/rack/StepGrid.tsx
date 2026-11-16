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
  stepHint,
  velocityFromDrag,
  type Cell,
  type GridLayout,
} from "./gridModel";
import { readPalette, type GridRenderer, type Palette } from "./palette";
import { createCanvasRenderer } from "./canvasRenderer";
import { createGlRenderer } from "./glRenderer";

/** A step is a sixteenth: 240 ticks. */
const TICKS_PER_STEP = 240n;
/** Pixels a press must move before it counts as a velocity drag. */
const DRAG_START_PX = 3;
/** What Alt+Up and Alt+Down change the velocity by. */
const VELOCITY_KEY_STEP = 0.1;
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
  /** The output's rate, for the latency shift of the playhead. */
  sampleRate: number;
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
  cell: Cell;
  y: number;
  start: number;
  lit: boolean;
  moved: boolean;
}

/** What the draw loop reads; React keeps it current without restarting the loop. */
interface Scene {
  cells: (number | null)[][];
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
  sampleRate,
  createRenderer = createGlRenderer,
  createFallbackRenderer = createCanvasRenderer,
}: Props) {
  const { apply } = useProject();
  const canvas = useRef<HTMLCanvasElement>(null);
  const [focus, setFocus] = useState<Cell>({ row: 0, step: 0 });
  const [hasFocus, setHasFocus] = useState(false);
  const [dpr, setDpr] = useState(() => window.devicePixelRatio || 1);

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
  const dirty = useRef(true);
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
    scene.current = {
      cells: channels.map((c) => pattern.cells[String(c.id)] ?? []),
      inks: channels.map((c) => c.color),
      layout: { rows: channels.length, steps: pattern.steps, cell: CELL, gap: GAP, dpr: scale },
      focus: focusRow >= 0 ? { row: focusRow, step: focusStep } : null,
      bpm,
      sampleRate,
    };
    dirty.current = true;
  }, [channels, pattern, scale, focusRow, focusStep, bpm, sampleRate]);

  // The device pixel ratio changes with zoom and when the window moves to another screen.
  useEffect(() => {
    const onResize = () => setDpr(window.devicePixelRatio || 1);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  // The renderer: WebGL2 first, rebuilt when a lost context comes back. If WebGL2 gives no
  // renderer, or a lost context is not back within RESTORE_WAIT_MS, switch to 2D for good.
  useEffect(() => {
    const el = canvas.current;
    if (!el) return;
    let wait: ReturnType<typeof setTimeout> | undefined;
    const giveUp = () => {
      setHasFocus(false);
      setUse2d(true);
    };
    const build = () => {
      clearTimeout(wait);
      palette.current = paletteOf(document.documentElement);
      const made = use2d ? factories.current.fallback(el) : factories.current.gl(el);
      renderer.current = made;
      dirty.current = true;
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
    if (use2d) {
      return () => {
        renderer.current?.dispose();
        renderer.current = null;
      };
    }
    el.addEventListener("webglcontextlost", onLost);
    el.addEventListener("webglcontextrestored", build);
    return () => {
      clearTimeout(wait);
      el.removeEventListener("webglcontextlost", onLost);
      el.removeEventListener("webglcontextrestored", build);
      renderer.current?.dispose();
      renderer.current = null;
    };
  }, [use2d]);

  // The draw loop: the playhead is read every frame; the grid is drawn only when the playhead's
  // step, the cells, the focus or the size changed.
  useEffect(() => {
    let raf = 0;
    let shown: number | null | undefined;
    const tick = () => {
      raf = requestAnimationFrame(tick);
      const s = scene.current;
      const r = renderer.current;
      const p = palette.current;
      if (!s || !r || !p) return;
      const frame = latest.current;
      let playhead: number | null = null;
      if (frame?.playing && s.layout.steps > 0) {
        const ticks = displayTicks(frame.positionTicks, frame.outputLatencyFrames, s.bpm, s.sampleRate);
        playhead = Number((ticks / TICKS_PER_STEP) % BigInt(s.layout.steps));
      }
      if (!dirty.current && playhead === shown) return;
      const o = override.current;
      const shownCells = o
        ? s.cells.map((row, i) => {
            if (i !== o.row) return row;
            const copy = row.slice();
            copy[o.step] = o.velocity;
            return copy;
          })
        : s.cells;
      r.draw(buildInstances(s.layout, shownCells, s.inks, s.focus, playhead), s.layout, p);
      dirty.current = false;
      shown = playhead;
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [latest]);

  const valueAt = (cell: Cell): number | null => cells[cell.row]?.[cell.step] ?? null;
  const channelAt = (cell: Cell) => channels[cell.row];

  const send = useCallback((edit: Edit) => apply(edit), [apply]);

  const cellFrom = (e: { clientX: number; clientY: number }): Cell | null => {
    const el = canvas.current;
    if (!el) return null;
    const rect = el.getBoundingClientRect();
    return cellAt(layout, e.clientX - rect.left - EDGE, e.clientY - rect.top - EDGE);
  };

  const setHint = (text: string) => canvas.current?.setAttribute("data-hint", text);

  const moveFocus = (cell: Cell) => {
    setFocus(cell);
    const ch = channelAt(cell);
    if (ch) setHint(stepHint(ch.name, cell.step));
  };

  const onPointerDown = (e: PointerEvent<HTMLCanvasElement>) => {
    if (e.button !== 0) return;
    const cell = cellFrom(e);
    if (!cell) return;
    setFocus(cell);
    const value = valueAt(cell);
    press.current = { cell, y: e.clientY, start: value ?? 0, lit: value !== null, moved: false };
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* no pointer capture here */
    }
  };

  const onPointerMove = (e: PointerEvent<HTMLCanvasElement>) => {
    const p = press.current;
    if (!p) {
      const cell = cellFrom(e);
      const ch = cell && channelAt(cell);
      setHint(cell && ch ? stepHint(ch.name, cell.step) : GRID_HINT);
      return;
    }
    if (!p.lit) return;
    const dy = e.clientY - p.y;
    if (!p.moved && Math.abs(dy) < DRAG_START_PX) return;
    p.moved = true;
    settled.current = false;
    override.current = { ...p.cell, velocity: velocityFromDrag(p.start, dy) };
    dirty.current = true;
  };

  const onPointerUp = (e: PointerEvent<HTMLCanvasElement>) => {
    const p = press.current;
    press.current = null;
    if (!p || e.button !== 0) return;
    const ch = channelAt(p.cell);
    if (!ch) return;
    if (p.moved && override.current) {
      // One edit for the whole drag; the drawn value stays until the view holds it.
      const velocity = round2(override.current.velocity);
      override.current = { ...p.cell, velocity };
      const { row, step } = p.cell;
      void send({ kind: "setStepVelocity", channel: ch.id, step, velocity }).then((applied) => {
        if (override.current?.row !== row || override.current.step !== step) return;
        if (!applied || scene.current?.cells[row]?.[step] === velocity) {
          // Refused (the view is unchanged), or the new view is already in the scene.
          override.current = null;
          dirty.current = true;
        } else {
          settled.current = true;
        }
      });
    } else if (!p.moved) {
      void send({ kind: "toggleStep", channel: ch.id, step: p.cell.step });
    }
  };

  const onPointerCancel = () => {
    press.current = null;
    override.current = null;
    dirty.current = true;
  };

  const onContextMenu = (e: MouseEvent<HTMLCanvasElement>) => {
    e.preventDefault();
    const cell = cellFrom(e);
    if (!cell) return;
    const ch = channelAt(cell);
    if (ch && valueAt(cell) !== null) void send({ kind: "clearStep", channel: ch.id, step: cell.step });
  };

  const onKeyDown = (e: KeyboardEvent<HTMLCanvasElement>) => {
    if (rows === 0 || steps === 0) return;
    const ch = channelAt(focusCell);
    const value = valueAt(focusCell);
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
        if (ch) void send({ kind: "toggleStep", channel: ch.id, step: focusCell.step });
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
  const status =
    hasFocus && focusedChannel
      ? `${focusedChannel.name}, step ${focusCell.step + 1}, ${
          focusedValue === null ? "off" : `on, velocity ${Math.round(focusedValue * 100)}%`
        }`
      : "";

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
