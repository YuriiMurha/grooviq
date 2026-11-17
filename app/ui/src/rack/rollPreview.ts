import type { WireNote } from "../api/notes";
import { FLAG_BAR, FLAG_FOCUS, STRIDE, gridSize, type GridLayout } from "./gridModel";

// A roll channel's row in the step grid: one well across the row and a thin bar per note, in the
// same records the grid's renderers draw (so it is drawn in the same pass as the steps).

/** A note bar's height, and the space kept above and below the bars, in CSS pixels. */
export const BAR_HEIGHT = 3;
const PAD = 4;
const MIN_BAR = 2;
const TICKS_PER_STEP = 240;

/** The records of row `row` for `channel`'s notes: the well first (with the focus ring if
 * `focused`), then the bars. Bars run from where a note starts to a gap before where it ends,
 * cut at the pattern's end; higher keys sit higher in the row. */
export function rollInstances(
  layout: GridLayout,
  row: number,
  notes: WireNote[],
  channel: number,
  ink: number,
  focused: boolean,
): Float32Array {
  const pitch = layout.cell + layout.gap;
  const end = layout.steps * TICKS_PER_STEP;
  const mine = notes.filter((n) => n.channel === channel && n.tick < end);
  const out = new Float32Array((1 + mine.length) * STRIDE);
  const top = row * pitch;
  out.set([0, top, gridSize(layout).width, layout.cell, ink, -1, focused ? FLAG_FOCUS : 0], 0);
  if (mine.length === 0) return out;

  let lo = 127;
  let hi = 0;
  for (const n of mine) {
    lo = Math.min(lo, n.key);
    hi = Math.max(hi, n.key);
  }
  const room = layout.cell - 2 * PAD - BAR_HEIGHT;
  let o = STRIDE;
  for (const n of mine) {
    const x = (n.tick / TICKS_PER_STEP) * pitch;
    const right = (Math.min(n.tick + n.length, end) / TICKS_PER_STEP) * pitch - layout.gap;
    const y = top + PAD + (hi === lo ? room / 2 : ((hi - n.key) / (hi - lo)) * room);
    out.set([x, y, Math.max(MIN_BAR, right - x), BAR_HEIGHT, ink, n.velocity, FLAG_BAR], o);
    o += STRIDE;
  }
  return out;
}
