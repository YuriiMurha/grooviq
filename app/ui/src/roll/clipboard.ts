import type { WireNote } from "../api/notes";

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
 * `newId` (0 without one). */
export function paste(clip: Clip, atTick: number, channel = 0, newId: () => number = () => 0): WireNote[] {
  return clip.notes.map((n) => ({ id: newId(), channel, tick: atTick + n.tick, length: n.length, key: n.key, velocity: n.velocity, slide: n.slide }));
}

/** Copies of the selection, moved on by whole bars so the first of them starts after the end of
 * the originals (Mod+B): each note keeps its place in the bar. Ids are copied; the caller
 * gives the new notes theirs. */
export function duplicate(selection: readonly WireNote[], bar: number): WireNote[] {
  if (!selection.length) return [];
  const start = selection.reduce((m, n) => Math.min(m, n.tick), Infinity);
  const end = selection.reduce((m, n) => Math.max(m, n.tick + n.length), 0);
  const offset = Math.max(1, Math.ceil((end - start) / bar)) * bar;
  return selection.map((n) => ({ ...n, tick: n.tick + offset }));
}
