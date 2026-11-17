import { describe, expect, it } from "vitest";
import { decodeNotes, encodeNotes, type WireNote } from "../api/notes";
import { NoteStore, buildInstances } from "./noteStore";

// Normal suites guard counted work. GROOVIQ_ROLL_PERF=1 runs the original timing
// acceptance separately, without instrumentation or concurrent UI tests.
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

const performanceGate = process.env.GROOVIQ_ROLL_PERF === "1";
let work = 0;

// Observe actual data access, rather than trusting a counter increment in the implementation.
// Bind native methods to their typed array; charge bulk operations by their input size.
function counted(store: NoteStore): NoteStore {
  if (performanceGate) return store;
  for (const key of Object.keys(store)) {
    const value = Reflect.get(store, key);
    if (!(value instanceof Float64Array || value instanceof Float32Array || value instanceof Uint32Array
      || value instanceof Int32Array || value instanceof Uint8Array)) continue;
    Reflect.set(store, key, new Proxy(value, {
      get(target, property) {
        if (typeof property === "string" && /^\d+$/.test(property)) work++;
        const member = Reflect.get(target, property, target);
        if (typeof member !== "function") return member;
        return (...args: unknown[]) => {
          work += property === "subarray"
            ? Math.max(0, Number(args[1] ?? target.length) - Number(args[0] ?? 0))
            : target.length;
          return Reflect.apply(member, target, args);
        };
      },
    }));
  }
  return store;
}

// Native searches on plain arrays and subarray views bypass the store proxies. Charge
// their full input length; install only around fn, so assertions do not count test-runner work.
function countNativeWork(): () => void {
  const restore: (() => void)[] = [];
  const wrap = (prototype: object, name: string, byLength: boolean) => {
    const descriptor = Object.getOwnPropertyDescriptor(prototype, name);
    if (!descriptor || typeof descriptor.value !== "function") return;
    const original = descriptor.value;
    Object.defineProperty(prototype, name, {
      ...descriptor,
      value: function (this: { length: number }, ...args: unknown[]) {
        work += byLength ? this.length : 1;
        return Reflect.apply(original, this, args);
      },
    });
    restore.push(() => { Object.defineProperty(prototype, name, descriptor); });
  };
  for (const name of ["getUint32", "getFloat32", "getUint8"]) wrap(DataView.prototype, name, false);
  for (const prototype of [Array.prototype, Object.getPrototypeOf(Int32Array.prototype)]) {
    for (const name of ["includes", "indexOf", "lastIndexOf", "find", "findIndex", "some", "filter", "splice", "unshift", "shift"]) {
      wrap(prototype, name, true);
    }
  }
  return () => { for (const undo of restore) undo(); };
}

/** Wall-clock acceptance is opt-in; normal tests enforce bounded observed work. */
function measure(fn: () => void, milliseconds: number, maxWork: number) {
  if (!performanceGate) {
    const restore = countNativeWork();
    work = 0;
    try {
      fn();
    } finally {
      restore();
    }
    expect(work).toBeGreaterThan(0);
    expect(work).toBeLessThanOrEqual(maxWork);
    return;
  }
  fn();
  const times: number[] = [];
  for (let i = 0; i < 5; i++) {
    const t = performance.now();
    fn();
    times.push(performance.now() - t);
  }
  times.sort((a, b) => a - b);
  console.log("roll timing samples (ms)", times);
  expect(times[2]).toBeLessThan(milliseconds);
}

const loaded = () => {
  const store = new NoteStore();
  store.load(1, seeded());
  return counted(store);
};

describe("piano roll budgets", () => {
  it("decodes_a_ten_thousand_note_buffer_in_5_ms", () => {
    const buf = encodeNotes(1, seeded());
    let count = 0;
    measure(() => {
      count = decodeNotes(buf).notes.length;
    }, 5, 8 * NOTES);
    expect(count).toBe(NOTES);
  });

  it("builds_the_instance_buffer_in_8_ms", () => {
    const store = loaded();
    let count = 0;
    measure(() => {
      count = buildInstances(store, 1).count;
    }, 8, 12 * NOTES);
    expect(count).toBe(NOTES);
  });

  it("builds_the_instance_buffer_with_ghosts_in_8_ms", () => {
    // The same ten thousand notes over four channels: this channel's own plus every other one as a ghost.
    const store = new NoteStore();
    store.load(1, seeded(4));
    counted(store);
    let count = 0;
    measure(() => {
      count = buildInstances(store, 1, { ghosts: true, inkOf: (c) => c % 6 }).count;
    }, 8, 16 * NOTES);
    expect(count).toBe(NOTES);
  });

  it("hit_tests_in_0_2_ms", () => {
    const store = loaded();
    // One hit test is too quick for the timer: the budget is per test, timed over 1,000.
    const points = Array.from({ length: 1000 }, (_, i) => [(i * 6151) % 61440, 36 + (i % 60)]);
    let hits = 0;
    measure(() => {
      hits = 0;
      for (const [tick, key] of points) if (store.hitTest(tick, key, 1) !== null) hits++;
    }, 0.2 * points.length, 100 * points.length);
    expect(hits).toBeGreaterThan(0);
  });

  it("box_selects_over_all_notes_in_4_ms", () => {
    const store = loaded();
    let found = 0;
    measure(() => {
      found = store.boxSelect(0, 61440, 0, 127, 1).length;
    }, 4, 8 * NOTES);
    expect(found).toBe(NOTES);
  });

  it("applies_a_thousand_note_move_in_6_ms", () => {
    const store = loaded();
    const ids = Array.from({ length: 1000 }, (_, i) => i * 10 + 1);
    let update: WireNote[] = [];
    measure(() => {
      update = ids.map((id) => {
        const n = store.get(id)!;
        return { ...n, tick: (n.tick + 240) % 61440, key: 36 + ((n.key - 35) % 60) };
      });
      store.apply({ add: [], update, delete: [] });
    }, 6, 40 * NOTES);
    for (const note of update) expect(store.get(note.id)).toEqual(note);
    // The move landed, and the buckets were rebuilt for it.
    const moved = store.get(1)!;
    expect(store.hitTest(moved.tick, moved.key, 1)).not.toBeNull();
  });
});
