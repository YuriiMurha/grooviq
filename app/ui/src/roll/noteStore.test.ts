import { describe, expect, it } from "vitest";
import type { WireNote } from "../api/notes";
import { FLAG_GHOST, FLAG_SELECTED, FLAG_SLIDE, INSTANCE_STRIDE, NoteStore, buildInstances } from "./noteStore";

const note = (id: number, tick: number, length: number, key: number, channel = 1, velocity = 0.8): WireNote => ({
  id,
  channel,
  tick,
  length,
  key,
  velocity,
  slide: false,
});

/** The ids `hitTest` and `boxSelect` should give, worked out the slow way. */
function slowHit(notes: WireNote[], tick: number, key: number, channel: number) {
  const hits = notes.filter((n) => n.channel === channel && n.key === key && n.tick <= tick && tick < n.tick + n.length);
  hits.sort((a, b) => b.tick - a.tick || b.id - a.id);
  return hits[0]?.id ?? null;
}

describe("note store", () => {
  it("hit_test_finds_the_latest_note_at_a_point", () => {
    const s = new NoteStore();
    s.load(3, [
      note(1, 0, 960, 60),
      note(2, 480, 240, 60), // starts later, on top of note 1
      note(3, 240, 4000, 60), // long, starts before note 2
      note(4, 480, 240, 61),
      note(5, 500, 240, 60, 2), // another channel
    ]);
    expect(s.revision).toBe(3);
    expect(s.hitTest(600, 60, 1)).toBe(2);
    expect(s.hitTest(479, 60, 1)).toBe(3);
    expect(s.hitTest(100, 60, 1)).toBe(1);
    // The end is exclusive.
    expect(s.hitTest(720, 60, 1)).toBe(3);
    expect(s.hitTest(4240, 60, 1)).toBeNull();
    expect(s.hitTest(4239, 60, 1)).toBe(3);
    expect(s.hitTest(600, 61, 1)).toBe(4);
    expect(s.hitTest(600, 62, 1)).toBeNull();
    expect(s.hitTest(600, 60, 2)).toBe(5);
    expect(s.hitTest(600, 61, 2)).toBeNull();

    // Against the slow answer on a dense random pattern (a long note far back still hits).
    const notes: WireNote[] = [];
    let seed = 7;
    const rand = (n: number) => {
      seed = (seed * 1103515245 + 12345) >>> 0;
      return seed % n;
    };
    for (let id = 1; id <= 2000; id++) notes.push(note(id, rand(20000), 1 + rand(3000), 40 + rand(8), 1 + rand(2)));
    const dense = new NoteStore();
    dense.load(1, notes);
    for (let i = 0; i < 2000; i++) {
      const [tick, key, channel] = [rand(24000), 40 + rand(8), 1 + rand(2)];
      expect(dense.hitTest(tick, key, channel)).toBe(slowHit(notes, tick, key, channel));
    }
  });

  it("box_select_respects_the_channel", () => {
    const s = new NoteStore();
    s.load(1, [
      note(1, 0, 240, 60),
      note(2, 960, 240, 64),
      note(3, 960, 240, 64, 2),
      note(4, 100, 4000, 72), // starts before the box, ends inside it
      note(5, 2000, 240, 64),
      note(6, 960, 240, 80),
    ]);
    // Ticks [480, 1920), keys 60..=72: notes overlapping it, of channel 1.
    expect(s.boxSelect(480, 1920, 60, 72, 1).sort()).toEqual([2, 4]);
    expect(s.boxSelect(480, 1920, 60, 72, 2)).toEqual([3]);
    // Touching the end is not overlapping.
    expect(s.boxSelect(240, 480, 60, 60, 1)).toEqual([]);
    expect(s.boxSelect(0, 1, 60, 60, 1)).toEqual([1]);
    // Corners in any order.
    expect(s.boxSelect(1920, 480, 72, 60, 1).sort()).toEqual([2, 4]);
    expect(s.channelNotes(2)).toEqual([3]);
    expect(s.channelNotes(1).sort()).toEqual([1, 2, 4, 5, 6]);
  });

  it("apply_updates_buckets_and_ids", () => {
    const s = new NoteStore();
    s.load(9, [note(1, 0, 240, 60), note(2, 240, 240, 60), note(3, 480, 240, 62)]);
    const version = s.version;
    s.apply({
      add: [note(-1, 960, 480, 65)],
      update: [{ ...note(2, 1200, 240, 70), velocity: 0.5 }],
      delete: [1],
    });
    expect(s.version).toBeGreaterThan(version);
    expect(s.get(1)).toBeUndefined();
    expect(s.get(2)).toEqual({ ...note(2, 1200, 240, 70), velocity: 0.5 });
    expect(s.get(-1)).toEqual(note(-1, 960, 480, 65));
    expect(s.count).toBe(3);
    // The buckets moved with the notes.
    expect(s.hitTest(300, 60, 1)).toBeNull();
    expect(s.hitTest(10, 60, 1)).toBeNull();
    expect(s.hitTest(1300, 70, 1)).toBe(2);
    expect(s.hitTest(1000, 65, 1)).toBe(-1);
    expect(s.boxSelect(0, 2000, 0, 127, 1).sort((a, b) => a - b)).toEqual([-1, 2, 3]);
    // The revision is the engine's: a local edit does not move it.
    expect(s.revision).toBe(9);

    // A temporary id becomes the engine's id, keeping the note and its selection.
    s.setSelection([-1, 3]);
    s.remap(-1, 17);
    expect(s.get(-1)).toBeUndefined();
    expect(s.get(17)).toEqual(note(17, 960, 480, 65));
    expect(s.hitTest(1000, 65, 1)).toBe(17);
    expect(s.selectedIds().sort((a, b) => a - b)).toEqual([3, 17]);

    // Deleting the note at the end of the arrays and one in the middle keeps the rest findable.
    s.apply({ add: [], update: [], delete: [17, 2] });
    expect(s.count).toBe(1);
    expect(s.get(3)).toEqual(note(3, 480, 240, 62));
    expect(s.hitTest(500, 62, 1)).toBe(3);
    expect(s.selectedIds()).toEqual([3]);

    // A load replaces everything but keeps the selection of ids that are still there.
    s.load(12, [note(3, 0, 240, 62), note(4, 0, 240, 63)]);
    expect(s.revision).toBe(12);
    expect(s.selectedIds()).toEqual([3]);
    expect(s.count).toBe(2);
  });

  it("instances_carry_ghosts_of_other_channels_under_the_channels_own_notes", () => {
    const s = new NoteStore();
    s.load(1, [note(1, 0, 240, 60), { ...note(2, 0, 240, 60, 2), slide: true }, note(3, 480, 240, 62, 3)]);
    s.setSelection([1]);
    const plain = buildInstances(s, 1);
    expect(plain.count).toBe(1);
    expect(plain.data[4]).toBe(FLAG_SELECTED);

    const inks = [0, 0, 3, 5];
    const both = buildInstances(s, 1, { ghosts: true, inkOf: (c) => inks[c] });
    expect(both.count).toBe(3);
    const rows = Array.from({ length: 3 }, (_, i) => [...both.data.subarray(i * INSTANCE_STRIDE, (i + 1) * INSTANCE_STRIDE)]);
    // Ghosts first, so the channel's own notes draw on top; each ghost names its channel's ink.
    expect(rows.map((r) => r[4])).toEqual([
      FLAG_GHOST | FLAG_SLIDE | ((3 + 1) << 3),
      FLAG_GHOST | ((5 + 1) << 3),
      FLAG_SELECTED,
    ]);
    // A ghost is never selected, even when its id is.
    s.setSelection([1, 2]);
    expect(s.selectedNotes(1).map((n) => n.id)).toEqual([1]);
    expect(s.selectedNotes().map((n) => n.id).sort()).toEqual([1, 2]);
    expect(buildInstances(s, 1, { ghosts: true, inkOf: () => 0 }).data[4]).toBe(FLAG_GHOST | FLAG_SLIDE | (1 << 3));
  });
});
