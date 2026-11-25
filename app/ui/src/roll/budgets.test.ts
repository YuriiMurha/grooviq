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
type NoteArray = Float64Array | Float32Array | Uint32Array | Int32Array | Uint8Array;
const isNoteArray = (value: unknown): value is NoteArray =>
  value instanceof Float64Array || value instanceof Float32Array || value instanceof Uint32Array
  || value instanceof Int32Array || value instanceof Uint8Array;

const observedArrays = new WeakMap<NoteArray, NoteArray>();
function observeArray(value: NoteArray): NoteArray {
  const known = observedArrays.get(value);
  if (known) return known;
  const observed = new Proxy(value, arrayWork);
  observedArrays.set(value, observed);
  observedArrays.set(observed, observed);
  return observed;
}

const arrayWork: ProxyHandler<NoteArray> = {
  get(target, property) {
    if (property === "buffer") throw new Error("Raw backing buffers are unsupported in counted mode");
    if (typeof property === "string" && /^\d+$/.test(property)) work++;
    const member = Reflect.get(target, property, target);
    if (typeof member !== "function") return member;
    return (...args: unknown[]) => {
      work += property === "subarray"
        ? Math.max(0, Number(args[1] ?? target.length) - Number(args[0] ?? 0))
        : target.length;
      const callback = args[0];
      if (typeof callback === "function") {
        args[0] = function (this: unknown, ...callbackArgs: unknown[]) {
          // forEach/map pass the array third; reduce passes it fourth. Keep callback
          // receivers intact while replacing any raw receiver alias with its observer.
          for (let i = 0; i < callbackArgs.length; i++) {
            if (callbackArgs[i] === target) callbackArgs[i] = observeArray(target);
          }
          return Reflect.apply(callback, this, callbackArgs);
        };
      }
      const result = Reflect.apply(member, target, args);
      // Methods can return the receiver (sort/fill/reverse) or a new typed array. Observe
      // either, preserving receiver identity while native calls still use the raw target.
      return isNoteArray(result) ? observeArray(result) : result;
    };
  },
};

function counted(store: NoteStore): NoteStore {
  if (performanceGate) return store;
  for (const key of Object.keys(store)) {
    const value = Reflect.get(store, key);
    if (!isNoteArray(value)) continue;
    Reflect.set(store, key, observeArray(value));
  }
  return store;
}

