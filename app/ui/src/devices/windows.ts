import { useSyncExternalStore } from "react";
import { onProjectReplaced } from "../project/replacement";

// The open device windows, one per channel, in the order they opened. Anything can open one (the
// rack's instrument button); `DeviceWindows` draws them. Raising a window changes only its
// stacking (`z`), never the order, so its focused control is never moved in the document.

export interface OpenDevice {
  channel: number;
  /** Where focus goes back to when the window closes. */
  opener: HTMLElement | null;
  /** Stacking: the highest is in front. */
  z: number;
}

let open: readonly OpenDevice[] = [];
let top = 0;
const listeners = new Set<() => void>();

function set(next: readonly OpenDevice[]) {
  open = next;
  for (const l of listeners) l();
}

/** Opens the channel's editor, or brings its open one to the front. */
export function openDevice(channel: number, opener: HTMLElement | null) {
  if (open.some((w) => w.channel === channel)) raiseDevice(channel);
  else set([...open, { channel, opener, z: ++top }]);
}

export function closeDevice(channel: number) {
  if (open.some((w) => w.channel === channel)) set(open.filter((w) => w.channel !== channel));
}

/** Brings an open window to the front. */
export function raiseDevice(channel: number) {
  const w = open.find((x) => x.channel === channel);
  if (w && w.z !== top) set(open.map((x) => (x === w ? { ...x, z: ++top } : x)));
}

// Channel ids start again in a new project: no window carries over.
onProjectReplaced(() => {
  if (open.length) set([]);
});

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useOpenDevices(): readonly OpenDevice[] {
  return useSyncExternalStore(subscribe, () => open);
}
