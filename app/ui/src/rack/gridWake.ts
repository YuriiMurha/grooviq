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

const DPR_CHECK_INTERVAL_MS = 500;

/** A resolution query only reports when crossing its current match, so re-arm at the new DPR. */
export function subscribeGridDpr(host: DprHost, onChange: (ratio: number) => void): () => void {
  let ratio: number | undefined;
  let query: EventTarget | undefined;
  const update = () => {
    const next = host.devicePixelRatio || 1;
    if (next === ratio) return;
    ratio = next;
    query?.removeEventListener("change", update);
    query = host.matchMedia?.(`(resolution: ${next}dppx)`);
    query?.addEventListener("change", update);
    onChange(next);
  };
  update();
  host.addEventListener("resize", update);
  // Chromium can change DPR at a fixed viewport size without notifying media queries or observers.
  const timer = setInterval(update, DPR_CHECK_INTERVAL_MS);
  return () => {
    clearInterval(timer);
    host.removeEventListener("resize", update);
    query?.removeEventListener("change", update);
  };
}
