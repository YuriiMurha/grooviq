/** What the piano roll shows: zoom (pixels a tick, pixels a key row), the tick at the left edge
 * and the key at the top edge (fractional: key `k` is the row from `k + 1` down to `k`), and the
 * size in CSS pixels. */
export interface RollView {
  tickPx: number;
  keyPx: number;
  scrollTick: number;
  topKey: number;
  width: number;
  height: number;
}

export const KEYS = 128;
/** Zoom limits: a bar is about 15 px at the widest view, a tick 2 px at the closest. */
export const MIN_TICK_PX = 1 / 256;
export const MAX_TICK_PX = 2;
export const MIN_KEY_PX = 6;
export const MAX_KEY_PX = 40;

/** The world point (tick, fractional key) under a CSS pixel; `Math.floor(key)` is its row. */
export const toWorld = (v: RollView, x: number, y: number) => ({
  tick: v.scrollTick + x / v.tickPx,
  key: v.topKey - y / v.keyPx,
});

/** Where a world point is on screen, in CSS pixels. */
export const toScreen = (v: RollView, tick: number, key: number) => ({
  x: (tick - v.scrollTick) * v.tickPx,
  y: (v.topKey - key) * v.keyPx,
});

const clamp = (x: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, x));

/** Keeps the view inside the world: never before tick 0, keys inside 0..128. */
export function clampView(v: RollView): RollView {
  const keyPx = clamp(v.keyPx, MIN_KEY_PX, MAX_KEY_PX);
  const visibleKeys = v.height / keyPx;
  const topKey = visibleKeys >= KEYS ? KEYS : clamp(v.topKey, visibleKeys, KEYS);
  return { ...v, keyPx, topKey, tickPx: clamp(v.tickPx, MIN_TICK_PX, MAX_TICK_PX), scrollTick: Math.max(0, v.scrollTick) };
}

/** Zooms time by `factor` about the CSS pixel `x`: the tick under it stays under it (unless the
 * view would scroll before tick 0). */
export function zoomAt(v: RollView, factor: number, x: number): RollView {
  const tick = v.scrollTick + x / v.tickPx;
  const tickPx = clamp(v.tickPx * factor, MIN_TICK_PX, MAX_TICK_PX);
  return { ...v, tickPx, scrollTick: Math.max(0, tick - x / tickPx) };
}

/** Scrolls by CSS pixels. */
export const scrollBy = (v: RollView, dx: number, dy: number): RollView =>
  clampView({ ...v, scrollTick: v.scrollTick + dx / v.tickPx, topKey: v.topKey - dy / v.keyPx });

/** How long the playhead runs on from the last live frame before it waits for the next. */
const MAX_EXTRAPOLATE_MS = 250;

/** The playhead between live frames: the last frame's position carried forward by the time that
 * passed since it arrived, read at an animation frame's timestamp. It depends only on timestamps,
 * so it moves the same at 60 Hz, 120 Hz or any other refresh rate. */
export class Playhead {
  private tick = 0;
  private receivedAt = 0;
  private ticksPerMs = 0;
  private running = false;

  /** A live frame: the position it carries (ticks), when it arrived (ms, the clock of
   * `performance.now` and `requestAnimationFrame`) and the tempo (ticks a millisecond). */
  observe(tick: number, receivedAt: number, ticksPerMs: number) {
    this.tick = tick;
    this.receivedAt = receivedAt;
    this.ticksPerMs = ticksPerMs;
    this.running = true;
  }

  stop() {
    this.running = false;
  }

  /** The tick at time `now`, inside a loop of `loopTicks`; `null` while stopped. */
  at(now: number, loopTicks: number): number | null {
    if (!this.running) return null;
    const elapsed = clamp(now - this.receivedAt, 0, MAX_EXTRAPOLATE_MS);
    const t = this.tick + elapsed * this.ticksPerMs;
    return loopTicks > 0 ? t % loopTicks : t;
  }
}
