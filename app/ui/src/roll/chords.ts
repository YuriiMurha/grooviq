import type { NewNote } from "../api/notes";
import { snapKey } from "./scales";

/** The chords as semitones above the root. */
export const CHORDS: Record<string, number[]> = {
  major: [0, 4, 7],
  minor: [0, 3, 7],
  dim: [0, 3, 6],
  aug: [0, 4, 8],
  sus2: [0, 2, 7],
  sus4: [0, 5, 7],
  maj7: [0, 4, 7, 11],
  min7: [0, 3, 7, 10],
  dom7: [0, 4, 7, 10],
  add9: [0, 4, 7, 14],
};

export const CHORD_LABELS: Record<string, string> = {
  major: "Major",
  minor: "Minor",
  dim: "Dim",
  aug: "Aug",
  sus2: "Sus2",
  sus4: "Sus4",
  maj7: "Maj7",
  min7: "Min7",
  dom7: "Dom7",
  add9: "Add9",
};

/** The notes of `chord` stamped at `at` (the clicked place). Inversion `n` (0..3) lifts the
 * lowest `n` voices an octave. With a `scale`, each voice moves to the nearest scale key and
 * voices that land together become one note. Voices off the keyboard are dropped. */
export function stamp(
  at: { channel: number; tick: number; length: number; key: number; velocity: number },
  chord: string,
  inversion: number,
  scale?: { root: number; scale: string },
): NewNote[] {
  const semis = CHORDS[chord];
  if (!semis) return [];
  const lifted = Math.min(3, Math.max(0, Math.trunc(inversion)));
  const keys = semis.map((s, i) => at.key + s + (i < lifted ? 12 : 0));
  const placed = scale ? keys.map((k) => snapKey(k, scale.root, scale.scale)) : keys;
  const out: NewNote[] = [];
  const seen = new Set<number>();
  for (const key of placed) {
    if (key < 0 || key > 127 || seen.has(key)) continue;
    seen.add(key);
    out.push({ channel: at.channel, tick: at.tick, length: at.length, key, velocity: at.velocity, slide: false });
  }
  return out;
}
