import { invoke } from "@tauri-apps/api/core";

export type EnvelopeStage = "idle" | "attack" | "decay" | "sustain" | "release";
const stages: readonly EnvelopeStage[] = ["idle", "attack", "decay", "sustain", "release"];
export interface SynthMeter {
  instance: number;
  frames: number;
  /** Left/right peaks, voices, LFO value, amp/filter envelope levels. */
  levels: number[];
  scope: number[];
  motion: { ampStage: EnvelopeStage; filterStage: EnvelopeStage; lfoPhase: number } | null;
}
/** A separate bounded meter response; an identity and clock alone are a pending observation. */
export function decodeSynthMeter(raw: unknown, expected: number | null): SynthMeter | null {
  if (!Array.isArray(raw) || (raw.length !== 2 && raw.length !== 264 && raw.length !== 267)) return null;
  if (!Number.isSafeInteger(raw[0]) || raw[0] <= 0 || (expected !== null && raw[0] !== expected)) return null;
  if (!Number.isSafeInteger(raw[1]) || raw[1] < 0) return null;
  if (!raw.every((v) => typeof v === "number" && Number.isFinite(v))) return null;
  let motion: SynthMeter["motion"] = null;
  if (raw.length === 267) {
    const [amp, filter, phase] = raw.slice(264);
    if (!Number.isInteger(amp) || amp < 0 || amp > 4 || !Number.isInteger(filter) || filter < 0 || filter > 4 || phase < 0 || phase > 1) return null;
    motion = { ampStage: stages[amp], filterStage: stages[filter], lfoPhase: phase };
  }
  return { motion, instance: raw[0], frames: raw[1], levels: raw.length === 2 ? Array(6).fill(0) : raw.slice(2, 8), scope: raw.length === 2 ? Array(256).fill(0) : raw.slice(8, 264) };
}
// Unmounting an editor cannot cancel native IPC. Keep ownership for this webview until
// settlement, so selection changes and reopening never queue another observation behind it.
let pending: Promise<unknown> | null = null;
export function readSynthMeter(channel: number, generation: number, instance: number | null): Promise<unknown> | null {
  if (pending !== null) return null;
  pending = invoke<unknown>("synth_meter", { channel, generation, instance }).finally(() => { pending = null; });
  return pending;
}
