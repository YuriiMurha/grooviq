import type { WireNote } from "../api/notes";
import { snapKey } from "./scales";

/** Copied notes: positions relative to the earliest start, with no ids or channel. */
export interface Clip {
  notes: { tick: number; length: number; key: number; velocity: number; slide: boolean }[];
}

/** The notes to remember, positioned from the earliest start. */
export function copy(notes: readonly WireNote[]): Clip {
  const first = notes.reduce((m, n) => Math.min(m, n.tick), Infinity);
  return { notes: notes.map((n) => ({ tick: n.tick - first, length: n.length, key: n.key, velocity: n.velocity, slide: n.slide })) };
}

/** The clip's notes with its earliest start at `atTick`, on `channel`, each with an id from
 * `newId` (0 without one). With a `scale`, each key moves to the nearest key of it. */
export function paste(
  clip: Clip,
  atTick: number,
  channel = 0,
  newId: () => number = () => 0,
  scale?: { root: number; scale: string } | null,
): WireNote[] {
  return clip.notes.map((n) => ({
    id: newId(),
    channel,
    tick: atTick + n.tick,
    length: n.length,
    key: scale ? snapKey(n.key, scale.root, scale.scale) : n.key,
    velocity: n.velocity,
    slide: n.slide,
  }));
}

/** Copies of the selection, moved on by the whole number of bars that covers it (Mod+B, as FL's
 * duplicate): each note keeps its place in the bar, and a selection that crosses a bar line
 * covers both bars. Ids are copied; the caller gives the new notes theirs. */
export function duplicate(selection: readonly WireNote[], bar: number): WireNote[] {
  if (!selection.length) return [];
  const start = selection.reduce((m, n) => Math.min(m, n.tick), Infinity);
  const end = selection.reduce((m, n) => Math.max(m, n.tick + n.length), 0);
  const offset = (Math.floor((end - 1) / bar) - Math.floor(start / bar) + 1) * bar;
  return selection.map((n) => ({ ...n, tick: n.tick + offset }));
}
