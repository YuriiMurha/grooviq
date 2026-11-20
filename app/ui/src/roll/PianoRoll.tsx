import { registerGestureCancellation } from "../project/pending";
import type { PatternStamp } from "../api/patterns";
import { useCallback, useEffect, useMemo, useRef, useState, type PointerEvent, type RefObject } from "react";
import { onNotesChanged, notesEdit, patternNotes, type WireNote } from "../api/notes";
import type { ProjectKey } from "../api/project";
import type { LiveFrame } from "../live/decode";
import { useProject } from "../project/context";
import { setRollHistory } from "../project/noteHistory";
import { isProjectReplacing, onProjectReplaceFailed, onProjectReplaced, onProjectReplacing } from "../project/replacement";
import { isTypingTarget, modalIsOpen } from "../project/shortcuts";
import { displayTicks } from "../transport/position";
import { createCanvasRoll } from "./canvasRoll";
import { CommitQueue, type NotesApi } from "./commitQueue";
import { createRollRenderer, readRollPalette, type RollPalette, type RollRenderer } from "./glRoll";
import { KeyColumn } from "./KeyColumn";
import { copy, duplicate, paste, type Clip } from "./clipboard";
import { NoteStore, buildInstances } from "./noteStore";
import { useTransformAmounts } from "./useTransformAmounts";
import { RollToolbar } from "./RollToolbar";
import { scaleMask } from "./scales";
import { DEFAULT_GRID, GRIDS, barTicks, gridTicks, snapTick } from "./snap";
import { rollShortcutFor } from "./shortcuts";
import { rollStats } from "./stats";
import { endGesture, moveGesture, startGesture, EDGE_PX, TOOL_BUTTONS, heldNotes, type Gesture, type Pointer, type Step, type Tool, type ToolContext } from "./tools";
import { changed, humanize, quantize, strum } from "./transforms";
import { VelocityLane } from "./VelocityLane";
import { Playhead, clampView, scrollBy, toWorld, zoomAt, type RollView } from "./view";

const API: NotesApi = { notesEdit, patternNotes };
/** The counters (`stats.ts`) are kept outside production builds only: there this is a constant
 * `false` and the bundler drops every use. */
const STATS = import.meta.env.MODE !== "production";
/** How long a lost WebGL context may take to come back before the roll moves to 2D for good. */
const RESTORE_WAIT_MS = 2000;
const STEP_TICKS = 240;
const TICKS_PER_WHOLE = 3840;
const EMPTY_HINT = "Click to draw a note, right-click to delete it. F6 goes back to the channel rack.";

/** What Mod+C copied, kept while the roll is closed and reopened. */
let clipboard: Clip | null = null;
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
  /** Changes on every transport stop and project replacement: a held key lets go. */
  releaseKeys?: number;
}

/** The piano roll: the channel's notes on a WebGL2 canvas (2D without it), edited with five
 * tools, beside a column of keys that plays the channel. */
