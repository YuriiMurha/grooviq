// The sampler editor's waveform: the sample's peaks as one outline to fill, and the region's
// three markers. Positions are fractions 0..1 of the whole sample, as the engine takes `start`
// and `end`; `loop_start` is a fraction of the played region in play order
// (`grooviq_dsp::sampler`), so its marker moves with the region and can never leave it.

/** The (min, max) pairs fetched per sample: enough for a wide screen at 2x, whatever the
 * window's size, so a resize redraws from what is already there. */
export const WAVEFORM_POINTS = 2048;

// The engine stores parameters as f32 and plays a region at least one frame long: one shorter
// than that is widened to a frame when a note starts (`grooviq_dsp::sampler`), so it would not
// play what the editor shows. The markers' limits are f32 values that leave a whole frame.
const F32 = new Float32Array(1);
const BITS = new Int32Array(F32.buffer);

/** The f32 just below the one the engine stores for `x` (0 at the bottom of the range). */
export function f32Below(x: number): number {
  F32[0] = x;
  if (F32[0] <= 0) return 0;
  BITS[0] -= 1; // a positive float's next value down
  return F32[0];
}

/** The f32 just above the one the engine stores for `x` (1 at the top of the range). */
export function f32Above(x: number): number {
  F32[0] = Math.max(0, x);
  if (F32[0] >= 1) return 1;
  BITS[0] += 1;
  return F32[0];
}

/** At least a frame between `start` and `end`, counted as the engine counts it
 * (`start * len` and `end * len` in f64, `end - start >= 1`). */
const frameApart = (start: number, end: number, frames: number) => end * frames - start * frames >= 1;

/** The latest f32 start that leaves a frame before `end` (one f32 step without a length). */
function lastStart(end: number, frames: number | null): number {
  if (!frames) return f32Below(end);
  let s = Math.fround(end - 1 / frames);
  if (s <= 0) return 0;
  while (s > 0 && !frameApart(s, end, frames)) s = f32Below(s);
  return s;
}

/** The earliest f32 end that leaves a frame after `start`. */
function firstEnd(start: number, frames: number | null): number {
  if (!frames) return f32Above(start);
  let e = Math.fround(start + 1 / frames);
  if (e >= 1) return 1;
  while (e < 1 && !frameApart(start, e, frames)) e = f32Above(e);
  return e;
}

export type Marker = "start" | "end" | "loop_start";

/** The region as the parameters hold it. */
export interface Region {
  start: number;
  end: number;
  loopStart: number;
  reverse: boolean;
  /** The sample's length in frames, `null` while unknown (no sample loaded). */
  frames: number | null;
}

const clamp = (x: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, x));

/** `channel_waveform`'s bytes: little-endian (min f32, max f32) pairs. */
export function decodePeaks(buf: ArrayBuffer): Float32Array {
  const n = Math.floor(buf.byteLength / 8) * 2;
  const v = new DataView(buf);
  const out = new Float32Array(n);
  for (let i = 0; i < n; i++) out[i] = v.getFloat32(i * 4, true);
  return out;
}

/** The outline of the peaks in a `width` x `height` box, one column per whole pixel, full scale
 * at the box's edges: (x, y) pairs along the top edge (the maxima) left to right, then the
 * bottom edge (the minima) right to left. A column covers its share of the pairs, or the pair
 * under it when there are fewer pairs than columns. */
export function peaksToPoints(peaks: Float32Array, width: number, height: number): Float32Array {
  const n = Math.floor(peaks.length / 2);
  const cols = Math.floor(width);
  if (n === 0 || cols < 1) return new Float32Array(0);
  const out = new Float32Array(cols * 4);
  const half = height / 2;
  const dx = width / cols;
  for (let i = 0; i < cols; i++) {
    const a = Math.floor((i * n) / cols);
    const b = Math.max(a + 1, Math.floor(((i + 1) * n) / cols));
    let lo = Infinity;
    let hi = -Infinity;
    for (let j = a; j < b; j++) {
      lo = Math.min(lo, peaks[2 * j]);
      hi = Math.max(hi, peaks[2 * j + 1]);
    }
    const x = (i + 0.5) * dx;
    // Point i on the top edge, point (2 cols - 1 - i) on the way back.
    const top = 2 * i;
    const bottom = 2 * (2 * cols - 1 - i);
    out[top] = x;
    out[top + 1] = half * (1 - clamp(hi, -1, 1));
    out[bottom] = x;
    out[bottom + 1] = half * (1 - clamp(lo, -1, 1));
  }
  return out;
}

/** The position 0..1 of the sample under `x` in a box `width` wide. */
export function markerAt(x: number, width: number): number {
  return width > 0 ? clamp(x / width, 0, 1) : 0;
}

/** The played region's ends, in either order of the parameters, as the engine plays it. */
const played = (r: Region): [number, number] => [Math.min(r.start, r.end), Math.max(r.start, r.end)];

/** Where a marker sits, 0..1 of the sample. */
export function markerPosition(m: Marker, r: Region): number {
  if (m === "start") return r.start;
  if (m === "end") return r.end;
  const [lo, hi] = played(r);
  const at = clamp(r.loopStart, 0, 1) * (hi - lo);
  return r.reverse ? hi - at : lo + at;
}

/** The values a marker may take now: f32s that keep start at least a frame before end. A
 * region stored narrower than that (or out of order) can be outside its own range: it stays as
 * it is until a marker is moved. */
export function markerRange(m: Marker, r: Region): [number, number] {
  if (m === "start") return [0, lastStart(r.end, r.frames)];
  if (m === "end") return [firstEnd(r.start, r.frames), 1];
  return [0, 1];
}

/** A marker's value after a key moves it by `delta` (towards `delta`'s sign): kept in its range,
 * and never moved the other way (a region stored out of order or tighter than the range stays
 * where it is). */
export function nudgeMarker(m: Marker, r: Region, value: number, delta: number): number {
  const [min, max] = markerRange(m, r);
  // Stored as the engine holds it: an f32 inside the range is still inside it.
  const next = Math.fround(Math.min(max, Math.max(min, value + delta)));
  return (delta > 0 && next < value) || (delta < 0 && next > value) ? value : next;
}

/** The marker's value with the pointer at `at` (0..1 of the sample): start and end stop short
 * of each other; the loop point is that place as a fraction of the played region. */
export function dragMarker(m: Marker, at: number, r: Region): number {
  const [min, max] = markerRange(m, r);
  if (m !== "loop_start") return Math.fround(clamp(at, min, max));
  const [lo, hi] = played(r);
  if (hi - lo <= 0) return 0;
  const f = r.reverse ? (hi - at) / (hi - lo) : (at - lo) / (hi - lo);
  return Math.fround(clamp(f, 0, 1));
}
