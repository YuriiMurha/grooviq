import type { WaveformRequest } from "./waveformLevel";

export type WaveformCacheKey = WaveformRequest & Readonly<{
  generation: number;
  asset: number;
}>;

export type WaveformPending<T> = Readonly<{
  resolve(value: T): boolean;
  reject(error: unknown): boolean;
}>;

export type WaveformCache<T> = Readonly<{
  setGeneration(generation: number): void;
  invalidateAsset(asset: number): void;
  get(key: WaveformCacheKey): T | undefined;
  /** Reserve a request slot. Settle even if the requesting view has gone away. */
  begin(key: WaveformCacheKey): WaveformPending<T> | null;
}>;

const MAX_ENTRIES = 64;
const MAX_PENDING = 64;
export function waveformSourceReplaced(error: unknown): boolean {
  return (error instanceof Error ? error.message : error) === "Waveform source replaced; reload";
}

export function waveformKey({ generation, asset, start, end, bins }: WaveformCacheKey): string {
  return `${generation}:${asset}:${start}:${end}:${bins}`;
}

export function createWaveformCache<T>(initialGeneration: number): WaveformCache<T> {
  let generation = initialGeneration;
  const entries = new Map<string, { asset: number; value: T }>();
  const pending = new Map<string, { asset: number; stale: boolean }>();

  const invalidateAsset = (asset: number) => {
    for (const [id, entry] of entries) if (entry.asset === asset) entries.delete(id);
    // Keep reservations until the old requests settle.
    for (const flight of pending.values()) if (flight.asset === asset) flight.stale = true;
  };

  return {
    invalidateAsset,
    setGeneration(next) {
      if (next === generation) return;
      generation = next;
      entries.clear();
      for (const flight of pending.values()) flight.stale = true;
    },
    get(key) {
      if (key.generation !== generation) return undefined;
      const id = waveformKey(key);
      const entry = entries.get(id);
      if (!entry) return undefined;
      entries.delete(id);
      entries.set(id, entry);
      return entry.value;
    },
    begin(key) {
      if (key.generation !== generation || pending.size >= MAX_PENDING) return null;
      const id = waveformKey(key);
      if (entries.has(id) || pending.has(id)) return null;

      const { asset } = key;
      const flight = { asset, stale: false };
      pending.set(id, flight);
      function settle(): boolean {
        if (pending.get(id) !== flight) return false;
        pending.delete(id);
        return !flight.stale;
      }

      return {
        resolve(value) {
          if (!settle()) return false;
          entries.set(id, { asset, value });
          if (entries.size > MAX_ENTRIES) {
            const oldest = entries.keys().next();
            if (!oldest.done) entries.delete(oldest.value);
          }
          return true;
        },
        reject(error) {
          if (!settle()) return false;
          if (waveformSourceReplaced(error)) invalidateAsset(asset);
          return true;
        },
      };
    },
  };
}
