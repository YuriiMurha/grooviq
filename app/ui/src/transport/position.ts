/** 960 ticks a quarter note; a step is a 16th, 240 ticks. */
const TICKS_PER_WHOLE = 3840;
const TICKS_PER_STEP = 240;

export interface Signature {
  beats: number;
  unit: number;
}

/** "bar.beat.step", all 1-based, the step as two digits ("9.3.02"). A beat is `1/unit` of a whole
 * note; the step counts 16ths inside it (and stays 01 when a beat is shorter than a step). */
export function formatPosition(ticks: bigint, sig: Signature): string {
  const unit = sig.unit > 0 ? sig.unit : 4;
  const beats = sig.beats > 0 ? sig.beats : 4;
  const beatTicks = BigInt(Math.max(1, Math.floor(TICKS_PER_WHOLE / unit)));
  const barTicks = beatTicks * BigInt(beats);
  const at = ticks < 0n ? 0n : ticks;
  const bar = at / barTicks;
  const inBar = at % barTicks;
  const beat = inBar / beatTicks;
  const step = (inBar % beatTicks) / BigInt(TICKS_PER_STEP);
  return `${bar + 1n}.${beat + 1n}.${String(step + 1n).padStart(2, "0")}`;
}

/** The tick the person hears now: the engine's position shifted earlier by the output latency
 * (the sound it just rendered is still on its way), never below 0. */
export function displayTicks(
  ticks: bigint,
  latencyFrames: number,
  bpm: number,
  sampleRate: number,
): bigint {
  if (!(sampleRate > 0) || !(latencyFrames > 0)) return ticks;
  // ticks per second = bpm / 60 * 960 = 16 * bpm
  const latencyTicks = BigInt(Math.round((latencyFrames / sampleRate) * 16 * bpm));
  return ticks > latencyTicks ? ticks - latencyTicks : 0n;
}
