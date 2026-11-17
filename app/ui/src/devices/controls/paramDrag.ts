// How a device control turns pointer, wheel and key input into a parameter value. Shared by the
// synth and sampler editors. Values are in the parameter's own units (Hz, ms, ...), clamped to
// its range; a control moves along a normalised 0..1 position, mapped by the parameter's curve.

/** `lin` evenly, `log` by ratios (Hz, ms), `sqr` finer near the bottom (glide). */
export type Curve = "lin" | "log" | "sqr";

export interface ParamRange {
  min: number;
  max: number;
  default: number;
  curve: Curve;
  /** Whole numbers only (octaves, unison voices, enums). */
  stepped: boolean;
}

/** Pixels of vertical drag for a continuous parameter's whole range. */
export const DRAG_PX = 180;
/** A stepped parameter moves at least this many pixels per step. */
const STEP_PX = 14;
/** Shift slows every gesture down by this much. */
export const FINE = 0.2;
/** One arrow key press, as a fraction of the range (Shift: a fifth of it). */
const KEY_STEP = 0.01;
/** PageUp / PageDown. */
const PAGE_STEP = 0.1;
/** One wheel notch (a delta of 100 or more). */
const WHEEL_NOTCH = 0.025;

const clamp01 = (n: number) => Math.min(1, Math.max(0, n));

/** The position 0..1 of a value on the control. */
export function toNorm(p: ParamRange, v: number): number {
  const x = Math.min(p.max, Math.max(p.min, v));
  if (p.curve === "log") return clamp01(Math.log(x / p.min) / Math.log(p.max / p.min));
  const n = clamp01((x - p.min) / (p.max - p.min));
  return p.curve === "sqr" ? Math.sqrt(n) : n;
}

/** The value at a position 0..1 (clamped), rounded to a whole number for a stepped parameter. */
export function fromNorm(p: ParamRange, n: number): number {
  const c = clamp01(n);
  const v =
    p.curve === "log" ? p.min * Math.pow(p.max / p.min, c) : p.min + (p.curve === "sqr" ? c * c : c) * (p.max - p.min);
  return quantize(p, v);
}

/** Clamps to the range; whole numbers for a stepped parameter. */
export function quantize(p: ParamRange, v: number): number {
  const x = Math.min(p.max, Math.max(p.min, Number.isFinite(v) ? v : p.default));
  return p.stepped ? Math.round(x) : x;
}

/** Pixels of drag for the whole range. */
function travel(p: ParamRange): number {
  return p.stepped ? Math.max(DRAG_PX, (p.max - p.min) * STEP_PX) : DRAG_PX;
}

/** The control's position after `dy` more pixels of vertical drag (up positive). A drag keeps
 * this unrounded position, so a stepped value moves once enough pixels have added up, and Shift
 * can come and go mid-drag. */
export function dragNorm(p: ParamRange, n: number, dy: number, fine: boolean): number {
  return clamp01(n + (dy * (fine ? FINE : 1)) / travel(p));
}

/** The value after a vertical drag of `dy` pixels (up positive) from where it started. */
export function dragValue(p: ParamRange, start: number, dy: number, fine: boolean): number {
  return fromNorm(p, dragNorm(p, toNorm(p, start), dy, fine));
}

/** The value after a key press, or `null` for a key the control does not take. A stepped
 * parameter moves one step per arrow press. */
export function keyValue(p: ParamRange, v: number, key: string, fine: boolean): number | null {
  if (key === "Home") return p.min;
  if (key === "End") return p.max;
  const dir = { ArrowUp: 1, ArrowRight: 1, ArrowDown: -1, ArrowLeft: -1, PageUp: 1, PageDown: -1 }[key];
  if (dir === undefined) return null;
  const page = key.startsWith("Page");
  if (p.stepped) return quantize(p, v + dir * (page ? Math.max(1, Math.round((p.max - p.min) * PAGE_STEP)) : 1));
  const step = page ? PAGE_STEP : KEY_STEP * (fine ? FINE : 1);
  return fromNorm(p, toNorm(p, v) + dir * step);
}

/** The value after a wheel event (`deltaY` down positive, as the browser reports it). A stepped
 * parameter moves one step per event. */
export function wheelValue(p: ParamRange, v: number, deltaY: number, fine: boolean): number {
  if (deltaY === 0) return v;
  if (p.stepped) return quantize(p, v - Math.sign(deltaY));
  // A mouse wheel sends whole notches (|delta| >= 100 or so); a trackpad many small deltas.
  const move = Math.abs(deltaY) >= 40 ? Math.sign(deltaY) * WHEEL_NOTCH : deltaY * (WHEEL_NOTCH / 40);
  return fromNorm(p, toNorm(p, v) - move * (fine ? FINE : 1));
}

/** Sends live values (the engine only, while a control moves) at most once per animation frame
 * per parameter: the latest value of each wins. */
export class LiveCoalescer {
  private pending = new Map<string, number>();
  private frame: number | null = null;

  constructor(
    private readonly send: (id: string, value: number) => void,
    private readonly schedule: (cb: () => void) => number = (cb) => requestAnimationFrame(cb),
    private readonly unschedule: (handle: number) => void = (h) => cancelAnimationFrame(h),
  ) {}

  push(id: string, value: number) {
    this.pending.set(id, value);
    this.frame ??= this.schedule(() => this.flush());
  }

  /** Drops a pending value: the gesture ended and its commit carries the value. */
  settle(id: string) {
    this.pending.delete(id);
    if (this.pending.size === 0 && this.frame !== null) {
      this.unschedule(this.frame);
      this.frame = null;
    }
  }

  /** Drops every pending value (the editor closed). */
  dispose() {
    this.pending.clear();
    if (this.frame !== null) this.unschedule(this.frame);
    this.frame = null;
  }

  private flush() {
    this.frame = null;
    const out = [...this.pending];
    this.pending.clear();
    for (const [id, v] of out) this.send(id, v);
  }
}
