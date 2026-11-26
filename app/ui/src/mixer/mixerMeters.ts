import {
  MIXER_METER_MAX_TRACKS, isMixerMeterTrack, requestMixerMeter, sameMixerMeterStamp,
} from "../api/mixerMeter";
import type { MixerMeterCapture, MixerMeterRequest, MixerMeterRow, MixerMeterStamp } from "../api/mixerMeter";

const CAPTURE_MAX_AGE_MS = 250;

type MeterTransport = (request: MixerMeterRequest) => Promise<MixerMeterCapture | null>;

export function createMixerMeters(
  request: MeterTransport = requestMixerMeter,
  now = () => performance.now(),
) {
  let active: MixerMeterRequest | null = null;
  let cancellation: MixerMeterRequest | null = null;
  let inFlight = false;
  let generation: number | null = null;
  let lastFrame = 0n;
  let clipFloor = 0n;
  let advancedAt = -Infinity;
  let rows = new Map<number, MixerMeterRow>();
  const latestClip = new Map<number, bigint>();
  const clipped = new Set<number>();
  const clipListeners = new Set<() => void>();

  function notifyClips() {
    for (const listener of clipListeners) listener();
  }

  function resetRows() {
    rows.clear();
    advancedAt = -Infinity;
    // A lease can survive a selection change that never made an IPC call. Clip times
    // from the previously displayed block or earlier still belong to that old selection.
    clipFloor = lastFrame;
    latestClip.clear();
    if (clipped.size > 0) {
      clipped.clear();
      notifyClips();
    }
  }

  function accept(input: MixerMeterRequest, frame: MixerMeterCapture | null) {
    if (active !== input || frame === null || !sameMixerMeterStamp(frame, input) || frame.frames <= lastFrame) return;
    lastFrame = frame.frames;
    advancedAt = now();
    rows = new Map(frame.tracks.map(row => [row.track, row]));
    let changed = false;
    for (const row of rows.values()) {
      const previous = latestClip.get(row.track) ?? clipFloor;
      if (row.clipFrame === null || row.clipFrame <= previous) continue;
      latestClip.set(row.track, row.clipFrame);
      if (!clipped.has(row.track)) {
        clipped.add(row.track);
        changed = true;
      }
    }
    if (changed) notifyClips();
  }

  async function send(input: MixerMeterRequest): Promise<void> {
    inFlight = true;
    try {
      const frame = await request(input).catch(() => null);
      accept(input, frame);
    } finally {
      inFlight = false;
      sendCancellation();
    }
  }

  function sendCancellation() {
    if (inFlight || cancellation === null) return;
    const input = cancellation;
    cancellation = null;
    void send(input);
  }

  function close() {
    if (active === null) return;
    const { generation, revision } = active;
    active = null;
    cancellation = { generation, revision, tracks: [] };
    resetRows();
    sendCancellation();
  }

  function setVisible(stamp: MixerMeterStamp, tracks: readonly number[]) {
    const visible = new Set<number>();
    for (const track of tracks) {
      if (isMixerMeterTrack(track)) visible.add(track);
      if (visible.size === MIXER_METER_MAX_TRACKS) break;
    }
    if (visible.size === 0) {
      close();
      return;
    }
    const ids = [...visible].sort((left, right) => left - right);
    if (active !== null && sameMixerMeterStamp(active, stamp)
      && active.tracks.length === ids.length && active.tracks.every((track, index) => track === ids[index])) return;
    if (generation !== stamp.generation) lastFrame = 0n;
    generation = stamp.generation;
    cancellation = null;
    active = { generation: stamp.generation, revision: stamp.revision, tracks: ids };
    resetRows();
  }

  return {
    setVisible,
    close,
    /** Driven by the mounted panel's timer. Skips while any request or cancellation is in flight. */
    poll(): Promise<void> | null {
      return inFlight || active === null ? null : send(active);
    },
    /** Draw-time cache lookup. Uses the same clock as animation frames. */
    read(track: number, time = now()): MixerMeterRow["peak"] | null {
      return time - advancedAt < CAPTURE_MAX_AGE_MS ? rows.get(track)?.peak ?? null : null;
    },
    isClipped(track: number): boolean {
      return clipped.has(track);
    },
    clearClip(track: number) {
      if (clipped.delete(track)) notifyClips();
    },
    subscribeClips(listener: () => void): () => void {
      clipListeners.add(listener);
      return () => { clipListeners.delete(listener); };
    },
  };
}

// Reuse across mounts so panel replacement cannot overlap a request from the previous panel.
export const mixerMeters = createMixerMeters();