// Native searches on plain arrays and subarray views bypass the store proxies. Charge
// their full input length; install only around fn, so assertions do not count test-runner work.
function countNativeWork(): () => void {
  const restore: (() => void)[] = [];
  const wrap = (prototype: object, name: PropertyKey, weight: (receiver: object) => number) => {
    const descriptor = Object.getOwnPropertyDescriptor(prototype, name);
    if (!descriptor || typeof descriptor.value !== "function") return;
    const original = descriptor.value;
    Object.defineProperty(prototype, name, {
      ...descriptor,
      value: function (this: object, ...args: unknown[]) {
        work += weight(this);
        return Reflect.apply(original, this, args);
      },
    });
    restore.push(() => { Object.defineProperty(prototype, name, descriptor); });
  };
  for (const name of ["getUint32", "getFloat32", "getUint8"]) wrap(DataView.prototype, name, () => 1);
  // Constructors are not traversals. subarray creates a constant-time view; reads of
  // store views are already counted by arrayWork. Iterator creation is charged up front.
  const constant = new Set<PropertyKey>(["constructor", "at", "push", "pop", "subarray"]);
  for (const prototype of [Array.prototype, Object.getPrototypeOf(Int32Array.prototype)]) {
    for (const name of Reflect.ownKeys(prototype)) {
      if (!constant.has(name)) wrap(prototype, name, (receiver) => Number(Reflect.get(receiver, "length")));
    }
  }
  for (const prototype of [Map.prototype, Set.prototype]) {
    for (const name of [Symbol.iterator, "entries", "keys", "values", "forEach"]) {
      wrap(prototype, name, (receiver) => Number(Reflect.get(receiver, "size")));
    }
  }
  // Avoid charging cleanup's own array iteration after the measured operation ends.
  return () => { for (let i = restore.length - 1; i >= 0; i--) restore[i](); };
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

function nativeWorkDelta(run: () => void): number {
  const restore = countNativeWork();
  const before = work;
  try {
    run();
    return work - before;
  } finally {
    restore();
  }
}

describe.skipIf(performanceGate)("counted native work", () => {
  // Two sizes, so a constant weight cannot pass for both.
  const sized = (size: number) => Array.from({ length: size }, (_, i) => 2 * (i + 1));

  it.each([3, 5].flatMap((size) => [
    { kind: "Array", size, prototype: Array.prototype, sample: () => sized(size) },
    { kind: "TypedArray", size, prototype: Reflect.getPrototypeOf(Int32Array.prototype)!, sample: () => new Int32Array(sized(size)) },
  ]))("charges $kind methods by length ($size) except the constant-time methods", ({ kind, prototype, sample }) => {
    // This exemption list is the test's contract, independent of the wrapper's list.
    const exempt = new Set<PropertyKey>(["at", "push", "pop", "subarray"]);
    const callback = () => 1;
    const argsByMethod = new Map<PropertyKey, unknown[]>([
      ["every", [callback]], ["filter", [callback]], ["find", [callback]],
      ["findIndex", [callback]], ["findLast", [callback]], ["findLastIndex", [callback]],
      ["flatMap", [callback]], ["forEach", [callback]], ["map", [callback]],
      ["reduce", [callback]], ["reduceRight", [callback]], ["some", [callback]],
      ["push", [8]], ["set", [[8]]], ["with", [0, 8]],
    ]);

    for (const name of Reflect.ownKeys(prototype)) {
      const original: unknown = Object.getOwnPropertyDescriptor(prototype, name)?.value;
      if (typeof original !== "function") continue;
      const label = `${kind}.${String(name)}`;
      if (name === "constructor") {
        let activeConstructor: unknown;
        nativeWorkDelta(() => { activeConstructor = Reflect.get(prototype, name); });
        expect(activeConstructor, label).toBe(original);
        continue;
      }
      const receiver = sample();
      const length = receiver.length;
      const args = argsByMethod.get(name) ?? [];
      // Native toString also calls the receiver's wrapped join.
      const expected = exempt.has(name) ? 0 : name === "toString" ? 2 * length : length;
      expect(nativeWorkDelta(() => {
        Reflect.apply(Reflect.get(prototype, name), receiver, args);
      }), label).toBe(expected);
    }
  });

  it.each([3, 5].flatMap((size) => [
    { kind: "Map", size, collection: new Map(sized(size).map((key) => [key, key + 1] as const)) },
    { kind: "Set", size, collection: new Set(sized(size)) },
  ]))("charges $kind iteration by size ($size)", ({ kind, size, collection }) => {
    expect(collection.size).toBe(size);
    for (const method of [Symbol.iterator, "entries", "keys", "values"] as const) {
      expect(nativeWorkDelta(() => { collection[method](); }), `${kind}.${String(method)}`).toBe(size);
    }
    expect(nativeWorkDelta(() => { collection.forEach(() => undefined); }), `${kind}.forEach`).toBe(size);
    expect(nativeWorkDelta(() => {
      for (const entry of collection) void entry;
    }), `${kind} for...of`).toBe(size);
    expect(nativeWorkDelta(() => { void [...collection]; }), `${kind} spread`).toBe(size);
  });
});

describe.skipIf(performanceGate)("counted typed-array aliases", () => {
  it("observes forEach callback arrays and preserves the callback receiver", () => {
    const observed = observeArray(new Float64Array([2, 4, 6]));
    const receiver = { multiplier: 3 };
    let total = 0;
    observed.forEach(function (this: typeof receiver, _value, index, alias) {
      expect(this).toBe(receiver);
      expect(alias).toBe(observed);
      const beforeRead = work;
      total += alias[index] * this.multiplier;
      expect(work).toBe(beforeRead + 1);
    }, receiver);
    expect(total).toBe(36);
  });

  it("observes the array argument of reduce and reduceRight", () => {
    const observed = observeArray(new Float64Array([2, 4, 6]));
    if (!(observed instanceof Float64Array)) throw new Error("Observer changed the array kind");
    for (const reduce of [observed.reduce, observed.reduceRight]) {
      const total = reduce((sum, _value, index, alias) => {
        expect(alias).toBe(observed);
        const beforeRead = work;
        const value = alias[index];
        expect(work).toBe(beforeRead + 1);
        return sum + value;
      }, 0);
      expect(total).toBe(12);
    }
  });

  it("rejects an unobserved typed view over a store buffer", () => {
    const observed = observeArray(new Float64Array([2, 4, 6]));
    expect(() => new Float64Array(observed.buffer)).toThrow("Raw backing buffers are unsupported in counted mode");
  });
});
