// Where an ADSR envelope graph draws its points, and which times a dragged point stands for.
// Each stage has a fixed share of the width; inside it, the segment grows with the control's
// position (the log time), so short and long times both stay easy to grab.

/** The share of the width for attack, decay, the sustain hold and release. */
const ZONES = [0.25, 0.27, 0.16, 0.32] as const;
/** A segment is at least this share of its zone, so a 0 ms attack is still a visible point. */
const MIN_SEG = 0.05;

const clamp01 = (n: number) => Math.min(1, Math.max(0, n));
const segment = (n: number) => MIN_SEG + (1 - MIN_SEG) * clamp01(n);
const unsegment = (share: number) => clamp01((share - MIN_SEG) / (1 - MIN_SEG));

export interface EnvBox {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** Control positions 0..1 of the three times, and the sustain level 0..1. */
export interface EnvShape {
  attack: number;
  decay: number;
  sustain: number;
  release: number;
}

export interface EnvPoints {
  /** End of the attack (the peak), of the decay, of the sustain hold and of the release. */
  xA: number;
  xD: number;
  xS: number;
  xR: number;
  /** The y of the peak, the sustain level and silence. */
  yTop: number;
  yS: number;
  yBottom: number;
}

export function envPoints(e: EnvShape, b: EnvBox): EnvPoints {
  const xA = b.left + b.width * ZONES[0] * segment(e.attack);
  const xD = xA + b.width * ZONES[1] * segment(e.decay);
  const xS = xD + b.width * ZONES[2];
  const xR = xS + b.width * ZONES[3] * segment(e.release);
  const yBottom = b.top + b.height;
  return { xA, xD, xS, xR, yTop: b.top, yS: b.top + (1 - clamp01(e.sustain)) * b.height, yBottom };
}

/** The point a drag moves: the peak (attack), the decay end (decay and sustain) or the release
 * end. */
export type EnvHandle = "attack" | "decay" | "release";

/** The handle nearest to (x, y) within `reach`, or `null`. */
export function handleAt(e: EnvShape, b: EnvBox, x: number, y: number, reach: number): EnvHandle | null {
  const p = envPoints(e, b);
  const handles: [EnvHandle, number, number][] = [
    ["attack", p.xA, p.yTop],
    ["decay", p.xD, p.yS],
    ["release", p.xR, p.yBottom],
  ];
  let best: EnvHandle | null = null;
  let bestD = reach;
  for (const [h, hx, hy] of handles) {
    const d = Math.hypot(x - hx, y - hy);
    if (d <= bestD) {
      best = h;
      bestD = d;
    }
  }
  return best;
}

/** The shape after a handle is dragged to (x, y): only what that handle stands for changes. */
export function dragHandle(e: EnvShape, b: EnvBox, h: EnvHandle, x: number, y: number): EnvShape {
  const p = envPoints(e, b);
  if (h === "attack") return { ...e, attack: unsegment((x - b.left) / (b.width * ZONES[0])) };
  if (h === "decay") {
    return {
      ...e,
      decay: unsegment((x - p.xA) / (b.width * ZONES[1])),
      sustain: clamp01(1 - (y - b.top) / b.height),
    };
  }
  return { ...e, release: unsegment((x - p.xS) / (b.width * ZONES[3])) };
}

/** The curve as (x, y) points: an attack that rises fast and eases in, exponential-looking
 * decay and release. */
export function envCurve(e: EnvShape, b: EnvBox, steps = 24): [number, number][] {
  const p = envPoints(e, b);
  const s = clamp01(e.sustain);
  const y = (level: number) => b.top + (1 - level) * b.height;
  const out: [number, number][] = [[b.left, y(0)]];
  for (let i = 1; i <= steps; i++) {
    const u = i / steps;
    out.push([b.left + (p.xA - b.left) * u, y(1 - Math.pow(1 - u, 1.6))]);
  }
  for (let i = 1; i <= steps; i++) {
    const u = i / steps;
    out.push([p.xA + (p.xD - p.xA) * u, y(s + (1 - s) * Math.pow(1 - u, 3))]);
  }
  out.push([p.xS, y(s)]);
  for (let i = 1; i <= steps; i++) {
    const u = i / steps;
    out.push([p.xS + (p.xR - p.xS) * u, y(s * Math.pow(1 - u, 3))]);
  }
  return out;
}
