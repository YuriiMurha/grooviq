import { describe, expect, it } from "vitest";
import { CHORDS, stamp } from "./chords";
import { inScale } from "./scales";

const at = { channel: 3, tick: 480, length: 240, key: 60, velocity: 0.8 };

describe("chords", () => {
  it("stamps_each_chord_and_inversion", () => {
    expect(CHORDS).toEqual({
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
    });
    const keys = (chord: string, inversion: number) => stamp(at, chord, inversion).map((n) => n.key).sort((a, b) => a - b);
    expect(keys("major", 0)).toEqual([60, 64, 67]);
    // Each inversion lifts the lowest voice an octave.
    expect(keys("major", 1)).toEqual([64, 67, 72]);
    expect(keys("major", 2)).toEqual([67, 72, 76]);
    expect(keys("min7", 3)).toEqual([70, 72, 75, 79]);
    // Every chord at every inversion has one note per voice, all at the clicked place.
    for (const chord of Object.keys(CHORDS)) {
      for (let inv = 0; inv < 4; inv++) {
        const notes = stamp(at, chord, inv);
        expect(notes).toHaveLength(CHORDS[chord].length);
        for (const n of notes) {
          expect([n.channel, n.tick, n.length, n.velocity, n.slide]).toEqual([3, 480, 240, 0.8, false]);
        }
      }
    }
    // Voices that leave the keyboard are dropped, not wrapped.
    expect(stamp({ ...at, key: 126 }, "major", 0).map((n) => n.key)).toEqual([126]);
    expect(stamp(at, "nonsense", 0)).toEqual([]);
  });

  it("scale_snap_keeps_stamped_notes_in_key", () => {
    // A major triad on D in C major is D F# A: F# is not in the key and snaps down to F.
    const notes = stamp({ ...at, key: 62 }, "major", 0, { root: 0, scale: "major" });
    expect(notes.map((n) => n.key)).toEqual([62, 65, 69]);
    // Every chord and inversion, on every key, stays in every scale it is snapped to.
    for (const chord of Object.keys(CHORDS)) {
      for (let inv = 0; inv < 4; inv++) {
        for (const scale of ["minor", "dorian", "blues", "major_pentatonic"]) {
          for (let key = 40; key < 80; key++) {
            for (const n of stamp({ ...at, key }, chord, inv, { root: 5, scale })) expect(inScale(n.key, 5, scale)).toBe(true);
          }
        }
      }
    }
    // Voices that snap onto one key become one note.
    const dim = stamp({ ...at, key: 60 }, "sus2", 0, { root: 0, scale: "major_pentatonic" });
    expect(new Set(dim.map((n) => n.key)).size).toBe(dim.length);
  });
});
