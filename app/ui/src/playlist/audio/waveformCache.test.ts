import { expect, it } from "vitest";
import { createWaveformCache, type WaveformCacheKey } from "./waveformCache";

const key: WaveformCacheKey = { generation: 4, asset: 7, request: 99, start: 0, end: 1024, bins: 32 };
const replaced = "Waveform source replaced; reload";

it("looks up a completed waveform by all parts of its request key", () => {
  const cache = createWaveformCache<string>(4);
  expect(cache.get(key)).toBeUndefined();
  expect(cache.begin(key)?.resolve("peaks")).toBe(true);
  expect(cache.get({ ...key })).toBe("peaks");
  for (const other of [
    { ...key, generation: 5 }, { ...key, asset: 6 }, { ...key, request: 100 },
    { ...key, start: 8 }, { ...key, end: 2048 }, { ...key, bins: 64 },
  ]) expect(cache.get(other)).toBeUndefined();
});

it("invalidates completed and pending work on generation changes", () => {
  const cache = createWaveformCache<string>(4);
  cache.begin(key)?.resolve("old");
  const pendingKey = { ...key, bins: 64 };
  const pending = cache.begin(pendingKey);
  expect(pending).not.toBeNull();
  cache.setGeneration(4);
  expect(cache.get(key)).toBe("old");
  cache.setGeneration(5);
  const newKey = { ...key, generation: 5 };
  expect(cache.begin(newKey)?.resolve("new")).toBe(true);
  expect(cache.get(key)).toBeUndefined();
  expect(cache.begin(key)).toBeNull();
  expect(pending?.resolve("late")).toBe(false);
  cache.setGeneration(4);
  expect(cache.get(key)).toBeUndefined();
  expect(cache.get(pendingKey)).toBeUndefined();
  expect(cache.get(newKey)).toBeUndefined();
});

it.each([replaced, new Error(replaced)])("invalidates only the replaced asset", (error) => {
  const cache = createWaveformCache<string>(4);
  const otherAsset = { ...key, asset: 8 };
  const pendingKey = { ...key, bins: 64 };
  cache.begin(key)?.resolve("old");
  cache.begin(otherAsset)?.resolve("unrelated");
  const pending = cache.begin(pendingKey);
  expect(cache.begin({ ...key, request: 100 })?.reject(error)).toBe(true);
  expect(cache.get(key)).toBeUndefined();
  expect(cache.get(otherAsset)).toBe("unrelated");
  expect(cache.begin(pendingKey)).toBeNull();
  expect(pending?.resolve("stale")).toBe(false);
  expect(cache.begin(pendingKey)?.resolve("fresh")).toBe(true);
  expect(cache.get(pendingKey)).toBe("fresh");
});

it("ignores late replacement errors from a previous generation", () => {
  const cache = createWaveformCache<string>(4);
  const pending = cache.begin(key);
  cache.setGeneration(5);
  const newKey = { ...key, generation: 5 };
  cache.begin(newKey)?.resolve("current");
  expect(pending?.reject(replaced)).toBe(false);
  expect(cache.get(newKey)).toBe("current");
});

it("evicts the least recently used reply once the cache reaches its cap", () => {
  const cache = createWaveformCache<number>(4);
  for (let request = 0; request < 64; request++) {
    expect(cache.begin({ ...key, request })?.resolve(request)).toBe(true);
  }
  expect(cache.get({ ...key, request: 0 })).toBe(0);
  cache.begin({ ...key, request: 64 })?.resolve(64);
  expect(cache.get({ ...key, request: 0 })).toBe(0);
  expect(cache.get({ ...key, request: 1 })).toBeUndefined();
  expect(cache.get({ ...key, request: 64 })).toBe(64);
});

it("deduplicates in-flight work, then allows a retry after an ordinary failure", () => {
  const cache = createWaveformCache<string>(4);
  const pending = cache.begin(key);
  expect(pending).not.toBeNull();
  expect(cache.begin({ ...key })).toBeNull();
  expect(cache.get(key)).toBeUndefined();
  expect(pending?.reject("Unavailable")).toBe(true);
  const retry = cache.begin(key);
  expect(retry).not.toBeNull();
  expect(pending?.resolve("old")).toBe(false);
  expect(retry?.resolve("fresh")).toBe(true);
  expect(cache.begin(key)).toBeNull();
  expect(cache.get(key)).toBe("fresh");
});

it("bounds outstanding work and admits a new request after one settles", () => {
  const cache = createWaveformCache<number>(4);
  const first = cache.begin(key);
  expect(first).not.toBeNull();
  for (let request = 0; request < 63; request++) {
    expect(cache.begin({ ...key, request })).not.toBeNull();
  }
  const next = { ...key, request: 64 };
  expect(cache.begin(next)).toBeNull();
  expect(first?.resolve(99)).toBe(true);
  expect(cache.begin(next)).not.toBeNull();
});
