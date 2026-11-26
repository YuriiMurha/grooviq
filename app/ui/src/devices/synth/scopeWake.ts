import type { SynthMeter } from "../../api/synthMeter";

type ScopeDisplay = Pick<SynthMeter, "scope" | "levels" | "motion">;

export const SILENT_SCOPE_DISPLAY: ScopeDisplay = {
  scope: Array<number>(256).fill(0),
  levels: Array<number>(6).fill(0),
  motion: null,
};

export const scopeSample = (sample: number) => Math.max(-1, Math.min(1, sample));
const envelopeLevel = (level: number) => Math.max(0, Math.min(1, level));

export function scopeStatus(levels: readonly number[]): string {
  const voices = levels[2];
  const peak = Math.max(levels[0], levels[1]).toFixed(2);
  return `${Math.round(voices)} ${voices === 1 ? "voice" : "voices"} · Peak ${peak}`;
}

/** Frame clocks track freshness. Only values that reach the canvas or visible meters wake it. */
export function scopeDisplayChanged(previous: ScopeDisplay, next: ScopeDisplay): boolean {
  if (previous === next) return false;
  if (scopeStatus(previous.levels) !== scopeStatus(next.levels)) return true;
  if (previous.scope.length !== next.scope.length ||
      next.scope.some((sample, index) => sample !== previous.scope[index] &&
        scopeSample(sample) !== scopeSample(previous.scope[index]))) return true;

  const before = previous.levels[2] > 0 ? previous.motion : null;
  const after = next.levels[2] > 0 ? next.motion : null;
  if (!before || !after) return before !== after;
  return before.lfoPhase % 1 !== after.lfoPhase % 1 ||
    scopeSample(previous.levels[3]) !== scopeSample(next.levels[3]) ||
    before.ampStage !== after.ampStage ||
    before.filterStage !== after.filterStage ||
    (after.ampStage !== "idle" && envelopeLevel(previous.levels[4]) !== envelopeLevel(next.levels[4])) ||
    (after.filterStage !== "idle" && envelopeLevel(previous.levels[5]) !== envelopeLevel(next.levels[5]));
}
