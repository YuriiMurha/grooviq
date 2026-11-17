// The synth's sounds as the editor draws them (display only; the engine is the truth).

const TAU = 2 * Math.PI;

/** One cycle of a base shape at `p` in 0..1, as the synth's tables hold it before band
 * limiting (dsp/src/wavetable.rs): 0 sine, 1 triangle, 2 a falling saw, 3 square. Every shape
 * starts at 0 and rises, so a morph between neighbours never cancels. */
function base(k: number, p: number): number {
  if (k === 0) return Math.sin(TAU * p);
  if (k === 1) return p < 0.25 ? 4 * p : p < 0.75 ? 2 - 4 * p : 4 * p - 4;
  if (k === 2) return p === 0 ? 0 : 1 - 2 * p;
  return p === 0 || p === 0.5 ? 0 : p < 0.5 ? 1 : -1;
}

/** The oscillator at morph position `shape` (0..3: sine, triangle, saw, square), phase `p`.
 * Neighbouring shapes blend linearly, as the synth's `Morph` does. */
export function oscWave(shape: number, p: number): number {
  const s = Math.min(3, Math.max(0, shape));
  const k = Math.min(2, Math.floor(s));
  const t = s - k;
  const ph = p - Math.floor(p);
  return t === 0 ? base(k, ph) : base(k, ph) * (1 - t) + base(k + 1, ph) * t;
}

/** The LFO (dsp/src/synth.rs `Lfo`) for shapes 0..3; 4 (random) holds a new value per cycle,
 * which the display draws with `randomSteps`. */
export function lfoWave(kind: number, p: number): number {
  const ph = p - Math.floor(p);
  if (kind === 0) return Math.sin(TAU * ph);
  if (kind === 1) return base(1, ph);
  if (kind === 2) return 1 - 2 * ph;
  return ph < 0.5 ? 1 : -1;
}

/** A fixed series of held values in -1..1 for drawing the random LFO (one per cycle). */
export function randomSteps(count: number): number[] {
  let x = 0x9e3779b9;
  return Array.from({ length: count }, () => {
    x ^= x << 13;
    x ^= x >>> 17;
    x ^= x << 5;
    return ((x >>> 0) / 0xffffffff) * 2 - 1;
  });
}

/** Filter types, in the synth's order. */
export const FILTER_LP12 = 0;
export const FILTER_LP24 = 1;
export const FILTER_HP = 2;
export const FILTER_BP = 3;

/** The filter's gain in dB at `f` Hz for cutoff `fc`, resonance and drive 0..1: an analogue
 * prototype of each type, close enough to read the shape (display only). */
export function filterDb(type: number, f: number, fc: number, res: number, drive: number): number {
  const x = f / fc;
  const x2 = x * x;
  const den = (q: number) => Math.sqrt((1 - x2) * (1 - x2) + (x / q) * (x / q));
  const r = Math.pow(res, 1.5);
  let m: number;
  if (type === FILTER_LP24) m = 1 / (den(0.54) * den(1.31 + r * 10));
  else if (type === FILTER_HP) m = x2 / den(0.707 + r * 11);
  else if (type === FILTER_BP) {
    const q = 0.6 + res * 7;
    m = x / q / den(q);
  } else m = 1 / den(0.707 + r * 11);
  return 20 * Math.log10(Math.max(m, 1e-6)) + drive * 5;
}
