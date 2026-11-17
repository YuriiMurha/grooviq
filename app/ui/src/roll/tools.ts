import type { WireNote } from "../api/notes";
import { stamp } from "./chords";
import type { LocalEdit, NoteStore } from "./noteStore";
import { snapKey } from "./scales";
import { snapTick } from "./snap";
import type { RollView } from "./view";

// The piano roll's tools as pure gesture reducers: a pointer goes down (`startGesture`), moves
// (`moveGesture`) and comes up (`endGesture`); each step returns the gesture so far and, when it
// changes the notes, a `LocalEdit`. A drag of the selection only grows a delta (the renderer
// offsets the selected notes by it); the notes change once, at the end.

export type Tool = "draw" | "paint" | "select" | "slice" | "delete";

/** The tool buttons: the letter that picks each (by `KeyboardEvent.code`) and its hint. */
export const TOOL_BUTTONS: { tool: Tool; label: string; key: string; hint: string }[] = [
  { tool: "draw", label: "Draw", key: "KeyP", hint: "Draw: click to add a note, drag a note to move it, drag its right edge to resize it (P)." },
  { tool: "paint", label: "Paint", key: "KeyB", hint: "Paint: drag along a row to put a note in every grid cell (B)." },
  { tool: "select", label: "Select", key: "KeyE", hint: "Select: drag a box around notes, Shift adds to the selection; drag a selected note to move them all (E)." },
  { tool: "slice", label: "Slice", key: "KeyC", hint: "Slice: click a note to cut it at the nearest grid line (C)." },
  { tool: "delete", label: "Delete", key: "KeyD", hint: "Delete: click or drag over notes to delete them (D)." },
];

/** The velocity of a drawn or painted note. */
export const DRAW_VELOCITY = 0.8;
/** The resize zone at a note's right edge, in CSS pixels. */
export const EDGE_PX = 6;
/** How far the pointer moves before a press on a note becomes a drag. */
const DRAG_PX = 3;
const TOP_KEY = 127;

/** A pointer over the roll: CSS pixels, the world point under it (`key` fractional: its row is
 * `Math.floor(key)`), the button (0 left, 2 right) and Shift. */
export interface Pointer {
  x: number;
  y: number;
  tick: number;
  key: number;
  button: number;
  shift: boolean;
}

export interface ToolContext {
  store: NoteStore;
  /** The channel being edited. */
  channel: number;
  tool: Tool;
  /** The snap grid, in ticks. */
  grid: number;
  /** The length a drawn note gets: the last one grabbed or resized (one step at first). */
  lastLength: number;
  view: RollView;
  /** A new temporary (negative) id. */
  newId: () => number;
  /** Snap to this scale: drawn, painted and moved notes land on its keys. */
  scale?: { root: number; scale: string } | null;
  /** New notes are slide notes. */
  slide?: boolean;
  /** Drawing stamps this chord instead of one note. */
  chord?: { name: string; inversion: number } | null;
}

export type Gesture =
  | { kind: "none" }
  /** Right button (or the delete tool): deletes every note it passes over. */
  | { kind: "erase" }
  /** Moves the selection: `dTick`, `dKey` so far. `anchor` is the grabbed note's start. */
  | { kind: "move"; from: Pointer; anchor: number; anchorKey: number; dTick: number; dKey: number; minDTick: number; minDKey: number; maxDKey: number; moved: boolean }
  /** Resizes the selection from the grabbed note's right edge (`end`): `dLength` so far. */
  | { kind: "resize"; from: Pointer; id: number; end: number; dLength: number; minLength: number; moved: boolean }
  /** Paints a note into each grid cell of the row `key`; `cell` is the last one visited. */
  | { kind: "paint"; key: number; cell: number }
  /** A selection rectangle. */
  | { kind: "box"; from: Pointer; to: Pointer; additive: boolean };

export interface Step {
  gesture: Gesture;
  edit: LocalEdit | null;
  /** The new selection, when it changes. */
  select?: number[];
  /** The new last length, when it changes. */
  lastLength?: number;
}

const NONE: Step = { gesture: { kind: "none" }, edit: null };
const edit = (e: Partial<LocalEdit>): LocalEdit => ({ add: e.add ?? [], update: e.update ?? [], delete: e.delete ?? [] });
const row = (p: Pointer) => Math.floor(p.key);

function erase(ctx: ToolContext, p: Pointer): LocalEdit | null {
  const hit = ctx.store.hitTest(p.tick, row(p), ctx.channel);
  return hit === null ? null : edit({ delete: [hit] });
}

