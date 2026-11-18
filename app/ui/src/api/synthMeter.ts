import { invoke } from "@tauri-apps/api/core";

export interface SynthMeter {
  instance: number;
  frames: number;
  /** Left/right peaks, voices, LFO value, amp/filter envelope levels. */
  levels: number[];
  scope: number[];
}
/** A separate bounded meter response; an identity and clock alone are a pending observation. */
export function decodeSynthMeter(raw: unknown, expected: number | null): SynthMeter | null {
  if (!Array.isArray(raw) || (raw.length !== 2 && raw.length !== 264)) return null;
  if (!Number.isSafeInteger(raw[0]) || raw[0] <= 0 || (expected !== null && raw[0] !== expected)) return null;
  if (!Number.isSafeInteger(raw[1]) || raw[1] < 0) return null;
  if (!raw.every((v) => typeof v === "number" && Number.isFinite(v))) return null;
  return { instance: raw[0], frames: raw[1], levels: raw.length === 2 ? Array(6).fill(0) : raw.slice(2, 8), scope: raw.length === 2 ? Array(256).fill(0) : raw.slice(8) };
}
export const readSynthMeter = (channel: number, generation: number, instance: number | null) =>
  invoke<unknown>("synth_meter", { channel, generation, instance });
