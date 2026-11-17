import type { WireNote } from "../api/notes";
import type { LocalEdit } from "./noteStore";

// Whole-selection operations on notes: each takes notes and returns new ones in the same order,
// leaving the input alone. `changed` turns a before/after pair into the edit to commit.

/** Notes with starts moved to the nearest grid line (half way rounds up). Nothing else moves. */
export function quantize(notes: readonly WireNote[], grid: number): WireNote[] {
  const g = Math.max(1, grid);
  return notes.map((n) => ({ ...n, tick: Math.round(n.tick / g) * g }));
}

/** Notes that share a start are spread by pitch: up plays the lowest first, down the highest.
 * Voice `i` of a chord starts `i * ticksPerVoice` late; lengths stay. */
export function strum(notes: readonly WireNote[], ticksPerVoice: number, direction: "up" | "down"): WireNote[] {
  const byStart = new Map<number, number[]>();
  notes.forEach((n, i) => {
    const group = byStart.get(n.tick);
    if (group) group.push(i);
    else byStart.set(n.tick, [i]);
  });
  const out = notes.map((n) => ({ ...n }));
  for (const group of byStart.values()) {
    if (group.length < 2) continue;
    group.sort((a, b) => (direction === "up" ? notes[a].key - notes[b].key : notes[b].key - notes[a].key) || a - b);
    group.forEach((i, voice) => {
      out[i].tick = notes[i].tick + voice * ticksPerVoice;
    });
  }
  return out;
}

/** A xorshift32 generator: the same seed gives the same numbers on every platform. */
function xorshift(seed: number): () => number {
  let x = (seed | 0) || 0x9e3779b9;
  return () => {
    x ^= x << 13;
    x ^= x >>> 17;
    x ^= x << 5;
    return (x >>> 0) / 0x1_0000_0000;
  };
}

/** Each note's start moved by up to `timing` ticks either way and its velocity by up to
 * `velocity` (0..1), chosen from `seed`. Starts stay whole and at 0 or later; velocities stay in
 * the session's range, 0..1. Zero amounts leave every note as it is. */
export function humanize(notes: readonly WireNote[], timing: number, velocity: number, seed: number): WireNote[] {
  const next = xorshift(seed);
  const spread = () => next() * 2 - 1;
  return notes.map((n) => {
    // Both draws happen for every note, so a note's change does not depend on the amounts.
    const dt = spread();
    const dv = spread();
    return {
      ...n,
      tick: Math.max(0, n.tick + Math.round(dt * timing)),
      velocity: Math.min(1, Math.max(0, n.velocity + dv * velocity)),
    };
  });
}

/** The edit that turns `before` into `after` (same notes, same order), or `null` when nothing differs. */
export function changed(before: readonly WireNote[], after: readonly WireNote[]): LocalEdit | null {
  const update = after.filter((n, i) => {
    const b = before[i];
    return b.tick !== n.tick || b.length !== n.length || b.key !== n.key || b.velocity !== n.velocity || b.slide !== n.slide;
  });
  return update.length ? { add: [], update, delete: [] } : null;
}
