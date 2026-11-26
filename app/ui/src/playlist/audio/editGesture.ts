import type { ArrangementOperation, AudioContent } from "../../api/arrangement";
import type { GestureGuard } from "../gesture";
import { MAX_SONG_TICKS, snapTick } from "../geometry";
import type { RenderClip } from "../viewport";
import { audioDurationTicks, audioSlice, sourceFrameDelta, trimAudio } from "./geometry";

export type AudioEditInput = GestureGuard & Readonly<{
  kind: "resizeStart" | "resizeEnd" | "slice";
  originTick: number;
  quantum: number;
  clip: RenderClip & { id: number };
  content: Readonly<AudioContent>;
  sourceRate: number;
  sourceFrames: number;
  bpm: number;
}>;

/** Captures the native clip and timing once, then consumes at most one edit on release. */
export class AudioEditGesture {
  private readonly input: AudioEditInput;
  private readonly originalFrames: number;
  private readonly originalEnd: number;
  private readonly maxExtension: number;
  private value: RenderClip;
  private operation: ArrangementOperation | null = null;
  private closed = false;

  constructor(input: AudioEditInput) {
    this.input = { ...input, stamp: { ...input.stamp }, clip: { ...input.clip }, content: { ...input.content } };
    this.originalFrames = input.content.sourceEnd - input.content.sourceStart;
    const lengthTicks = this.duration(this.originalFrames);
    this.originalEnd = input.clip.startTick + lengthTicks;
    this.value = { ...input.clip, lengthTicks };
    const maxTicks = input.kind === "resizeStart" ? this.originalEnd : MAX_SONG_TICKS - input.clip.startTick;
    const frames = sourceFrameDelta(maxTicks, input.sourceRate, input.bpm);
    this.maxExtension = frames - Number(this.duration(frames) > maxTicks) - this.originalFrames;
    if (input.kind === "slice") this.update(input.originTick);
  }

  get preview(): RenderClip { return this.value; }
  get sliceTick(): number | null { return this.operation?.kind === "slice" ? this.operation.tick : null; }
  cancel(): void { this.closed = true; }

  private duration(frames: number): number {
    return audioDurationTicks(frames, this.input.sourceRate, this.input.bpm);
  }

  update(tick: number): void {
    if (this.closed) return;
    const { kind, clip, content, quantum, originTick, sourceRate, sourceFrames, bpm } = this.input;
    this.operation = null;
    if (kind === "slice") {
      const snapped = snapTick(tick, quantum);
      if (snapped <= clip.startTick || snapped >= this.originalEnd || this.originalFrames < 2) return;
      const frames = Math.min(this.originalFrames - 1, sourceFrameDelta(snapped - clip.startTick, sourceRate, bpm));
      let partition = audioSlice(content, clip.startTick, frames, sourceRate, bpm);
      // Nearest-frame rounding can overshoot the tick because native rounds the onset up.
      if (partition.tick > snapped && frames > 1) partition = audioSlice(content, clip.startTick, frames - 1, sourceRate, bpm);
      if (partition.tick === snapped && snapped + this.duration(this.originalFrames - partition.audioFirstFrames) <= MAX_SONG_TICKS) {
        this.operation = { kind: "slice", clip: clip.id, tick: snapped, audioFirstFrames: partition.audioFirstFrames };
      }
      return;
    }
    const edge = kind === "resizeStart" ? "start" : "end";
    // A left edge can't snap before tick 0: native start ticks are unsigned.
    const deltaTicks = snapTick(tick - originTick, quantum, edge === "start" ? -clip.startTick : -MAX_SONG_TICKS);
    const frames = sourceFrameDelta(deltaTicks, sourceRate, bpm);
    const deltaFrames = edge === "start" ? Math.max(-this.maxExtension, frames) : Math.min(this.maxExtension, frames);
    const trimmed = trimAudio(content, edge, deltaFrames, sourceFrames);
    const trimmedFrames = trimmed.sourceEnd - trimmed.sourceStart;
    const lengthTicks = this.duration(trimmedFrames);
    let startTick = clip.startTick;
    if (edge === "start") {
      // A rounded duration can shift the end by a tick. Keep the snapped start if it fits.
      const clamped = this.originalFrames - trimmedFrames !== frames;
      startTick = clamped ? this.originalEnd - lengthTicks : Math.min(clip.startTick + deltaTicks, MAX_SONG_TICKS - lengthTicks);
    }
    this.value = { ...clip, startTick, lengthTicks };
    if (trimmed.sourceStart !== content.sourceStart || trimmed.sourceEnd !== content.sourceEnd) {
      this.operation = { kind: "resize", clip: clip.id, startTick, content: trimmed };
    }
  }

  take(guard: GestureGuard): readonly ArrangementOperation[] | null {
    if (this.closed || guard.pointerId !== this.input.pointerId) return null;
    this.closed = true;
    if (guard.replacements !== this.input.replacements || guard.stamp.generation !== this.input.stamp.generation ||
        guard.stamp.revision !== this.input.stamp.revision) return null;
    return this.operation ? [this.operation] : [];
  }
}
