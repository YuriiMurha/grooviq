/** One pending frame for scene changes, with a continuous read only while the playhead is visible. */
export function createGridWake(
  readPlayhead: () => number | null,
  draw: (playhead: number | null) => boolean,
  requestFrame = requestAnimationFrame,
  cancelFrame = cancelAnimationFrame,
) {
  let pending: number | null = null;
  let shown: number | null = null;
  let dirty = false;
  const schedule = () => {
    if (pending === null) pending = requestFrame(tick);
  };
  const tick = () => {
    pending = null;
    const playhead = readPlayhead();
    if (playhead !== null) schedule();
    if ((dirty || playhead !== shown) && draw(playhead)) {
      dirty = false;
      shown = playhead;
    }
  };
  return {
    markDirty() {
      dirty = true;
      schedule();
    },
    startPlayback: schedule,
    cancel() {
      if (pending !== null) cancelFrame(pending);
      pending = null;
    },
  };
}

type DprHost = Pick<EventTarget, "addEventListener" | "removeEventListener"> & {
  devicePixelRatio: number;
  matchMedia?: (query: string) => EventTarget;
};

/** A resolution query only reports when crossing its current match, so re-arm at the new DPR. */
export function subscribeGridDpr(host: DprHost, onChange: (ratio: number) => void): () => void {
  let ratio = host.devicePixelRatio || 1;
  let query: EventTarget | undefined;
  const arm = () => {
    query?.removeEventListener("change", update);
    query = host.matchMedia?.(`(resolution: ${ratio}dppx)`);
    query?.addEventListener("change", update);
  };
  const update = () => {
    const next = host.devicePixelRatio || 1;
    if (next === ratio) return;
    ratio = next;
    arm();
    onChange(ratio);
  };
  arm();
  host.addEventListener("resize", update);
  return () => {
    host.removeEventListener("resize", update);
    query?.removeEventListener("change", update);
  };
}
