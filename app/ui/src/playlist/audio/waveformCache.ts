import type { WaveformRequest } from "./waveformLevel";

export type WaveformCacheKey = WaveformRequest & Readonly<{
  generation: number;
  asset: number;
  /** Reuse the native request ID for repeat lookups of the same source and bucket. */
  request: number;
}>;

export type WaveformPending<T> = Readonly<{
  resolve(value: T): boolean;
  reject(error: unknown): boolean;
}>;

export type WaveformCache<T> = Readonly<{
  setGeneration(generation: number): void;
  get(key: WaveformCacheKey): T | undefined;
  /** Reserve a request slot. Settle even if the requesting view has gone away. */
  begin(key: WaveformCacheKey): WaveformPending<T> | null;
}>;

const MAX_ENTRIES = 64;
const MAX_PENDING = 64;
const SOURCE_REPLACED = "Waveform source replaced; reload";

function cacheId({ generation, asset, request, start, end, bins }: WaveformCacheKey): string {
  return `${generation}:${asset}:${request}:${start}:${end}:${bins}`;
}

export function createWaveformCache<T>(initialGeneration: number): WaveformCache<T> {
  let generation = initialGeneration;
  const entries = new Map<string, { asset: number; value: T }>();
  const pending = new Map<string, { asset: number; stale: boolean }>();

  return {
    setGeneration(next) {
      if (next === generation) return;
      generation = next;
      entries.clear();
      for (const flight of pending.values()) flight.stale = true;
    },
    get(key) {
      if (key.generation !== generation) return undefined;
      const id = cacheId(key);
      const entry = entries.get(id);
      if (!entry) return undefined;
      entries.delete(id);
      entries.set(id, entry);
      return entry.value;
    },
    begin(key) {
      if (key.generation !== generation || pending.size >= MAX_PENDING) return null;
      const id = cacheId(key);
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
          const message = error instanceof Error ? error.message : error;
          if (message === SOURCE_REPLACED) {
            for (const [entryId, entry] of entries) {
              if (entry.asset === asset) entries.delete(entryId);
            }
            // Retain reservations until they settle so an invalidated request cannot overlap its retry.
            for (const other of pending.values()) {
              if (other.asset === asset) other.stale = true;
            }
          }
          return true;
        },
      };
    },
  };
}
