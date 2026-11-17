import { useSyncExternalStore } from "react";

// How many times each channel's sample was replaced in this session (a relink). The view names a
// sample only by its file name, which a different file can share, so whatever is drawn from a
// sample's audio (the sampler editor's waveform) is keyed by this too.

const revisions = new Map<number, number>();
const listeners = new Set<() => void>();

/** The channel now plays another file. */
export function sampleReplaced(channel: number) {
  revisions.set(channel, (revisions.get(channel) ?? 0) + 1);
  for (const l of listeners) l();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useSampleRevision(channel: number): number {
  return useSyncExternalStore(subscribe, () => revisions.get(channel) ?? 0);
}
