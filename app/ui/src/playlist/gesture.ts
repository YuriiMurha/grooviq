import type { ArrangementOperation, ArrangementStamp } from "../api/arrangement";
import { integer, MAX_SONG_TICKS, resizePattern, snapTick, validatePattern, type PatternPreview } from "./geometry";

export type GestureInput = Readonly<{
  kind: "place" | "move" | "copy" | "resizeStart" | "resizeEnd";
  pointerId: number; stamp: ArrangementStamp; replacements: number;
  originTick: number; originTrack: number; quantum: number;
  tracks: readonly Readonly<{ id: number }>[];
  /** Actual source duration captured under the original arrangement stamp, for resize only. */
  sourceTicks?: number;
  clip: PatternPreview;
}>;
export type GestureGuard = Readonly<{ pointerId: number; stamp: ArrangementStamp; replacements: number }>;
const copy = (clip: PatternPreview): PatternPreview => ({ ...clip, content: { ...clip.content } });

/** A local preview and a single take, not a second command queue or native history. */
export class PatternGesture {
  private readonly input: GestureInput;
  private value: PatternPreview;
  private closed = false;

  constructor(input: GestureInput) {
    validatePattern(input.clip);
    integer(input.pointerId, "pointer", 0, Number.MAX_SAFE_INTEGER);
    integer(input.stamp.generation, "generation", 0, Number.MAX_SAFE_INTEGER);
    integer(input.stamp.revision, "revision", 0, Number.MAX_SAFE_INTEGER);
    integer(input.replacements, "replacement", 0, Number.MAX_SAFE_INTEGER);
    integer(input.originTrack, "origin track", 0, 256);
    if (!Number.isFinite(input.originTick)) throw new RangeError("Invalid gesture origin");
    integer(input.quantum, "snap", 1);
    if (input.kind === "resizeStart" || input.kind === "resizeEnd") {
      if (input.sourceTicks === undefined) throw new RangeError("Pattern source duration is unavailable");
      integer(input.sourceTicks, "pattern source duration", 1);
    }
    if (input.tracks.length > 256 || new Set(input.tracks.map(t => t.id)).size !== input.tracks.length) throw new RangeError("Invalid track identities");
    for (const track of input.tracks) integer(track.id, "track identity", 0, 0xffffffff);
    if (input.kind !== "place" && (input.clip.id === null || input.clip.trackIndex >= input.tracks.length)) throw new RangeError("Gesture requires the original clip and track");
    if (input.kind === "place" && input.clip.id !== null) throw new RangeError("Placement must not invent a native clip identity");
    if (input.kind === "place" && input.clip.trackIndex > Math.min(input.tracks.length, 255)) throw new RangeError("Placement has no target row");
    this.input = { ...input, stamp: { ...input.stamp }, tracks: input.tracks.map(track => ({ ...track })), clip: copy(input.clip) };
    this.value = copy(input.clip);
    if (input.kind === "copy") this.value = { ...this.value, id: null };
  }
  get preview(): PatternPreview { return copy(this.value); }
  cancel(): void { this.closed = true; }

  update(tick: number, trackIndex: number): void {
    if (this.closed) return;
    const { input } = this;
    const delta = snapTick(tick - input.originTick, input.quantum, -MAX_SONG_TICKS);
    integer(trackIndex, "pointer track", 0, Number.MAX_SAFE_INTEGER);
    if (input.kind === "resizeStart" || input.kind === "resizeEnd") {
      this.value = resizePattern(input.clip, input.kind === "resizeStart" ? "start" : "end", delta, input.sourceTicks!);
      return;
    }
    const last = input.kind === "place" ? Math.min(input.tracks.length, 255) : input.tracks.length - 1;
    const row = Math.max(0, Math.min(last, input.clip.trackIndex + trackIndex - input.originTrack));
    this.value = { ...this.value, trackIndex: row, startTick: Math.max(0, Math.min(MAX_SONG_TICKS - input.clip.content.lengthTicks, input.clip.startTick + delta)) };
  }

  take(current: GestureGuard): readonly ArrangementOperation[] | null {
    if (this.closed || current.pointerId !== this.input.pointerId) return null;
    this.closed = true;
    const { input, value } = this;
    if (current.replacements !== input.replacements || current.stamp.generation !== input.stamp.generation || current.stamp.revision !== input.stamp.revision) return null;
    if (input.kind === "place") {
      const track = input.tracks[value.trackIndex];
      const clip = { name: value.name, startTick: value.startTick, muted: value.muted, content: { ...value.content } };
      return track ? [{ kind: "createClip", track: track.id, ...clip }] : [
        { kind: "createTrack", name: `Track ${input.tracks.length + 1}`, color: 0 },
        { kind: "createClipOnCreatedTrack", trackOperation: 0, ...clip },
      ];
    }
    const id = input.clip.id;
    if (id === null) throw new Error("Missing captured native clip");
    if (input.kind === "move" || input.kind === "copy") {
      if (input.kind === "move" && value.trackIndex === input.clip.trackIndex && value.startTick === input.clip.startTick) return [];
      return [{ kind: input.kind, clip: id, track: input.tracks[value.trackIndex].id, startTick: value.startTick }];
    }
    if (value.startTick === input.clip.startTick && value.content.lengthTicks === input.clip.content.lengthTicks) return [];
    return [{ kind: "resize", clip: id, startTick: value.startTick, content: { ...value.content } }];
  }
}
