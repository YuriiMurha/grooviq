import type { ArrangementStamp } from "../api/arrangement";
import { isMixerMeterTrack, MIXER_METER_MAX_TRACKS } from "../api/mixerMeter";
import type { LiveFrame } from "../live/decode";
import { mixerMeters } from "./mixerMeters";
import type { MeterSource, Window } from "./mixerModel";

export const MIXER_METER_POLL_MS = 50;
const MASTER_MAX_AGE_MS = 250;

type MeterScope = Readonly<{
  stamp: ArrangementStamp;
  masterId: string;
  resolveTrack: (nodeId: string) => number | undefined;
}>;

/** Pinned nodes and the actual viewport take priority over mounted overscan. */
export function meterWindowNodes(
  tracks: readonly { id: string }[], visible: Window, pinned: readonly number[], drawn: readonly number[],
): string[] {
  const ids = new Set<string>();
  const add = (index: number) => {
    const track = tracks[index];
    if (track && ids.size < MIXER_METER_MAX_TRACKS) ids.add(track.id);
  };
  for (const index of pinned) add(index);
  for (let index = visible.start; index < Math.min(visible.end, tracks.length) && ids.size < MIXER_METER_MAX_TRACKS; index++) add(index);
  for (const index of drawn) {
    if (ids.size === MIXER_METER_MAX_TRACKS) break;
    add(index);
  }
  return [...ids];
}

/** Supplies one mounted panel, borrowing the singleton's serialized native lease. */
export function createMixerMeterDriver(cache = mixerMeters, clock = () => performance.now()) {
  let latest: (() => LiveFrame | null) | null = null;
  let scope: MeterScope | null = null;
  let nodes: readonly string[] = [];
  let windowIds = new Map<string, number>();
  let unsubscribeCache: (() => void) | null = null;

  let master: LiveFrame | null = null;
  let lastMasterFrame = 0n;
  let lastMasterArrival = -Infinity;
  let masterAbove = false;
  let masterClip = false;
  const listeners = new Set<() => void>();

  function notifyClips() {
    for (const listener of listeners) listener();
  }

  function setMasterClip(clipped: boolean) {
    if (masterClip === clipped) return;
    masterClip = clipped;
    notifyClips();
  }

  function setScope(next: MeterScope | null) {
    const generationChanged = next?.stamp.generation !== scope?.stamp.generation;
    scope = next;
    if (!next || generationChanged) {
      windowIds.clear();
      cache.close();
      // Live frames carry no project generation. Leave the retained frame behind.
      const retained = next && latest ? latest() : null;
      lastMasterFrame = retained?.frames ?? 0n;
      lastMasterArrival = next ? Math.max(clock(), retained?.receivedAt ?? -Infinity) : -Infinity;
      master = null;
      masterAbove = false;
      setMasterClip(false);
    }
    syncWindow();
  }

  function syncWindow() {
    if (!latest || !scope) return;
    const selected = new Map<string, number>();
    for (const nodeId of nodes) {
      const id = scope.resolveTrack(nodeId);
      if (isMixerMeterTrack(id)) selected.set(nodeId, id);
      if (selected.size === MIXER_METER_MAX_TRACKS) break;
    }
    windowIds = selected;
    cache.setVisible(scope.stamp, [...selected.values()]);
  }

  function freshMaster(frame: LiveFrame | null, time: number): frame is LiveFrame & { receivedAt: number } {
    const arrival = frame?.receivedAt;
    return arrival !== undefined && time >= arrival && time - arrival < MASTER_MAX_AGE_MS;
  }

  function observeMaster(latch: boolean) {
    if (!latest || !scope) return;
    const frame = latest();
    if (freshMaster(frame, clock()) && frame.frames > lastMasterFrame && frame.receivedAt > lastMasterArrival
      && frame.peak.every(peak => Number.isFinite(peak) && peak >= 0)) {
      lastMasterFrame = frame.frames;
      lastMasterArrival = frame.receivedAt;
      master = frame;
      const above = frame.peak[0] >= 1 || frame.peak[1] >= 1;
      if (latch && above && !masterAbove) setMasterClip(true);
      masterAbove = above;
    }
  }

  function tick(): Promise<void> | null {
    if (!latest || !scope) return null;
    observeMaster(true);
    return cache.poll();
  }

  const source = {
    setVisible(ids) {
      nodes = ids;
      syncWindow();
    },
    read(nodeId, out, time = clock()) {
      const id = windowIds.get(nodeId);
      const peak = nodeId === scope?.masterId
        ? (freshMaster(master, time) ? master.peak : null)
        : (id === undefined ? null : cache.read(id, time));
      if (!peak) return false;
      out[0] = peak[0];
      out[1] = peak[1];
      return true;
    },
    isClipped(nodeId) {
      if (nodeId === scope?.masterId) return masterClip;
      const id = windowIds.get(nodeId);
      return id !== undefined && cache.isClipped(id);
    },
    subscribeClips(listener) {
      listeners.add(listener);
      return () => { listeners.delete(listener); };
    },
  } satisfies MeterSource;

  return {
    source, setScope, tick,
    start(readLatest: () => LiveFrame | null) {
      latest = readLatest;
      unsubscribeCache ??= cache.subscribeClips(notifyClips);
      syncWindow();
    },
    stop() {
      setScope(null);
      latest = null;
      unsubscribeCache?.();
      unsubscribeCache = null;
    },
    clearClip(nodeId: string) {
      if (nodeId === scope?.masterId) {
        // Consume the currently retained observation, even between ticks, without latching it.
        observeMaster(false);
        setMasterClip(false);
      } else {
        const id = windowIds.get(nodeId);
        if (id !== undefined) cache.clearClip(id);
      }
    },
  };
}
