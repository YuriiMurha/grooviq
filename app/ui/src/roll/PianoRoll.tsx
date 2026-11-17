import { useCallback, useEffect, useMemo, useRef, useState, type PointerEvent, type RefObject } from "react";
import { onNotesChanged, notesEdit, patternNotes } from "../api/notes";
import type { LiveFrame } from "../live/decode";
import { useProject } from "../project/context";
import { isTypingTarget, modalIsOpen } from "../project/shortcuts";
import { displayTicks } from "../transport/position";
import { createCanvasRoll } from "./canvasRoll";
import { CommitQueue, type NotesApi } from "./commitQueue";
import { createRollRenderer, readRollPalette, type RollPalette, type RollRenderer } from "./glRoll";
import { KeyColumn } from "./KeyColumn";
import { NoteStore, buildInstances } from "./noteStore";
import { DEFAULT_GRID, GRIDS, barTicks, gridTicks } from "./snap";
import { rollStats } from "./stats";
import { endGesture, moveGesture, startGesture, EDGE_PX, type Gesture, type Pointer, type Step, type Tool, type ToolContext } from "./tools";
import { Playhead, clampView, scrollBy, toWorld, zoomAt, type RollView } from "./view";

const API: NotesApi = { notesEdit, patternNotes };
/** How long a lost WebGL context may take to come back before the roll moves to 2D for good. */
const RESTORE_WAIT_MS = 2000;
const STEP_TICKS = 240;
const TICKS_PER_WHOLE = 3840;
const EMPTY_HINT = "Click to draw a note, right-click to delete it. F6 goes back to the channel rack.";

const TOOLS: { tool: Tool; label: string; key: string; hint: string }[] = [
  { tool: "draw", label: "Draw", key: "KeyP", hint: "Draw: click to add a note, drag a note to move it, drag its right edge to resize it (P)." },
  { tool: "paint", label: "Paint", key: "KeyB", hint: "Paint: drag along a row to put a note in every grid cell (B)." },
  { tool: "select", label: "Select", key: "KeyE", hint: "Select: drag a box around notes, Shift adds to the selection; drag a selected note to move them all (E)." },
  { tool: "slice", label: "Slice", key: "KeyC", hint: "Slice: click a note to cut it at the nearest grid line (C)." },
  { tool: "delete", label: "Delete", key: "KeyD", hint: "Delete: click or drag over notes to delete them (D)." },
];

interface Props {
  /** The channel being edited. */
  channel: number | null;
  /** The live stream's latest frame, read every animation frame for the playhead. */
  latest?: RefObject<LiveFrame | null>;
  sampleRate?: number;
  /** The typing keyboard is on: the tools' letter shortcuts are off. */
  keyboard?: boolean;
  onError?: (message: string) => void;
  /** The notes commands (the bench passes its own). */
  api?: NotesApi;
}

/** The piano roll: the channel's notes on a WebGL2 canvas (2D without it), edited with five
 * tools, beside a column of keys that plays the channel. */
