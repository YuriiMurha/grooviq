/** The scales' semitone sets from the root. Ids are the names in snake_case. */
export const SCALES: Record<string, number[]> = {
  major: [0, 2, 4, 5, 7, 9, 11],
  minor: [0, 2, 3, 5, 7, 8, 10],
  harmonic_minor: [0, 2, 3, 5, 7, 8, 11],
  melodic_minor: [0, 2, 3, 5, 7, 9, 11],
  dorian: [0, 2, 3, 5, 7, 9, 10],
  phrygian: [0, 1, 3, 5, 7, 8, 10],
  lydian: [0, 2, 4, 6, 7, 9, 11],
  mixolydian: [0, 2, 4, 5, 7, 9, 10],
  locrian: [0, 1, 3, 5, 6, 8, 10],
  major_pentatonic: [0, 2, 4, 7, 9],
  minor_pentatonic: [0, 3, 5, 7, 10],
  blues: [0, 3, 5, 6, 7, 10],
};

/** "harmonic_minor" → "Harmonic minor". */
export const scaleLabel = (id: string) => (id.charAt(0).toUpperCase() + id.slice(1)).replaceAll("_", " ");

export const NOTE_NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

const pitchClass = (key: number) => ((key % 12) + 12) % 12;

/** Whether `key` (a MIDI key) is a note of the scale built on `root` (0 = C). An unknown scale
 * has no notes. */
export function inScale(key: number, root: number, scale: string): boolean {
  return SCALES[scale]?.includes(pitchClass(key - root)) ?? false;
}

/** The scale key nearest to `key`, inside 0..=127; a tie goes down. A key already in the scale,
 * or an unknown scale, leaves it as it is. */
export function snapKey(key: number, root: number, scale: string): number {
  if (!SCALES[scale] || inScale(key, root, scale)) return key;
  for (let d = 1; d <= 6; d++) {
    if (key - d >= 0 && inScale(key - d, root, scale)) return key - d;
    if (key + d <= 127 && inScale(key + d, root, scale)) return key + d;
  }
  return key;
}

/** Twelve bits, bit n set when pitch class n (0 = C) is in the scale; 0 without a key. */
export function scaleMask(root: number, scale: string | null): number {
  const set = scale === null ? undefined : SCALES[scale];
  if (!set) return 0;
  let mask = 0;
  for (const s of set) mask |= 1 << pitchClass(root + s);
  return mask;
}

/** Where a note at `key` lands when moved `dKey` rows: held to the keyboard and, with a scale,
 * resolved to its nearest key (ties down). A key that does not move stays as it is. The one
 * rule for a drag's preview and its commit. */
export function moveKey(key: number, dKey: number, scale?: { root: number; scale: string } | null): number {
  if (dKey === 0) return key;
  const k = Math.min(127, Math.max(0, key + dKey));
  return scale ? snapKey(k, scale.root, scale.scale) : k;
}
