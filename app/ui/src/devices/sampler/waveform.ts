// The sampler editor's waveform: the sample's peaks as one outline to fill, and the region's
// three markers. Positions are fractions 0..1 of the whole sample, as the engine takes `start`
// and `end`; `loop_start` is a fraction of the played region in play order
// (`grooviq_dsp::sampler`), so its marker moves with the region and can never leave it.

/** The (min, max) pairs fetched per sample: enough for a wide screen at 2x, whatever the
 * window's size, so a resize redraws from what is already there. */
export const WAVEFORM_POINTS = 2048;

/** The narrowest region the start and end markers leave between them (a fraction of the
 * sample). The engine plays at least one frame of any region. */
export const MIN_REGION = 0.001;

export type Marker = "start" | "end" | "loop_start";

/** The region as the parameters hold it. */
export interface Region {
  start: number;
  end: number;
  loopStart: number;
  reverse: boolean;
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

/** The values a marker may take now, so that start stays before end. */
export function markerRange(m: Marker, r: Region): [number, number] {
  if (m === "start") return [0, Math.max(0, r.end - MIN_REGION)];
  if (m === "end") return [Math.min(1, r.start + MIN_REGION), 1];
  return [0, 1];
}

/** The marker's value with the pointer at `at` (0..1 of the sample): start and end stop short
 * of each other; the loop point is that place as a fraction of the played region. */
export function dragMarker(m: Marker, at: number, r: Region): number {
  const [min, max] = markerRange(m, r);
  if (m !== "loop_start") return clamp(at, min, max);
  const [lo, hi] = played(r);
  if (hi - lo <= 0) return 0;
  const f = r.reverse ? (hi - at) / (hi - lo) : (at - lo) / (hi - lo);
  return clamp(f, 0, 1);
}
