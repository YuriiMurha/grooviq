// The step grid's geometry and the per-cell records the renderers draw. Pure functions, so the
// grid's behaviour is tested without a canvas.

/** One step of one channel: `row` is the channel's index in the rack, `step` 0-based. */
export interface Cell {
  row: number;
  step: number;
}

/** The grid's shape, in CSS pixels (`dpr` turns them into device pixels). */
export interface GridLayout {
  rows: number;
  steps: number;
  /** A cell's side. */
  cell: number;
  /** The space between two cells. */
  gap: number;
  dpr: number;
}

/** The rack's cell size and gap; the channel rows use the same pitch, so rows and cells line up. */
export const CELL = 28;
export const GAP = 6;
/** The canvas reaches this far past the grid on every side, so the focus and playhead rings and
 * the second-ink shadow of an edge cell are not cut off. The cell records stay in grid space. */
export const EDGE = 4;

/** What the grid says in the hint bar away from a step. */
export const GRID_HINT =
  "The step grid: a row for each channel, a square for each sixteenth. Click a square to add a hit.";
/** The hint for one step (the copy is fixed by the spec). */
export const stepHint = (channel: string, step: number) =>
  `${channel}, step ${step + 1}. Click to add a hit, right-click to clear it, drag up or down to change its velocity.`;

/** The hint for a roll channel's row (the copy is fixed by the spec). */
export const rollHint = (channel: string) => `${channel} has piano-roll notes. Click to open the piano roll.`;

/** Floats per cell record: x, y, w, h, ink index, velocity (-1 when empty), flags. */
export const STRIDE = 7;
/** The cell has keyboard focus. */
export const FLAG_FOCUS = 1;
/** The cell is in the playhead's column. */
export const FLAG_PLAYHEAD = 2;
/** The cell is in beats 2 or 4 of a bar (the other off-step colour). */
export const FLAG_BEAT_B = 4;
/** The record is a note bar of a roll row's preview: solid ink, no second-ink shadow. */
export const FLAG_BAR = 8;

/** Steps in a beat: a step is a sixteenth. */
const BEAT_STEPS = 4;
/** Pixels of vertical drag for the whole velocity range. */
const DRAG_RANGE_PX = 100;

const pitch = (layout: GridLayout) => layout.cell + layout.gap;

/** The grid's size in CSS pixels. */
export function gridSize(layout: GridLayout): { width: number; height: number } {
  const p = pitch(layout);
  return {
    width: Math.max(0, layout.steps * p - layout.gap),
    height: Math.max(0, layout.rows * p - layout.gap),
  };
}

/** The largest side and area of the canvas's backing store, in device pixels: within what
 * WebKit and most GPUs accept for a canvas and a WebGL viewport. */
const MAX_SIDE = 8192;
const MAX_AREA = 4096 * 4096;

/** Device pixels per CSS pixel for a canvas this big: the device pixel ratio, lowered when the
 * backing store would pass those limits (the largest grids then draw a little softer). */
export function backingScale(cssWidth: number, cssHeight: number, dpr: number): number {
  let scale = dpr;
  if (cssWidth > 0) scale = Math.min(scale, MAX_SIDE / cssWidth);
  if (cssHeight > 0) scale = Math.min(scale, MAX_SIDE / cssHeight);
  if (cssWidth > 0 && cssHeight > 0) scale = Math.min(scale, Math.sqrt(MAX_AREA / (cssWidth * cssHeight)));
  return scale;
}

/** The cell under a point (CSS pixels from the grid's top left); `null` in a gap or outside. */
export function cellAt(layout: GridLayout, x: number, y: number): Cell | null {
  if (!(x >= 0) || !(y >= 0)) return null;
  const p = pitch(layout);
  const step = Math.floor(x / p);
  const row = Math.floor(y / p);
  if (step >= layout.steps || row >= layout.rows) return null;
  if (x - step * p >= layout.cell || y - row * p >= layout.cell) return null;
  return { row, step };
}

/** How much of a lit cell's height its velocity fills, from the bottom. */
export function fillHeight(height: number, velocity: number): number {
  return height * Math.min(1, Math.max(0, velocity));
}

/** One record per cell, row by row (`STRIDE` floats each). `cells[row]` may be shorter than the
 * pattern: the rest is empty. `inks[row]` is the row's ink index (0..5). A row with records in
 * `roll` (a roll channel's preview, from `rollInstances`) shows those instead of its steps. */
export function buildInstances(
  layout: GridLayout,
  cells: (number | null)[][],
  inks: number[],
  focus: Cell | null,
  playheadStep: number | null,
  roll: (Float32Array | null | undefined)[] = [],
): Float32Array {
  const { rows, steps, cell } = layout;
  const p = pitch(layout);
  let size = 0;
  for (let r = 0; r < rows; r++) size += roll[r] ? roll[r]!.length : steps * STRIDE;
  const out = new Float32Array(size);
  let o = 0;
  for (let r = 0; r < rows; r++) {
    const preview = roll[r];
    if (preview) {
      out.set(preview, o);
      o += preview.length;
      continue;
    }
    const row = cells[r];
    const ink = inks[r] ?? 0;
    const y = r * p;
    const focusStep = focus && focus.row === r ? focus.step : -1;
    for (let s = 0; s < steps; s++) {
      const v = row ? row[s] : null;
      let flags = Math.floor(s / BEAT_STEPS) % 2 === 1 ? FLAG_BEAT_B : 0;
      if (s === focusStep) flags |= FLAG_FOCUS;
      if (s === playheadStep) flags |= FLAG_PLAYHEAD;
      out[o] = s * p;
      out[o + 1] = y;
      out[o + 2] = cell;
      out[o + 3] = cell;
      out[o + 4] = ink;
      out[o + 5] = v == null ? -1 : v;
      out[o + 6] = flags;
      o += STRIDE;
    }
  }
  return out;
}

/** The velocity after dragging `dy` CSS pixels from `start` (up, a negative `dy`, raises it). */
export function velocityFromDrag(start: number, dy: number): number {
  return Math.min(1, Math.max(0, start - dy / DRAG_RANGE_PX));
}

/** Where focus goes on a key: left and right wrap within the row, up and down stop at the ends,
 * Home and End jump along the row. Any other key leaves it (brought back inside the grid). */
export function nextFocus(focus: Cell, key: string, rows: number, steps: number): Cell {
  const lastRow = Math.max(0, rows - 1);
  const lastStep = Math.max(0, steps - 1);
  const row = Math.min(Math.max(0, focus.row), lastRow);
  const step = Math.min(Math.max(0, focus.step), lastStep);
  const n = lastStep + 1;
  switch (key) {
    case "ArrowRight":
      return { row, step: (step + 1) % n };
    case "ArrowLeft":
      return { row, step: (step - 1 + n) % n };
    case "ArrowDown":
      return { row: Math.min(row + 1, lastRow), step };
    case "ArrowUp":
      return { row: Math.max(row - 1, 0), step };
    case "Home":
      return { row, step: 0 };
    case "End":
      return { row, step: lastStep };
    default:
      return { row, step };
  }
}
