import type { EnvelopeStage } from "../../api/synthMeter";

export interface MeterBox { left: number; right: number; top: number; bottom: number }
export interface EnvelopeMeterBox extends MeterBox { attack: number; decay: number; sustain: number }
const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));

/** Stage centres convey identity, never elapsed time along the illustrative curve. */
export function envelopeMeter(stage: EnvelopeStage, level: number, box: EnvelopeMeterBox, active: boolean) {
  if (!active || stage === "idle" || !Number.isFinite(level)) return null;
  const boundaries = [box.left, box.attack, box.decay, box.sustain, box.right];
  const index = { attack: 0, decay: 1, sustain: 2, release: 3 }[stage];
  const left = boundaries[index];
  const right = boundaries[index + 1];
  return { left, width: right - left, x: (left + right) / 2, y: box.bottom - clamp(level, 0, 1) * (box.bottom - box.top) };
}

/** One of the display's two cycles, with measured smoothed output independent of its curve. */
export function lfoMeter(phase: number, output: number, amount: number, box: MeterBox, active: boolean) {
  if (!active || !Number.isFinite(phase) || !Number.isFinite(output)) return null;
  const wrapped = ((phase % 1) + 1) % 1;
  const mid = (box.top + box.bottom) / 2;
  return { x: box.left + wrapped * (box.right - box.left) / 2, y: mid - clamp(output, -1, 1) * (box.bottom - box.top) / 2 * clamp(amount, 0.04, 1) };
}
