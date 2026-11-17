import { describe, expect, it } from "vitest";
import { SCALES, inScale, moveKey, scaleMask, snapKey } from "./scales";

describe("scales", () => {
  it("every_scale_has_its_semitones", () => {
    expect(SCALES).toEqual({
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
    });
    // Rooted anywhere: D major has F# and C#, not F.
    expect([66, 61, 65].map((k) => inScale(k, 2, "major"))).toEqual([true, true, false]);
    // Keys below the root still count by pitch class.
    expect(inScale(1, 2, "major")).toBe(true);
  });

  it("snap_key_picks_the_nearest_and_ties_go_down", () => {
    // C major: C# sits between C and D (tie: down), F# between F and G (tie: down).
    expect(snapKey(61, 0, "major")).toBe(60);
    expect(snapKey(66, 0, "major")).toBe(65);
    // Nearer one wins: A# in C major is a tie between A and B too, so down; in A minor pentatonic
    // (A C D E G) the F is nearer E (1) than G (2).
    expect(snapKey(70, 0, "major")).toBe(69);
    expect(snapKey(65, 9, "minor_pentatonic")).toBe(64);
    // In the scale already: unchanged.
    expect(snapKey(64, 0, "major")).toBe(64);
    // The ends of the keyboard: never leaves 0..127.
    expect(snapKey(1, 0, "major")).toBe(0);
    expect(snapKey(127, 1, "major")).toBe(126);
    // C is not in D major: with nothing below it snaps up to C#.
    expect(snapKey(0, 2, "major")).toBe(1);
    // Every result of every scale on every root is in the scale and in range.
    for (const scale of Object.keys(SCALES)) {
      for (let root = 0; root < 12; root++) {
        for (let k = 0; k < 128; k++) {
          const s = snapKey(k, root, scale);
          expect(inScale(s, root, scale)).toBe(true);
          expect(s).toBeGreaterThanOrEqual(0);
          expect(s).toBeLessThanOrEqual(127);
        }
      }
    }
  });

  it("move_key_shifts_and_resolves_into_the_scale_inside_the_keyboard", () => {
    const c = { root: 0, scale: "major" };
    expect(moveKey(64, 2, c)).toBe(65);
    expect(moveKey(60, 2, c)).toBe(62);
    // No scale: chromatic, held to the keyboard.
    expect(moveKey(64, 2)).toBe(66);
    expect(moveKey(125, 9)).toBe(127);
    expect(moveKey(3, -9)).toBe(0);
    // A key that does not move stays, in the scale or not.
    expect(moveKey(61, 0, c)).toBe(61);
    // Moved results are always in the scale and in range.
    for (let k = 0; k < 128; k++) for (let d = -127; d <= 127; d += 7) {
      const m = moveKey(k, d, c);
      expect(m).toBeGreaterThanOrEqual(0);
      expect(m).toBeLessThanOrEqual(127);
      if (d !== 0) expect(inScale(m, 0, "major")).toBe(true);
    }
  });

  it("the_mask_has_a_bit_per_pitch_class_in_the_scale", () => {
    // C major: C D E F G A B = bits 0 2 4 5 7 9 11.
    expect(scaleMask(0, "major")).toBe(0b101010110101);
    // Rotated to D: D E F# G A B C# = bits 2 4 6 7 9 11 1.
    expect(scaleMask(2, "major")).toBe(0b101011010110);
    // No key, no mask.
    expect(scaleMask(0, null)).toBe(0);
    expect(scaleMask(0, "nonsense")).toBe(0);
  });
});
