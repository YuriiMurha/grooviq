import { describe, expect, it } from "vitest";
import { decodeNotes, encodeNotes, type WireNote } from "../api/notes";
import { NoteStore, buildInstances } from "./noteStore";

// The CI budgets of the performance acceptance (Global Constraints): median of 5 runs, timed
// with performance.now, on a 10,000-note pattern.
const NOTES = 10_000;

/** 10,000 notes across 256 steps and five octaves, the same on every run. */
function seeded(channels = 1): WireNote[] {
  let s = 12345;
  const rand = (n: number) => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s % n;
  };
  return Array.from({ length: NOTES }, (_, i) => ({
    id: i + 1,
    channel: 1 + (i % channels),
    tick: rand(256 * 240),
    length: 60 + rand(900),
    key: 36 + rand(60),
    velocity: 0.8,
    slide: false,
  }));
}

/** The median time of `fn` over 5 runs, after a warm-up run. */
function median(fn: () => void): number {
  fn();
  const times: number[] = [];
  for (let i = 0; i < 5; i++) {
    const t = performance.now();
    fn();
    times.push(performance.now() - t);
  }
  times.sort((a, b) => a - b);
  return times[2];
}

const loaded = () => {
  const store = new NoteStore();
  store.load(1, seeded());
  return store;
};

describe("piano roll budgets", () => {
  it("decodes_a_ten_thousand_note_buffer_in_5_ms", () => {
    const buf = encodeNotes(1, seeded());
    let count = 0;
    const ms = median(() => {
      count = decodeNotes(buf).notes.length;
    });
    expect(count).toBe(NOTES);
    expect(ms).toBeLessThan(5);
  });

  it("builds_the_instance_buffer_in_8_ms", () => {
    const store = loaded();
    let count = 0;
    const ms = median(() => {
      count = buildInstances(store, 1).count;
    });
    expect(count).toBe(NOTES);
    expect(ms).toBeLessThan(8);
  });

  it("hit_tests_in_0_2_ms", () => {
    const store = loaded();
    // One hit test is too quick for the timer: the budget is per test, timed over 1,000.
    const points = Array.from({ length: 1000 }, (_, i) => [(i * 6151) % 61440, 36 + (i % 60)]);
    let hits = 0;
    const ms = median(() => {
      hits = 0;
      for (const [tick, key] of points) if (store.hitTest(tick, key, 1) !== null) hits++;
    });
    expect(hits).toBeGreaterThan(0);
    expect(ms / points.length).toBeLessThan(0.2);
  });

  it("box_selects_over_all_notes_in_4_ms", () => {
    const store = loaded();
    let found = 0;
    const ms = median(() => {
      found = store.boxSelect(0, 61440, 0, 127, 1).length;
    });
    expect(found).toBe(NOTES);
    expect(ms).toBeLessThan(4);
  });

  it("applies_a_thousand_note_move_in_6_ms", () => {
    const store = loaded();
    const ids = Array.from({ length: 1000 }, (_, i) => i * 10 + 1);
    const ms = median(() => {
      const update = ids.map((id) => {
        const n = store.get(id)!;
        return { ...n, tick: (n.tick + 240) % 61440, key: 36 + ((n.key - 35) % 60) };
      });
      store.apply({ add: [], update, delete: [] });
    });
    // The move landed, and the buckets were rebuilt for it.
    const moved = store.get(1)!;
    expect(store.hitTest(moved.tick, moved.key, 1)).not.toBeNull();
    expect(ms).toBeLessThan(6);
  });
});
