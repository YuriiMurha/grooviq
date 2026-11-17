import { describe, expect, it } from "vitest";
import type { WireNote } from "../api/notes";
import { copy, duplicate, paste } from "./clipboard";

const note = (id: number, tick: number, key: number, o: Partial<WireNote> = {}): WireNote => ({
  id,
  channel: 1,
  tick,
  length: 240,
  velocity: 0.8,
  key,
  slide: false,
  ...o,
});

const BAR = 3840;

describe("clipboard", () => {
  it("duplicate_places_after_the_selection_on_the_bar_grid", () => {
    // Within the first bar: the copy starts one bar later, every note keeping its place in the bar.
    const sel = [note(1, 0, 60), note(2, 960, 64, { length: 480 }), note(3, 1920, 67, { slide: true })];
    expect(duplicate(sel, BAR).map((n) => [n.tick, n.length, n.key, n.slide])).toEqual([
      [BAR, 240, 60, false],
      [BAR + 960, 480, 64, false],
      [BAR + 1920, 240, 67, true],
    ]);
    // Inside one bar, off the bar line: 100..300 moves on by that one bar, to 3940..4140.
    const small = [note(8, 100, 60, { length: 200 })];
    expect(duplicate(small, BAR).map((n) => [n.tick, n.tick + n.length])).toEqual([[3940, 4140]]);
    // A selection that crosses a bar line covers two bars, even if it is shorter than one:
    // 3000..4000 moves on by two bars.
    const across = [note(4, 3000, 60, { length: 500 }), note(5, 3500, 62, { length: 500 })];
    expect(duplicate(across, BAR).map((n) => n.tick)).toEqual([3000 + 2 * BAR, 3500 + 2 * BAR]);
    // Ending exactly on a bar line does not cover the next bar.
    expect(duplicate([note(9, 0, 60, { length: BAR })], BAR).map((n) => n.tick)).toEqual([BAR]);
    const mid = across;
    const long = [note(6, 0, 60), note(7, 4000, 62, { length: 400 })];
    expect(duplicate(long, BAR).map((n) => n.tick)).toEqual([2 * BAR, 4000 + 2 * BAR]);
    // The copies never overlap the selection's own span.
    for (const set of [sel, mid, long, small]) {
      const end = Math.max(...set.map((n) => n.tick + n.length));
      const start = Math.min(...set.map((n) => n.tick));
      const copies = duplicate(set, BAR);
      expect(Math.min(...copies.map((n) => n.tick)) - start).toBeGreaterThanOrEqual(end - start);
    }
    expect(duplicate([], BAR)).toEqual([]);
  });

  it("paste_keeps_relative_positions", () => {
    const clip = copy([note(1, 1000, 60), note(2, 1480, 67, { velocity: 0.3, slide: true }), note(3, 1240, 64, { length: 90 })]);
    const out = paste(clip, 5000);
    // The earliest note lands at the paste point; the others keep their distance and everything else.
    expect(out.map((n) => [n.tick, n.length, n.key, n.velocity, n.slide])).toEqual([
      [5000, 240, 60, 0.8, false],
      [5480, 240, 67, 0.3, true],
      [5240, 90, 64, 0.8, false],
    ]);
    // Pasted on another channel, with ids from the caller.
    let id = 0;
    const other = paste(clip, 0, 9, () => --id);
    expect(other.map((n) => [n.channel, n.id])).toEqual([[9, -1], [9, -2], [9, -3]]);
    // An empty clip pastes nothing.
    expect(paste(copy([]), 100)).toEqual([]);
    // Into a scale: each key goes to the nearest key of it, ties down (C# to C in C major); a key
    // already in it, and one at the end of the keyboard, stay inside 0..127.
    const keyed = paste(copy([note(1, 0, 61), note(2, 0, 62), note(3, 0, 127)]), 0, 1, () => 0, { root: 0, scale: "major" });
    expect(keyed.map((n) => n.key)).toEqual([60, 62, 127]);
  });
});
