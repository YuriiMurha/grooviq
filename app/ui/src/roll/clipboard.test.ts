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
    // A selection that starts mid-bar and ends mid-bar: the offset is whole bars covering it.
    const mid = [note(4, 960, 60), note(5, 4000, 62, { length: 400 })];
    // Spans 960..4400 (3440 ticks): one bar on.
    expect(duplicate(mid, BAR).map((n) => n.tick)).toEqual([960 + BAR, 4000 + BAR]);
    // Spans past a bar: two bars on.
    const long = [note(6, 0, 60), note(7, 4000, 62, { length: 400 })];
    expect(duplicate(long, BAR).map((n) => n.tick)).toEqual([2 * BAR, 4000 + 2 * BAR]);
    // The copies never overlap the selection's own span.
    for (const set of [sel, mid, long]) {
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
  });
});