export function PianoRoll({ channel, latest, sampleRate = 48000, keyboard = false, onError = () => {}, api = API }: Props) {
  const { view: project, setView } = useProject();
  const store = useMemo(() => new NoteStore(), []);
  const queue = useMemo(() => new CommitQueue(store, api), [store, api]);
  const report = useRef(onError);
  useEffect(() => {
    report.current = onError;
    queue.setHooks({ onError, onView: setView });
  }, [queue, onError, setView]);

  const [tool, setTool] = useState<Tool>("draw");
  const [gridIndex, setGridIndex] = useState(GRIDS.indexOf(DEFAULT_GRID));
  const [use2d, setUse2d] = useState(false);
  const [empty, setEmpty] = useState(true);

  const canvas = useRef<HTMLCanvasElement>(null);
  const wrap = useRef<HTMLDivElement>(null);
  const strip = useRef<HTMLDivElement>(null);
  const renderer = useRef<RollRenderer | null>(null);
  const palette = useRef<RollPalette | null>(null);
  const view = useRef<RollView>({ tickPx: 0.1, keyPx: 14, scrollTick: 0, topKey: 84, width: 0, height: 0 });
  const centred = useRef<number | null | undefined>(undefined);
  const gesture = useRef<Gesture | null>(null);
  const lastLength = useRef(STEP_TICKS);
  const tempId = useRef(0);
  const dirty = useRef(true);
  const built = useRef<{ version: number; channel: number | null; renderer: RollRenderer | null }>({ version: -1, channel: null, renderer: null });

  const sig = project?.timeSignature ?? { beats: 4, unit: 4 };
  const grid = gridTicks(GRIDS[gridIndex], sig);
  // What the draw loop and the pointer handlers read: the latest props, without re-subscribing.
  const scene = useRef({ channel, ink: 0, sig, steps: 16, bpm: 120, sampleRate, tool, grid });
  const ink = project?.channels.find((c) => c.id === channel)?.color ?? 0;
  useEffect(() => {
    scene.current = { channel, ink, sig, steps: project?.pattern.steps ?? 16, bpm: project?.bpm ?? 120, sampleRate, tool, grid };
    dirty.current = true;
  });

  // The notes: fetched when the engine's revision moves past the store's, and after a reload.
  const revision = project?.notesRevision;
  useEffect(() => {
    if (revision !== undefined) queue.notesChanged(revision);
  }, [queue, revision]);
  useEffect(() => {
    // Outside the Tauri shell there is nothing to listen to.
    const listening = onNotesChanged((r) => queue.notesChanged(r)).catch(() => null);
    return () => void listening.then((stop) => stop?.());
  }, [queue]);
  // Another channel: nothing stays selected, and the view centres on its notes once loaded.
  useEffect(() => {
    store.setSelection([]);
    centred.current = undefined;
    gesture.current = null;
  }, [store, channel]);

  // The renderer: WebGL2 first, rebuilt when a lost context comes back; 2D for good if WebGL2
  // gives none or a lost context stays lost.
  useEffect(() => {
    const el = canvas.current;
    if (!el) return;
    let wait: ReturnType<typeof setTimeout> | undefined;
    const build = () => {
      clearTimeout(wait);
      palette.current = readRollPalette(document.documentElement);
      const made = use2d ? createCanvasRoll(el) : createRollRenderer(el);
      renderer.current = made;
      built.current = { version: -1, channel: null, renderer: made };
      dirty.current = true;
      if (made) rollStats.renderer = made.kind;
      // Deferred: a state change straight from an effect would render in a cascade.
      else if (!use2d) queueMicrotask(() => setUse2d(true));
    };
    const onLost = (e: Event) => {
      e.preventDefault();
      renderer.current = null;
      clearTimeout(wait);
      wait = setTimeout(() => setUse2d(true), RESTORE_WAIT_MS);
    };
    build();
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

  // The size, in CSS pixels; the backing store follows the device pixel ratio in the draw loop.
  useEffect(() => {
    const el = wrap.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      view.current = clampView({ ...view.current, width: el.clientWidth, height: el.clientHeight });
      dirty.current = true;
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // The draw loop. Motion comes from the animation frame's timestamp, never from a frame count,
  // so it runs the same at any refresh rate.
  useEffect(() => {
    let raf = 0;
    const playhead = new Playhead();
    let seenFrame: LiveFrame | null = null;
    let shownPlayhead: number | null = null;
    let shownEmpty: boolean | null = null;
    let keys = { top: NaN, px: NaN };
    const frame = (now: number) => {
      raf = requestAnimationFrame(frame);
      const el = canvas.current;
      const r = renderer.current;
      const p = palette.current;
      const s = scene.current;
      if (!el || !r || !p) return;
      let v = view.current;
      if (v.width <= 0 || v.height <= 0) return;

      const dpr = window.devicePixelRatio || 1;
      const w = Math.round(v.width * dpr);
      const h = Math.round(v.height * dpr);
      if (el.width !== w || el.height !== h) {
        el.width = w;
        el.height = h;
        dirty.current = true;
      }

      const b = built.current;
      if (b.version !== store.version || b.channel !== s.channel || b.renderer !== r) {
        const { data, count } = s.channel === null ? { data: new Float32Array(0), count: 0 } : buildInstances(store, s.channel);
        r.setNotes(data, count);
        rollStats.bufferBuilds++;
        built.current = { version: store.version, channel: s.channel, renderer: r };
        dirty.current = true;
        if ((count === 0) !== shownEmpty) {
          shownEmpty = count === 0;
          setEmpty(shownEmpty);
        }
        // Centre on the channel's notes the first time they are there.
        if (centred.current === undefined && store.revision > 0) {
          let lo = 128;
          let hi = -1;
          for (let i = 0; i < store.count; i++) {
            if (store.channels[i] !== s.channel) continue;
            lo = Math.min(lo, store.keys[i]);
            hi = Math.max(hi, store.keys[i]);
          }
          const mid = hi >= 0 ? (lo + hi + 1) / 2 : 60.5;
          view.current = clampView({ ...v, topKey: mid + v.height / v.keyPx / 2 });
          centred.current = s.channel;
          v = view.current;
        }
      }

      // The playhead: the last live frame carried forward by the time since it arrived.
      const live = latest?.current ?? null;
      const loop = s.steps * STEP_TICKS;
      if (live?.playing) {
        if (live !== seenFrame) {
          const heard = displayTicks(live.positionTicks, live.outputLatencyFrames, s.bpm, s.sampleRate);
          playhead.observe(Number(heard % BigInt(Math.max(1, loop))), live.receivedAt ?? now, (s.bpm * 16) / 1000);
          seenFrame = live;
        }
      } else {
        playhead.stop();
      }
      const at = playhead.at(now, loop);
      if (!dirty.current && at === shownPlayhead) return;

      const g = gesture.current;
      const box = g?.kind === "box" ? { x0: g.from.x, y0: g.from.y, x1: g.to.x, y1: g.to.y } : null;
      r.draw(
        {
          view: v,
          dpr,
          ink: s.ink,
          beatTicks: Math.max(1, Math.round(TICKS_PER_WHOLE / s.sig.unit)),
          barTicks: barTicks(s.sig),
          endTick: loop,
          drag: g?.kind === "move" ? [g.dTick, g.dKey] : [0, 0],
          resize: g?.kind === "resize" ? g.dLength : 0,
          minLength: g?.kind === "resize" ? g.minLength : 1,
          playhead: at,
          box,
        },
        p,
      );
      rollStats.draws++;
      rollStats.view = v;
      dirty.current = false;
      shownPlayhead = at;
      if ((v.topKey !== keys.top || v.keyPx !== keys.px) && strip.current) {
        keys = { top: v.topKey, px: v.keyPx };
        strip.current.style.setProperty("--key-px", `${v.keyPx}px`);
        strip.current.style.transform = `translateY(${(v.topKey - 128) * v.keyPx}px)`;
      }
    };
    raf = requestAnimationFrame(frame);
    return () => cancelAnimationFrame(raf);
  }, [store, latest]);

  // Wheel: scroll; Shift scrolls time; Cmd/Ctrl (and a pinch) zooms time about the pointer.
  useEffect(() => {
    const el = canvas.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const scale = e.deltaMode === 1 ? 16 : 1;
      const v = view.current;
      if (e.ctrlKey || e.metaKey) {
        const x = e.clientX - el.getBoundingClientRect().left;
        view.current = clampView(zoomAt(v, Math.exp(-e.deltaY * scale * 0.002), x));
      } else if (e.shiftKey) {
        view.current = scrollBy(v, (e.deltaY || e.deltaX) * scale, 0);
      } else {
        view.current = scrollBy(v, e.deltaX * scale, e.deltaY * scale);
      }
      dirty.current = true;
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [use2d]);

  // Tool letters, while the typing keyboard is off.
  useEffect(() => {
    if (keyboard) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey || e.repeat || modalIsOpen() || isTypingTarget(e.target)) return;
      const t = TOOLS.find((x) => x.key === e.code);
      if (!t) return;
      e.preventDefault();
      setTool(t.tool);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [keyboard]);

  const context = (): ToolContext | null => {
    const s = scene.current;
    if (s.channel === null) return null;
    return {
      store,
      channel: s.channel,
      tool: s.tool,
      grid: s.grid,
      lastLength: lastLength.current,
      view: view.current,
      newId: () => --tempId.current,
    };
  };
  const pointer = (e: PointerEvent<HTMLCanvasElement>): Pointer => {
    const rect = e.currentTarget.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    const w = toWorld(view.current, x, y);
    return { x, y, tick: w.tick, key: w.key, button: e.button, shift: e.shiftKey };
  };
  const take = (step: Step) => {
    if (step.edit) queue.commit(step.edit);
    if (step.select) store.setSelection(step.select);
    if (step.lastLength !== undefined) lastLength.current = step.lastLength;
    dirty.current = true;
    return step.gesture;
  };

  const onPointerDown = (e: PointerEvent<HTMLCanvasElement>) => {
    const ctx = context();
    if (!ctx || gesture.current) return;
    e.preventDefault();
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* no pointer capture here */
    }
    gesture.current = take(startGesture(ctx, pointer(e)));
  };
  const onPointerMove = (e: PointerEvent<HTMLCanvasElement>) => {
    const ctx = context();
    if (!ctx) return;
    const p = pointer(e);
    const held = gesture.current;
    if (held) {
      gesture.current = take(moveGesture(ctx, held, p));
      return;
    }
    e.currentTarget.style.cursor = cursorFor(ctx, p);
  };
  const onPointerUp = (e: PointerEvent<HTMLCanvasElement>) => {
    const ctx = context();
    const held = gesture.current;
    gesture.current = null;
    if (ctx && held) take(endGesture(ctx, held, pointer(e)));
    dirty.current = true;
  };
  const onPointerCancel = () => {
    gesture.current = null;
    dirty.current = true;
  };

  const keyAt = useCallback((y: number) => Math.floor(view.current.topKey - y / view.current.keyPx), []);
  const reportKey = useCallback((m: string) => report.current(m), []);

  return (
    <div className="roll">
      <div className="roll-tools" role="toolbar" aria-label="Piano roll tools">
        {TOOLS.map((t) => (
          <button
            key={t.tool}
            type="button"
            className="roll-tool"
            aria-pressed={tool === t.tool}
            data-hint={t.hint}
            onClick={() => setTool(t.tool)}
          >
            {t.label}
          </button>
        ))}
        <label className="roll-snap" data-hint="The grid notes snap to when they are drawn, moved, resized or sliced.">
          Snap
          <select value={gridIndex} onChange={(e) => setGridIndex(Number(e.target.value))}>
            {GRIDS.map((g, i) => (
              <option key={g.name} value={i}>
                {g.name}
              </option>
            ))}
          </select>
        </label>
      </div>
      <div className="roll-body">
        <KeyColumn channel={channel} keyAt={keyAt} strip={strip} onError={reportKey} />
        <div className="roll-wrap" ref={wrap}>
          <canvas
            key={use2d ? "2d" : "webgl2"}
            ref={canvas}
            className="roll-canvas"
            aria-label="Piano roll notes"
            data-hint={EMPTY_HINT}
            onPointerDown={onPointerDown}
            onPointerMove={onPointerMove}
            onPointerUp={onPointerUp}
            onPointerCancel={onPointerCancel}
            onContextMenu={(e) => e.preventDefault()}
          />
          {empty && channel !== null && <p className="roll-empty">{EMPTY_HINT}</p>}
        </div>
      </div>
    </div>
  );
}

/** The pointer's look over the roll: a resize arrow on a note's right edge, a hand on a note. */
function cursorFor(ctx: ToolContext, p: Pointer): string {
  if (ctx.tool === "delete" || ctx.tool === "slice") return "crosshair";
  const hit = ctx.store.hitTest(p.tick, Math.floor(p.key), ctx.channel);
  if (hit === null) return ctx.tool === "select" ? "default" : "copy";
  const n = ctx.store.get(hit)!;
  const edge = (n.tick + n.length - ctx.view.scrollTick) * ctx.view.tickPx - p.x;
  return edge <= Math.min(EDGE_PX, (n.length * ctx.view.tickPx) / 2) ? "ew-resize" : "grab";
}
