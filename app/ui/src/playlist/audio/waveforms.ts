import type { ArrangementPage, AudioContent } from "../../api/arrangement";
import type { decodeWaveform } from "../../api/waveform";
import type { PlaylistView } from "../geometry";
import { splitTick, type RenderClip } from "../viewport";
import { createWaveformCache, waveformKey, waveformSourceReplaced, type WaveformCacheKey } from "./waveformCache";
import { waveformRequest } from "./waveformLevel";

type Packet = ReturnType<typeof decodeWaveform>;
type AudioAsset = ArrangementPage["audioAssets"][number];
type Target = Readonly<{ clip: RenderClip; content: AudioContent; key: WaveformCacheKey }>;
type Row = Target & Readonly<{ packet: Packet }>;
type Request = WaveformCacheKey & { request: number };
type Entry = { key: WaveformCacheKey; packet?: Packet; retried?: boolean; failed?: boolean };
const MAX_IN_FLIGHT = 8;

export function visibleWaveforms(page: ArrangementPage, clips: readonly RenderClip[], view: PlaylistView, missing: ReadonlySet<number>): Target[] {
  const assets = new Map(page.audioAssets.map(asset => [asset.id, asset]));
  return clips.flatMap((clip, index) => {
    const content = page.clips[index]?.content;
    if (content?.type !== "audio" || missing.has(content.asset)) return [];
    const top = (clip.trackIndex - view.scrollTrack) * view.rowHeight + 18;
    if (top >= view.height || top + 42 <= 0) return [];
    const asset = assets.get(content.asset);
    if (!asset) return [];
    const request = waveformRequest({
      ...content, sourceFrames: asset.sourceFrames, clipLeftPx: (clip.startTick - view.scrollTick) * view.tickWidth,
      clipWidthPx: Math.max(1, clip.lengthTicks * view.tickWidth), viewportWidthPx: view.width,
    });
    return request ? [{ clip, content, key: { ...request, generation: page.generation, asset: content.asset } }] : [];
  });
}

/** Keeps the visible window resident while the LRU retains recently visited buckets. */
export function createWaveformLoader(fetch: (request: Request) => Promise<Packet>, publish: (rows: readonly Row[]) => void) {
  const cache = createWaveformCache<Packet | null>(-1);
  let generation = -1, nextRequest = 1, inFlight = 0;
  let sourceKeys = new Map<number, string>();
  let targets: readonly Target[] = [];
  let visible = new Map<string, Entry>();
  let displayed: readonly Row[] = [];

  const emit = () => {
    const rows = targets.flatMap(target => {
      const packet = visible.get(waveformKey(target.key))?.packet;
      return packet ? [{ ...target, packet }] : [];
    });
    if (displayed.length === rows.length && displayed.every((row, index) =>
      row.clip === rows[index].clip && row.packet === rows[index].packet)) return;
    displayed = rows;
    publish(rows);
  };

  const request = (key: WaveformCacheKey) => fetch({ ...key, request: nextRequest++ });
  const fetchRange = async (key: WaveformCacheKey): Promise<Packet | null> => {
    try {
      return await request(key);
    } catch (error) {
      const message = error instanceof Error ? error.message : error;
      if (message !== "Invalid waveform range or bin count" || key.end === 1) throw error;
      // Relink preserves the project's saved frame count. Probe the decoded source only
      // if the requested span runs past it, then leave unavailable frames undrawn.
      const first = await request({ ...key, start: 0, end: 1, bins: 1 });
      if (first.frames >= key.end) throw error;
      if (key.start >= first.frames) return null;
      if (first.frames === 1) return first;
      return request({ ...key, end: first.frames, bins: Math.min(key.bins, first.frames - key.start) });
    }
  };

  const pump = () => {
    for (const entry of visible.values()) {
      if (inFlight >= MAX_IN_FLIGHT) break;
      if (entry.packet || entry.failed) continue;
      const pending = cache.begin(entry.key);
      if (!pending) continue;
      inFlight++;
      void fetchRange(entry.key).then(packet => {
        if (pending.resolve(packet)) {
          const current = visible.get(waveformKey(entry.key));
          if (current) { current.packet = packet ?? undefined; current.failed = packet === null; }
        }
      }, error => {
        if (!pending.reject(error)) return;
        if (waveformSourceReplaced(error)) {
          for (const candidate of visible.values()) {
            if (candidate.key.asset !== entry.key.asset) continue;
            candidate.packet = undefined;
            candidate.failed = candidate.retried === true;
            candidate.retried = true;
          }
        } else {
          const current = visible.get(waveformKey(entry.key));
          if (current) current.failed = true;
        }
      }).finally(() => { inFlight--; emit(); pump(); });
    }
  };

  return {
    update(nextGeneration: number, versions: ReadonlyMap<number, number>, assets: readonly AudioAsset[], next: readonly Target[]) {
      if (generation !== nextGeneration) {
        generation = nextGeneration;
        cache.setGeneration(generation);
        visible.clear();
        sourceKeys.clear();
      }
      const keys = new Map(assets.map(asset => [asset.id, JSON.stringify([asset.sample, versions.get(asset.id) ?? 0])]));
      const changed = new Set<number>();
      for (const [id, key] of sourceKeys) {
        if (keys.get(id) !== key) {
          cache.invalidateAsset(id);
          changed.add(id);
        }
      }
      sourceKeys = keys;
      const previous = visible;
      targets = next;
      visible = new Map(next.map(({ key }) => {
        const id = waveformKey(key);
        const packet = cache.get(key);
        return [id, (!changed.has(key.asset) && previous.get(id)) || { key, packet: packet ?? undefined, failed: packet === null }];
      }));
      emit();
      pump();
    },
  };
}

/** Same half-open per-bin partition as the peak pyramid. Each instance draws one peak column. */
export function packWaveforms(rows: readonly Row[]): Float32Array {
  const packed = new Float32Array(rows.reduce((size, { packet }) => size + packet.bins * 8, 0));
  let offset = 0;
  for (const { clip, content, packet } of rows) {
    const { start, end, bins, peaks } = packet;
    const clampPeak = (peak: number) => Math.max(-1, Math.min(1, peak));
    const ticksAt = (frame: number) => {
      const distance = content.reverse ? content.sourceEnd - frame : frame - content.sourceStart;
      return clip.startTick + distance / (content.sourceEnd - content.sourceStart) * clip.lengthTicks;
    };
    for (let bin = 0; bin < bins; bin++) {
      const first = ticksAt(start + Math.floor((end - start) * bin / bins));
      const last = ticksAt(start + Math.floor((end - start) * (bin + 1) / bins));
      const [high, low] = splitTick(Math.min(first, last));
      const base = bin * 4;
      packed.set([high, low, clip.trackIndex, Math.abs(last - first),
        clampPeak(Math.min(peaks[base], peaks[base + 2])), clampPeak(Math.max(peaks[base + 1], peaks[base + 3])),
        clip.ink, Number(clip.muted)], offset);
      offset += 8;
    }
  }
  return packed;
}
