import type { AudioContent } from "../../api/arrangement";
import { integer, MAX_SONG_TICKS } from "../geometry";

function frame(value: number, name: string, min = 0): void {
  integer(value, name, min, Number.MAX_SAFE_INTEGER);
}
function tempoAndRate(sourceRate: number, bpm: number): number {
  integer(sourceRate, "source rate", 8000, 384000);
  // Project.bpm and the native tempo DTO are f32; do not round intermediate maths.
  const tempo = Math.fround(bpm);
  if (!Number.isFinite(tempo) || tempo < 20 || tempo > 400) throw new RangeError("Invalid tempo");
  return tempo;
}
function validate(content: Readonly<AudioContent>): number {
  frame(content.sourceStart, "source start"); frame(content.sourceEnd, "source end", content.sourceStart + 1);
  const length = content.sourceEnd - content.sourceStart;
  integer(content.fadeIn, "fade in", 0, length); integer(content.fadeOut, "fade out", 0, length);
  return length;
}
/** Same operation order as project/src/arrangement.rs validation. */
export function audioDurationTicks(frames: number, sourceRate: number, bpm: number): number {
  frame(frames, "source frame count", 1);
  return Math.ceil(frames / sourceRate * tempoAndRate(sourceRate, bpm) * 16);
}
/** Signed timeline displacement, rounded to the nearest source frame (ties away from zero). */
export function sourceFrameDelta(deltaTicks: number, sourceRate: number, bpm: number): number {
  const tempo = tempoAndRate(sourceRate, bpm);
  if (!Number.isFinite(deltaTicks)) throw new RangeError("Invalid tick displacement");
  const magnitude = Math.round(Math.abs(deltaTicks) / (tempo * 16) * sourceRate);
  frame(magnitude, "source displacement");
  return magnitude === 0 ? 0 : Math.sign(deltaTicks) * magnitude;
}
/** Positive delta moves the chronological edge right; never stretch or cross source edges. */
export function trimAudio(content: Readonly<AudioContent>, edge: "start" | "end", deltaFrames: number, sourceFrames: number): AudioContent {
  validate(content); frame(sourceFrames, "asset frames", content.sourceEnd);
  integer(deltaFrames, "frame displacement", -Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER);
  let { sourceStart, sourceEnd } = content;
  const sourceDelta = content.reverse ? -deltaFrames : deltaFrames;
  const clamp = (value: number, min: number, max: number) => Math.max(min, Math.min(max, value));
  if ((edge === "start") !== content.reverse) sourceStart = clamp(sourceStart + sourceDelta, 0, sourceEnd - 1);
  else sourceEnd = clamp(sourceEnd + sourceDelta, sourceStart + 1, sourceFrames);
  const length = sourceEnd - sourceStart;
  return { ...content, sourceStart, sourceEnd, fadeIn: Math.min(content.fadeIn, length), fadeOut: Math.min(content.fadeOut, length) };
}
/** Explicit source-frame partition and native-compatible second onset for ArrangementOperation.slice. */
export function audioSlice(content: Readonly<AudioContent>, startTick: number, firstFrames: number, sourceRate: number, bpm: number): {
  tick: number; audioFirstFrames: number; left: AudioContent; right: AudioContent;
} {
  const length = validate(content);
  integer(startTick, "clip onset"); integer(firstFrames, "first fragment frames", 1, length - 1);
  const tempo = tempoAndRate(sourceRate, bpm);
  // Preserve arrangement_edit.rs Slice's distinct operation order, including ceil.
  const tick = startTick + Math.ceil(firstFrames / sourceRate * tempo * 960 / 60);
  integer(tick, "slice onset", startTick + 1, MAX_SONG_TICKS);
  const boundary = content.reverse ? content.sourceEnd - firstFrames : content.sourceStart + firstFrames;
  const left = { ...content, fadeIn: Math.min(content.fadeIn, firstFrames), fadeOut: 0 };
  const right = { ...content, fadeIn: 0, fadeOut: Math.min(content.fadeOut, length - firstFrames) };
  if (content.reverse) { left.sourceStart = boundary; right.sourceEnd = boundary; }
  else { left.sourceEnd = boundary; right.sourceStart = boundary; }
  return { tick, audioFirstFrames: firstFrames, left, right };
}
