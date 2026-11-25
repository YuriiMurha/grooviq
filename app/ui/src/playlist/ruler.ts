import type { ArrangementOperation, LoopRegion } from "../api/arrangement";
import type { GestureGuard } from "./gesture";
import { MAX_SONG_TICKS, snapTick, tickAtX, type PlaylistView } from "./geometry";

export const RULER_DRAG_THRESHOLD = 4;

export function rulerTick(clientX: number, rect: { left: number; width: number }, layoutWidth: number, view: PlaylistView): number {
  if (!(rect.width > 0) || !(layoutWidth > 0)) throw new RangeError("Ruler has no input area");
  return tickAtX((clientX - rect.left) * layoutWidth / rect.width, view);
}

export function keyboardLoop(key: string, region: LoopRegion | null, visibleStart: number, quantum: number): LoopRegion | null {
  if (key === "Home") {
    const startTick = snapTick(visibleStart, quantum, 0, MAX_SONG_TICKS - quantum);
    return { startTick, endTick: startTick + quantum, enabled: region?.enabled ?? true };
  }
  if (!region) return null;
  switch (key) {
    case "ArrowLeft": return { ...region, endTick: Math.max(region.startTick + 1, region.endTick - quantum) };
    case "ArrowRight": return { ...region, endTick: Math.min(MAX_SONG_TICKS, region.endTick + quantum) };
    case "ArrowDown": return { ...region, startTick: Math.max(0, region.startTick - quantum) };
    case "ArrowUp": return { ...region, startTick: Math.min(region.endTick - 1, region.startTick + quantum) };
    default: return null;
  }
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
    this.moved ||= Math.abs(clientX - this.clientX) >= RULER_DRAG_THRESHOLD;
    this.region = this.interval(tick);
    return this.moved ? this.region : null;
  }
  get dragging(): boolean { return this.moved; }
  cancel(): void { this.closed = true; }
  take(guard: GestureGuard): readonly ArrangementOperation[] | null {
    if (this.closed) return null;
    this.closed = true;
    if (!this.moved || guard.pointerId !== this.guard.pointerId || guard.stamp.generation !== this.guard.stamp.generation || guard.stamp.revision !== this.guard.stamp.revision || guard.replacements !== this.guard.replacements) return null;
    return [{ kind: "setLoop", region: this.region }];
  }
}

/** Targets stay transport-only; native owns Song extent and active-loop clamping. */
export function seekTarget(tick: number, quantum: number): number {
  return snapTick(tick, quantum);
}
export function keyboardSeek(key: string, position: number | null, quantum: number): number | null {
  if (key === "Home") return 0;
  if (position === null || !Number.isSafeInteger(position) || position < 0) return null;
  const direction = key === "ArrowLeft" || key === "ArrowDown" ? -1 : key === "ArrowRight" || key === "ArrowUp" ? 1 : 0;
  return direction ? Math.max(0, Math.min(MAX_SONG_TICKS, position + direction * quantum)) : null;
}
