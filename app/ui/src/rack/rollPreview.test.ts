import { describe, expect, it } from "vitest";
import type { WireNote } from "../api/notes";
import { FLAG_BAR, FLAG_FOCUS, STRIDE, type GridLayout } from "./gridModel";
import { BAR_HEIGHT, rollInstances } from "./rollPreview";

const layout: GridLayout = { rows: 3, steps: 16, cell: 28, gap: 6, dpr: 1 };
const note = (tick: number, length: number, key: number, channel = 2, id = 1): WireNote => ({
  id,
  channel,
  tick,
  length,
  key,
  velocity: 0.8,
  slide: false,
});

/** The records as objects. */
function records(out: Float32Array) {
  const all = [];
  for (let o = 0; o < out.length; o += STRIDE) {
    all.push({ x: out[o], y: out[o + 1], w: out[o + 2], h: out[o + 3], ink: out[o + 4], velocity: out[o + 5], flags: out[o + 6] });
  }
  return all;
}

describe("roll preview", () => {
  it("bars_cover_each_note_s_steps", () => {
    const pitch = 34;
    const notes = [note(480, 960, 60), note(0, 120, 72, 2, 2), note(3600, 600, 48, 2, 3), note(0, 240, 60, 1, 4)];
    const [well, ...bars] = records(rollInstances(layout, 1, notes, 2, 4, false));
    // One well across the whole row, then a bar per note of this channel (channel 1's note is not).
    expect(well).toEqual({ x: 0, y: pitch, w: 16 * pitch - 6, h: 28, ink: 4, velocity: -1, flags: 0 });
    expect(bars).toHaveLength(3);
    for (const b of bars) {
      expect(b.flags & FLAG_BAR).toBe(FLAG_BAR);
      expect(b.ink).toBe(4);
      expect(b.h).toBe(BAR_HEIGHT);
      // Inside the row.
      expect(b.y).toBeGreaterThanOrEqual(pitch);
      expect(b.y + b.h).toBeLessThanOrEqual(pitch + 28);
    }
    const [mid, high, cut] = bars;
    // Steps 3 to 6 (ticks 480..1440): from step 2's left edge to step 5's right edge.
    expect(mid.x).toBe(2 * pitch);
    expect(mid.x + mid.w).toBe(6 * pitch - 6);
    // A bar ends a gap before where its note ends, so half a step shows as half a step less the gap.
    expect(high.x).toBe(0);
    expect(high.w).toBeCloseTo(pitch / 2 - 6, 5);
    // A note past the end is cut at the pattern's end.
    expect(cut.x).toBe(15 * pitch);
    expect(cut.x + cut.w).toBe(16 * pitch - 6);
    // Higher keys sit higher in the row.
    expect(high.y).toBeLessThan(mid.y);
    expect(mid.y).toBeLessThan(cut.y);
  });

  it("a_focused_row_rings_its_well_and_notes_past_the_end_are_left_out", () => {
    const out = records(rollInstances(layout, 0, [note(16 * 240, 240, 60)], 2, 1, true));
    expect(out).toHaveLength(1);
    expect(out[0].flags & FLAG_FOCUS).toBe(FLAG_FOCUS);
    expect(out[0].y).toBe(0);
  });
});
