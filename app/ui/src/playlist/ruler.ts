import type { ArrangementOperation, LoopRegion } from "../api/arrangement";
import type { GestureGuard } from "./gesture";
import { MAX_SONG_TICKS, snapTick, tickAtX, type PlaylistView } from "./geometry";

export function rulerTick(clientX: number, rect: { left: number; width: number }, view: PlaylistView): number {
  if (!(rect.width > 0)) throw new RangeError("Ruler has no input area");
  return tickAtX((clientX - rect.left) * view.width / rect.width, view);
}
export class LoopGesture {
  private closed = false;
  private moved = false;
  private region: LoopRegion;
  constructor(private readonly guard: GestureGuard, private readonly origin: number, private readonly quantum: number, private readonly clientX: number, private readonly enabled = true) {
    this.region = this.interval(origin);
  }
  private interval(tick: number): LoopRegion {
    const start = snapTick(this.origin, this.quantum);
    const end = snapTick(tick, this.quantum);
    const low = Math.min(start, end, MAX_SONG_TICKS - this.quantum);
    return { startTick: low, endTick: Math.max(low + this.quantum, start, end), enabled: this.enabled };
  }
  update(tick: number, clientX: number): LoopRegion | null {
    if (this.closed) return null;
    this.moved ||= Math.abs(clientX - this.clientX) >= 3;
    this.region = this.interval(tick);
    return this.moved ? this.region : null;
  }
  cancel(): void { this.closed = true; }
  take(guard: GestureGuard): readonly ArrangementOperation[] | null {
    if (this.closed) return null;
    this.closed = true;
    if (!this.moved || guard.pointerId !== this.guard.pointerId || guard.stamp.generation !== this.guard.stamp.generation || guard.stamp.revision !== this.guard.stamp.revision || guard.replacements !== this.guard.replacements) return null;
    return [{ kind: "setLoop", region: this.region }];
  }
}