/** How far the notes `ids` (or the selected notes) may move. */
function moveLimits(store: NoteStore, ids: number[] | null) {
  let minTick = Infinity;
  let minKey = TOP_KEY;
  let maxKey = 0;
  const take = (tick: number, key: number) => {
    if (tick < minTick) minTick = tick;
    if (key < minKey) minKey = key;
    if (key > maxKey) maxKey = key;
  };
  if (ids) {
    for (const id of ids) {
      const n = store.get(id);
      if (n) take(n.tick, n.key);
    }
  } else {
    for (let i = 0; i < store.count; i++) if (store.selected[i]) take(store.ticks[i], store.keys[i]);
  }
  if (minTick === Infinity) return { minDTick: 0, minDKey: 0, maxDKey: 0 };
  return { minDTick: -minTick, minDKey: -minKey, maxDKey: TOP_KEY - maxKey };
}

function moveOf(from: Pointer, anchor: number, anchorKey: number, limits: ReturnType<typeof moveLimits>): Gesture {
  return { kind: "move", from, anchor, anchorKey, dTick: 0, dKey: 0, ...limits, moved: false };
}

/** A row, moved to the nearest key of the scale when snapping to one. */
const inKey = (ctx: ToolContext, key: number) => (ctx.scale ? snapKey(key, ctx.scale.root, ctx.scale.scale) : key);

/** The cell `c`'s note for a paint stroke in `key`, unless a note of the channel sounds there. */
function paintCell(ctx: ToolContext, key: number, c: number): WireNote | null {
  const tick = c * ctx.grid;
  if (c < 0 || ctx.store.hitTest(tick, key, ctx.channel) !== null) return null;
  return { id: ctx.newId(), channel: ctx.channel, tick, length: ctx.grid, key, velocity: DRAW_VELOCITY, slide: ctx.slide ?? false };
}

function slice(ctx: ToolContext, id: number, p: Pointer): LocalEdit | null {
  const n = ctx.store.get(id);
  if (!n) return null;
  const at = snapTick(p.tick, ctx.grid, "round");
  if (at <= n.tick || at >= n.tick + n.length) return null;
  return edit({
    update: [{ ...n, length: at - n.tick }],
    add: [{ ...n, id: ctx.newId(), tick: at, length: n.tick + n.length - at, slide: false }],
  });
}

export function startGesture(ctx: ToolContext, p: Pointer): Step {
  const { store, tool } = ctx;
  if (p.button === 2 || (p.button === 0 && tool === "delete")) return { gesture: { kind: "erase" }, edit: erase(ctx, p) };
  const key = row(p);
  if (p.button !== 0 || key < 0 || key > TOP_KEY || p.tick < 0) return NONE;

  const hit = store.hitTest(p.tick, key, ctx.channel);
  if (tool === "slice") return hit === null ? NONE : { gesture: { kind: "none" }, edit: slice(ctx, hit, p) };

  if (hit !== null) {
    const n = store.get(hit)!;
    const was = store.isSelected(hit);
    let select: number[] | undefined;
    if (p.shift) {
      const current = store.selectedIds();
      if (was) return { gesture: { kind: "none" }, edit: null, select: current.filter((id) => id !== hit) };
      select = [...current, hit];
    } else if (!was) {
      select = [hit];
    }
    const lastLength = tool === "draw" || tool === "paint" ? n.length : undefined;
    const width = n.length * ctx.view.tickPx;
    const edgeX = (n.tick + n.length - ctx.view.scrollTick) * ctx.view.tickPx;
    if (edgeX - p.x <= Math.min(EDGE_PX, width / 2)) {
      const gesture: Gesture = { kind: "resize", from: p, id: hit, end: n.tick + n.length, dLength: 0, minLength: Math.max(1, ctx.grid), moved: false };
      return { gesture, edit: null, select, lastLength };
    }
    return { gesture: moveOf(p, n.tick, n.key, moveLimits(store, select ?? null)), edit: null, select, lastLength };
  }

  if (tool === "select") return { gesture: { kind: "box", from: p, to: p, additive: p.shift }, edit: null };
  if (tool === "paint") {
    const row = inKey(ctx, key);
    const c = Math.floor(p.tick / ctx.grid);
    const n = paintCell(ctx, row, c);
    return { gesture: { kind: "paint", key: row, cell: c }, edit: n ? edit({ add: [n] }) : null };
  }
  // Draw: a note (or a chord) under the pointer, selected, and held for a drag.
  const tick = snapTick(p.tick, ctx.grid, "floor");
  const root = inKey(ctx, key);
  const at = { channel: ctx.channel, tick, length: ctx.lastLength, key: root, velocity: DRAW_VELOCITY };
  const added = ctx.chord ? stamp(at, ctx.chord.name, ctx.chord.inversion, ctx.scale ?? undefined) : [{ ...at, slide: false }];
  const notes: WireNote[] = added.map((a) => ({ ...a, id: ctx.newId(), slide: ctx.slide ?? false }));
  if (!notes.length) return NONE;
  const lo = Math.min(...notes.map((n) => n.key));
  const hi = Math.max(...notes.map((n) => n.key));
  const limits = { minDTick: -tick, minDKey: -lo, maxDKey: TOP_KEY - hi };
  return { gesture: moveOf(p, tick, root, limits), edit: edit({ add: notes }), select: notes.map((n) => n.id) };
}

