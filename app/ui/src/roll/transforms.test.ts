import { describe, expect, it } from "vitest";
import type { WireNote } from "../api/notes";
import { changed, humanize, quantize, strum } from "./transforms";

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

describe("transforms", () => {
  it("quantize_moves_starts_only", () => {
    const input = [note(1, 100, 60, { length: 333, velocity: 0.3, slide: true }), note(2, 130, 62), note(3, 120, 64)];
    const out = quantize(input, 240);
    // 100 -> 0, 130 -> 240 (past half), 120 -> 240 (half rounds up, as the snap grid does).
    expect(out.map((n) => n.tick)).toEqual([0, 240, 240]);
    // Nothing but the start changed.
    expect(out[0]).toEqual({ ...note(1, 0, 60), length: 333, velocity: 0.3, slide: true });
    // The input is untouched.
    expect(input.map((n) => n.tick)).toEqual([100, 130, 120]);
  });

  it("strum_offsets_by_pitch_order", () => {
    const chord = [note(1, 480, 67), note(2, 480, 60), note(3, 480, 64), note(4, 960, 50)];
    const up = strum(chord, 30, "up");
    // Lowest first: C at 480, E at 510, G at 540; the lone note stays.
    expect(up.map((n) => [n.id, n.tick])).toEqual([[1, 540], [2, 480], [3, 510], [4, 960]]);
    const down = strum(chord, 30, "down");
    expect(down.map((n) => [n.id, n.tick])).toEqual([[1, 480], [2, 540], [3, 510], [4, 960]]);
    // Only starts moved.
    expect(up.map((n) => n.length)).toEqual([240, 240, 240, 240]);
  });

  it("humanize_is_seeded_and_clamped", () => {
    const notes = Array.from({ length: 200 }, (_, i) => note(i + 1, i * 10, 60, { velocity: i % 2 ? 0.99 : 0.01 }));
    const a = humanize(notes, 20, 0.2, 7);
    const b = humanize(notes, 20, 0.2, 7);
    const c = humanize(notes, 20, 0.2, 8);
    expect(a).toEqual(b);
    expect(a).not.toEqual(c);
    for (const [i, n] of a.entries()) {
      expect(Math.abs(n.tick - notes[i].tick)).toBeLessThanOrEqual(20);
      expect(Number.isInteger(n.tick)).toBe(true);
      expect(n.tick).toBeGreaterThanOrEqual(0);
      expect(n.velocity).toBeGreaterThanOrEqual(0);
      expect(n.velocity).toBeLessThanOrEqual(1);
    }
    // It does move things, both ways.
    expect(a.some((n, i) => n.tick > notes[i].tick)).toBe(true);
    expect(a.some((n, i) => n.tick < notes[i].tick)).toBe(true);
    // Zero amounts change nothing, even for silent and nearly silent notes.
    expect(humanize(notes, 0, 0, 3)).toEqual(notes);
    const quiet = [note(1, 10, 60, { velocity: 0 }), note(2, 10, 60, { velocity: 0.003 }), note(3, 10, 60, { velocity: 1 })];
    expect(humanize(quiet, 0, 0, 1)).toEqual(quiet);
    // Velocity only: starts stay; a silent note can only go up, a full one only down; 0 and 1 are reachable.
    const only = humanize(quiet, 0, 0.5, 5);
    expect(only.map((n) => n.tick)).toEqual([10, 10, 10]);
    expect(only[0].velocity).toBeGreaterThanOrEqual(0);
    expect(only[2].velocity).toBeLessThanOrEqual(1);
    const wide = humanize(Array.from({ length: 300 }, (_, i) => note(i, 0, 60, { velocity: 0.01 })), 0, 1, 11);
    expect(Math.min(...wide.map((n) => n.velocity))).toBeLessThan(1 / 127);
  });

  it("changed_lists_only_the_notes_that_differ", () => {
    const before = [note(1, 0, 60), note(2, 240, 62)];
    expect(changed(before, [before[0], { ...before[1], tick: 250 }])).toEqual({ add: [], update: [{ ...before[1], tick: 250 }], delete: [] });
    expect(changed(before, before)).toBeNull();
  });
});
