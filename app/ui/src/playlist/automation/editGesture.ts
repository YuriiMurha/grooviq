import type { ArrangementOperation, AutomationContent } from "../../api/arrangement";
import type { GestureGuard } from "../gesture";
import { MAX_SONG_TICKS, snapTick } from "../geometry";
import type { RenderClip } from "../viewport";

export type AutomationEditInput = GestureGuard & Readonly<{
  originTick: number;
  quantum: number;
  clip: RenderClip & { id: number };
}> & (
  | { kind: "slice" }
  | { kind: "resizeStart" | "resizeEnd"; content: Readonly<AutomationContent> }
);

export class AutomationEditGesture {
  private readonly input: AutomationEditInput;
  private value: RenderClip;
  private operation: ArrangementOperation | null = null;
  private closed = false;

  constructor(input: AutomationEditInput) {
    this.input = { ...input, stamp: { ...input.stamp }, clip: { ...input.clip } };
    this.value = this.input.clip;
    if (input.kind === "slice") this.update(input.originTick);
  }

  get preview(): RenderClip { return this.value; }
  get sliceTick(): number | null { return this.operation?.kind === "slice" ? this.operation.tick : null; }
  cancel(): void { this.closed = true; }

  update(tick: number): void {
    if (this.closed) return;
    const input = this.input;
    const { clip, quantum, originTick } = input;
    this.operation = null;
    if (input.kind === "slice") {
      const snapped = snapTick(tick, quantum);
      if (snapped > clip.startTick && snapped < clip.startTick + clip.lengthTicks) {
        this.operation = { kind: "slice", clip: clip.id, tick: snapped, audioFirstFrames: null };
      }
      return;
    }

    const { content } = input;
    const originalLength = content.lengthTicks;
    const originalOffset = content.offsetTick;
    let startTick = clip.startTick, offsetTick = originalOffset, lengthTicks = originalLength;
    if (input.kind === "resizeStart") {
      // Both windows keep their end, with the same signed shift applied to their starts.
      const delta = snapTick(tick - originTick, quantum, -Math.min(startTick, offsetTick), originalLength - 1);
      startTick += delta;
      offsetTick += delta;
      lengthTicks -= delta;
    } else {
      const limit = MAX_SONG_TICKS - Math.max(startTick, offsetTick);
      const delta = snapTick(tick - originTick, quantum, 1 - originalLength, limit - originalLength);
      lengthTicks += delta;
    }
    this.value = { ...clip, startTick, lengthTicks };
    if (startTick !== clip.startTick || lengthTicks !== originalLength) {
      this.operation = { kind: "resize", clip: clip.id, startTick, content: { ...content, offsetTick, lengthTicks } };
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