const clamp = (x: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, x));
const hasMoved = (from: Pointer, p: Pointer) => Math.abs(p.x - from.x) >= DRAG_PX || Math.abs(p.y - from.y) >= DRAG_PX;

export function moveGesture(ctx: ToolContext, g: Gesture, p: Pointer): Step {
  switch (g.kind) {
    case "none":
      return { gesture: g, edit: null };
    case "erase":
      return { gesture: g, edit: erase(ctx, p) };
    case "move": {
      const moved = g.moved || hasMoved(g.from, p);
      if (!moved) return { gesture: g, edit: null };
      const start = snapTick(g.anchor + (p.tick - g.from.tick), ctx.grid, "round");
      const dTick = Math.max(start - g.anchor, g.minDTick);
      const dKey = clamp(inKey(ctx, clamp(g.anchorKey + row(p) - row(g.from), 0, TOP_KEY)) - g.anchorKey, g.minDKey, g.maxDKey);
      return { gesture: { ...g, moved, dTick, dKey }, edit: null };
    }
    case "resize": {
      const moved = g.moved || hasMoved(g.from, p);
      if (!moved) return { gesture: g, edit: null };
      const end = snapTick(g.end + (p.tick - g.from.tick), ctx.grid, "round");
      return { gesture: { ...g, moved, dLength: end - g.end }, edit: null };
    }
    case "paint": {
      const c = Math.floor(p.tick / ctx.grid);
      if (c === g.cell) return { gesture: g, edit: null };
      const dir = c > g.cell ? 1 : -1;
      const add: WireNote[] = [];
      for (let i = g.cell + dir; i !== c + dir; i += dir) {
        const n = paintCell(ctx, g.key, i);
        // Notes painted in this step are not in the store yet: never twice in one cell.
        if (n && !add.some((a) => a.tick === n.tick)) add.push(n);
      }
      return { gesture: { ...g, cell: c }, edit: add.length ? edit({ add }) : null };
    }
    case "box":
      return { gesture: { ...g, to: p }, edit: null };
  }
}

export function endGesture(ctx: ToolContext, g: Gesture, p: Pointer): Step {
  const last = moveGesture(ctx, g, p);
  const { store } = ctx;
  const done = last.gesture;
  switch (done.kind) {
    case "move": {
      if (done.dTick === 0 && done.dKey === 0) return { gesture: done, edit: null };
      const update: WireNote[] = [];
      for (let i = 0; i < store.count; i++) {
        if (!store.selected[i]) continue;
        const n = store.note(i);
        update.push({ ...n, tick: n.tick + done.dTick, key: n.key + done.dKey });
      }
      return { gesture: done, edit: edit({ update }) };
    }
    case "resize": {
      if (done.dLength === 0) return { gesture: done, edit: null };
      const update: WireNote[] = [];
      let lastLength: number | undefined;
      for (let i = 0; i < store.count; i++) {
        if (!store.selected[i]) continue;
        const n = store.note(i);
        const length = Math.max(done.minLength, n.length + done.dLength);
        if (n.id === done.id) lastLength = length;
        update.push({ ...n, length });
      }
      return { gesture: done, edit: edit({ update }), lastLength };
    }
    case "box": {
      const a = done.from;
      const ids = store.boxSelect(a.tick, p.tick, row(a), row(p), ctx.channel);
      const select = done.additive ? [...new Set([...store.selectedIds(), ...ids])] : ids;
      return { gesture: done, edit: null, select };
    }
    default:
      return last;
  }
}