export function PianoRoll({ channel, latest, sampleRate = 48000, keyboard = false, onError = () => {}, api = API, releaseKeys = 0 }: Props) {
  const { view: project, setView, apply } = useProject();
  const store = useMemo(() => new NoteStore(), []);
  const queue = useMemo(() => new CommitQueue(store, api), [store, api]);
  // The project the roll edits now: every edit carries it, and the engine refuses one made for a
  // project that was replaced since.
  const generation = project?.generation ?? 0;
  useEffect(() => queue.setGeneration(generation), [queue, generation]);
  const patternId = project?.pattern.id ?? 0;
  const report = useRef(onError);
  useEffect(() => {
    report.current = onError;
    queue.setHooks({ onError, onView: setView });
  }, [queue, onError, setView]);

  const [tool, setTool] = useState<Tool>("draw");
  const [gridIndex, setGridIndex] = useState(GRIDS.indexOf(DEFAULT_GRID));
  const [use2d, setUse2d] = useState(false);
  const [empty, setEmpty] = useState(true);
  const [snapScale, setSnapScale] = useState(true);
  const [slide, setSlide] = useState(false);
  const [ghosts, setGhosts] = useState(true);
  const [lane, setLane] = useState(true);
  const [chord, setChord] = useState("");
  const [inversion, setInversion] = useState(0);

  const canvas = useRef<HTMLCanvasElement>(null);
  const wrap = useRef<HTMLDivElement>(null);
  const strip = useRef<HTMLDivElement>(null);
  const renderer = useRef<RollRenderer | null>(null);
  const palette = useRef<RollPalette | null>(null);
  const view = useRef<RollView>({ tickPx: 0.1, keyPx: 14, scrollTick: 0, topKey: 84, width: 0, height: 0 });
  const centred = useRef<number | null | undefined>(undefined);
  /** The key the view was centred on, kept in the middle as the roll is resized until the person scrolls or zooms. */
  const centreKey = useRef<number | null>(null);
  const gesture = useRef<Gesture | null>(null);
  const gestureOrigin = useRef<PatternStamp | null>(null);
  useEffect(() => {
    abandon(store, gesture.current); gesture.current = null; gestureOrigin.current = null;
    store.setSelection([]);
    queue.setPatternId(patternId);
  }, [queue, store, patternId, generation]);

  const lastLength = useRef(STEP_TICKS);
  const tempId = useRef(0);
  const dirty = useRef(true);
  useEffect(() => registerGestureCancellation(() => {
    if (gestureOrigin.current && queue.accepts(gestureOrigin.current)) abandon(store, gesture.current);
    gesture.current = null; gestureOrigin.current = null; dirty.current = true;
  }), [queue, store]);


  const built = useRef<{ version: number; channel: number | null; renderer: RollRenderer | null; ghosts: string }>({ version: -1, channel: null, renderer: null, ghosts: "" });
  // A refetch of the engine's notes must not erase what a held gesture shows.
  useEffect(() => {
    return queue.setHeld(() => {
      const held = heldNotes(gesture.current);
      return held.length ? { add: held, update: [], delete: [] } : null;
    });
  }, [queue]);
  // Undo and redo wait their turn behind this roll's edits; the queue fetches the notes after.
  useEffect(() => setRollHistory({ run: (command) => queue.run(command) }), [queue]);
  const pointerTick = useRef<number | null>(null);
  const humanizeSeed = useRef(1);
  const amounts = useTransformAmounts();

  const sig = project?.timeSignature ?? { beats: 4, unit: 4 };
  const grid = gridTicks(GRIDS[gridIndex], sig);
  // What the draw loop and the pointer handlers read: the latest props, without re-subscribing.
  const projectKey: ProjectKey | null = project?.key ?? null;
  const inkOf = useMemo(() => {
    const colours = new Map(project?.channels.map((c) => [c.id, c.color]));
    return { sig: project?.channels.map((c) => `${c.id}:${c.color}`).join(",") ?? "", of: (c: number) => colours.get(c) ?? 0 };
  }, [project?.channels]);
  const scene = useRef({
    channel,
    ink: 0,
    sig,
    steps: 16,
    bpm: 120,
    sampleRate,
    tool,
    grid,
    key: null as ProjectKey | null,
    snapScale,
    slide,
    ghosts,
    chord: null as { name: string; inversion: number } | null,
    inkOf,
  });
  const ink = project?.channels.find((c) => c.id === channel)?.color ?? 0;
  useEffect(() => {
    scene.current = {
      channel,
      ink,
      sig,
      steps: project?.pattern.steps ?? 16,
      bpm: project?.bpm ?? 120,
      sampleRate,
      tool,
      grid,
      key: projectKey,
      snapScale,
      slide,
      ghosts,
      chord: chord ? { name: chord, inversion } : null,
      inkOf,
    };
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
  // New, Open or Restore: a held gesture is let go unsent, and what the queue holds for this
  // project is dropped (its ids would land on the next project's notes). No gesture starts
  // while it is on its way (see `onPointerDown`).
  useEffect(() => {
    const before = onProjectReplacing(() => {
      abandon(store, gesture.current);
      gesture.current = null;
      dirty.current = true;
      queue.replacing();
    });
    const after = onProjectReplaced(() => queue.reopened());
    const failed = onProjectReplaceFailed(() => queue.reopened());
    return () => {
      before();
      after();
      failed();
    };
  }, [store, queue]);

  // Another channel: nothing stays selected, and the view centres on its notes once loaded.
  useEffect(() => {
    abandon(store, gesture.current);
    store.setSelection([]);
    centred.current = undefined;
    centreKey.current = null;
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
      built.current = { version: -1, channel: null, renderer: made, ghosts: "" };
      dirty.current = true;
      if (made) {
        if (STATS) rollStats.renderer = made.kind;
      } else if (!use2d) {
        // Deferred: a state change straight from an effect would render in a cascade.
        queueMicrotask(() => setUse2d(true));
      }
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
      const v = { ...view.current, width: el.clientWidth, height: el.clientHeight };
      if (centreKey.current !== null) v.topKey = centreKey.current + v.height / v.keyPx / 2;
      view.current = clampView(v);
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
      // Ghosts are in the buffer with their channels' inks: either changing rebuilds it.
      // A drag that snaps each voice to a scale moves keys by more than the shader's one offset,
      // so while one is held the buffer is built with the selection already moved, again only
      // when the snapped delta changes.
      const held = gesture.current;
      const snapped = held?.kind === "move" && s.snapScale && s.key && held.dKey !== 0 ? held : null;
      const ghostKey = `${s.ghosts ? s.inkOf.sig : ""}|${snapped ? `${snapped.dTick}:${snapped.dKey}` : ""}`;
      if (b.version !== store.version || b.channel !== s.channel || b.renderer !== r || b.ghosts !== ghostKey) {
        const moved = snapped ? { dTick: snapped.dTick, dKey: snapped.dKey, scale: s.key } : undefined;
        const { data, count, own } =
          s.channel === null ? { data: new Float32Array(0), count: 0, own: 0 } : buildInstances(store, s.channel, { ghosts: s.ghosts, inkOf: s.inkOf.of, moved });
        r.setNotes(data, count);
        if (STATS) rollStats.bufferBuilds++;
        built.current = { version: store.version, channel: s.channel, renderer: r, ghosts: ghostKey };
        dirty.current = true;
        if ((own === 0) !== shownEmpty) {
          shownEmpty = own === 0;
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
          centreKey.current = mid;
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
      const offset = g?.kind === "move" && !(s.snapScale && s.key && g.dKey !== 0);
      const box = g?.kind === "box" ? { x0: g.from.x, y0: g.from.y, x1: g.to.x, y1: g.to.y } : null;
      r.draw(
        {
          view: v,
          dpr,
          ink: s.ink,
          beatTicks: Math.max(1, Math.round(TICKS_PER_WHOLE / s.sig.unit)),
          barTicks: barTicks(s.sig),
          endTick: loop,
          scaleMask: s.key ? scaleMask(s.key.root, s.key.scale) : 0,
          drag: g?.kind === "move" && offset ? [g.dTick, g.dKey] : [0, 0],
          resize: g?.kind === "resize" ? g.dLength : 0,
          minLength: g?.kind === "resize" ? g.minLength : 1,
          playhead: at,
          box,
        },
        p,
      );
      if (STATS) {
        rollStats.draws++;
        rollStats.view = v;
        rollStats.scaleMask = s.key ? scaleMask(s.key.root, s.key.scale) : 0;
      }
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
      centreKey.current = null;
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
      const t = TOOL_BUTTONS.find((x) => x.key === e.code);
      if (!t) return;
      e.preventDefault();
      setTool(t.tool);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [keyboard]);

  const newId = () => --tempId.current;
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
      newId,
      scale: s.snapScale && s.key ? s.key : null,
      slide: s.slide,
      chord: s.chord,
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
    // A held stamp shows in the store only; the release sends it.
    const origin = gestureOrigin.current ?? queue.stamp();
    if (!queue.accepts(origin)) return null;
    if (step.preview) store.apply(step.preview);
    if (step.edit) queue.commit(step.edit, origin);
    if (step.select) store.setSelection(step.select);
    if (step.lastLength !== undefined) lastLength.current = step.lastLength;
    dirty.current = true;
    return step.gesture;
  };

  const onPointerDown = (e: PointerEvent<HTMLCanvasElement>) => {
    const ctx = context();
    // While a New, Open or Restore is on its way the old project shows, but takes no edits: a
    // gesture begun now would end in the project that replaces it.
    if (!ctx || gesture.current || isProjectReplacing()) return;
    e.preventDefault();
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* no pointer capture here */
    }
    gestureOrigin.current = queue.stamp();
    gesture.current = take(startGesture(ctx, pointer(e)));
  };
  const onPointerMove = (e: PointerEvent<HTMLCanvasElement>) => {
    const ctx = context();
    if (!ctx) return;
    const p = pointer(e);
    pointerTick.current = p.tick;
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
    if (ctx && held && gestureOrigin.current) {
      if (queue.accepts(gestureOrigin.current)) take(endGesture(ctx, held, pointer(e)));
      else queue.rejectHeld();
    }
    gestureOrigin.current = null;
    dirty.current = true;
  };
  const onPointerCancel = () => {
    if (gestureOrigin.current && queue.accepts(gestureOrigin.current)) abandon(store, gesture.current);
    gesture.current = null; gestureOrigin.current = null;
    dirty.current = true;
  };

  // What the toolbar buttons and the shortcuts do to the notes. Each works on the selected notes
  // of this channel; quantize, strum and humanize take all of its notes when none is selected.
  const worked = (): WireNote[] => {
    const ch = scene.current.channel;
    if (ch === null) return [];
    const picked = store.selectedNotes(ch);
    return picked.length ? picked : store.channelNotes(ch).map((id) => store.get(id)!);
  };
  const transform = (run: (notes: WireNote[]) => WireNote[]) => {
    const before = worked();
    const edit = changed(before, run(before));
    if (edit) queue.commit(edit);
  };
  const select = (notes: WireNote[]) => {
    store.setSelection(notes.map((n) => n.id));
    dirty.current = true;
  };
  const add = (notes: WireNote[]) => {
    if (!notes.length) return;
    queue.commit({ add: notes, update: [], delete: [] });
    select(notes);
  };
  const actions = {
    quantize: () => transform((ns) => quantize(ns, scene.current.grid)),
    strum: (direction: "up" | "down") => transform((ns) => strum(ns, amounts.values.spread, direction)),
    humanize: () => transform((ns) => humanize(ns, amounts.values.timing, amounts.values.velocity, humanizeSeed.current++)),
    selectAll: () => {
      const ch = scene.current.channel;
      if (ch !== null) store.setSelection(store.channelNotes(ch));
      dirty.current = true;
    },
    copy: () => {
      const ch = scene.current.channel;
      const picked = ch === null ? [] : store.selectedNotes(ch);
      if (picked.length) clipboard = copy(picked);
    },
    paste: () => {
      const s = scene.current;
      if (!clipboard || s.channel === null) return;
      const at = snapTick(Math.max(0, pointerTick.current ?? view.current.scrollTick), s.grid, "floor");
      add(paste(clipboard, at, s.channel, newId, s.snapScale ? s.key : null));
    },
    duplicate: () => {
      const s = scene.current;
      const picked = s.channel === null ? [] : store.selectedNotes(s.channel);
      add(duplicate(picked, barTicks(s.sig)).map((n) => ({ ...n, id: newId() })));
    },
    remove: () => {
      const ids = store.selectedNotes(scene.current.channel ?? undefined).map((n) => n.id);
      if (ids.length) queue.commit({ add: [], update: [], delete: ids });
    },
    slide: (on: boolean) => {
      setSlide(on);
      const picked = store.selectedNotes(scene.current.channel ?? undefined);
      const edit = changed(picked, picked.map((n) => ({ ...n, slide: on })));
      if (edit) queue.commit(edit);
    },
  };
  const run = useRef(actions);
  useEffect(() => {
    run.current = actions;
  });

  // Edit chords follow printed letters; Option+Q deliberately follows its physical position.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (modalIsOpen()) return;
      if ((e.target as Element | null)?.closest?.(".panel-rack")) return;
      const action = rollShortcutFor(e, navigator.platform);
      if (!action) return;
      e.preventDefault();
      run.current[action]();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const keyAt = useCallback((y: number) => Math.floor(view.current.topKey - y / view.current.keyPx), []);
  const reportKey = useCallback((m: string) => report.current(m), []);

  return (
    <div className="roll">
      <RollToolbar
        amounts={amounts}
        tool={tool}
        onTool={setTool}
        gridIndex={gridIndex}
        onGrid={setGridIndex}
        projectKey={projectKey}
        onKey={(key) => void apply({ kind: "setProjectKey", key })}
        snapScale={snapScale}
        onSnapScale={setSnapScale}
        slide={slide}
        onSlide={actions.slide}
        ghosts={ghosts}
        onGhosts={setGhosts}
        lane={lane}
        onLane={setLane}
        chord={chord}
        onChord={setChord}
        inversion={inversion}
        onInversion={setInversion}
        onQuantize={actions.quantize}
        onStrum={actions.strum}
        onHumanize={actions.humanize}
      />
      <div className="roll-body">
        <KeyColumn projectKey={project?.key ?? null} channel={channel} keyAt={keyAt} strip={strip} onError={reportKey} releaseKeys={releaseKeys} />
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
      {lane && <VelocityLane store={store} queue={queue} channel={channel} view={view} ink={ink} />}
    </div>
  );
}

/** A gesture that ends without a release takes back the notes it only showed. */
function abandon(store: NoteStore, g: Gesture | null) {
  const held = heldNotes(g);
  if (held.length) store.apply({ add: [], update: [], delete: held.map((n) => n.id) });
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
