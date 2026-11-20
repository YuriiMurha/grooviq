export interface MeterContext { id?: string; rate?: number }
interface Observation {
  frames: bigint;
  peak: readonly [number, number];
  outputLatencyFrames?: number;
  receivedAt?: number;
}
interface Entry { peak: readonly [number, number]; deadline: number }
const SILENCE = [0, 0] as const;
const clean = (n: number) => Number.isFinite(n) && n > 0 ? n : 0;

/** Arrival-based approximation only: the stream has no coherent presentation timestamp. */
export class MeterHistory {
  private pending: Entry[] = [];
  private selected: readonly [number, number] = SILENCE;
  private context: MeterContext | null = null;
  private counter: bigint | null = null;
  private receipt = 0;
  private expires = -Infinity;
  private clock = -Infinity;

  get pendingCount() { return this.pending.length; }

  reset() {
    this.pending = [];
    this.selected = SILENCE;
    this.context = null;
    this.counter = null;
    this.expires = -Infinity;
    this.clock = -Infinity;
  }

  private advance(now: number): boolean {
    if (!Number.isFinite(now)) { this.reset(); return false; }
    if (now < this.clock) this.reset();
    this.clock = now;
    if (now >= this.expires) {
      this.pending = [];
      this.selected = SILENCE;
      // Retain the counter so stalled duplicates cannot revive expired peaks.
    }
    return true;
  }

  push(o: Observation, context: MeterContext, now: number) {
    if (!this.advance(now)) return;
    const receipt = o.receivedAt ?? now;
    if (!Number.isFinite(receipt)) { this.reset(); return; }
    const changed = this.context !== null &&
      (this.context.id !== context.id || !Object.is(this.context.rate, context.rate));
    if (changed || (this.counter !== null && (o.frames < this.counter || receipt < this.receipt))) {
      this.reset();
      this.clock = now;
    }
    this.context = { id: context.id, rate: context.rate };
    if (o.frames === this.counter) return;
    if (this.counter !== null && receipt > this.expires) {
      this.reset();
      this.clock = now;
      this.context = { id: context.id, rate: context.rate };
    }
    const rate = context.rate;
    const latency = o.outputLatencyFrames;
    const delay = rate !== undefined && Number.isFinite(rate) && rate > 0 &&
      latency !== undefined && Number.isFinite(latency) && latency >= 0
      ? Math.min(2000, 1000 * latency / rate) : 0;
    const deadline = Math.max(this.pending.at(-1)?.deadline ?? -Infinity, receipt + delay);
    if (this.pending.length === 512) this.pending.shift();
    this.pending.push({ peak: [clean(o.peak[0]), clean(o.peak[1])], deadline });
    this.counter = o.frames;
    this.receipt = receipt;
    this.expires = receipt + delay + 1000;
  }

  read(now: number): readonly [number, number] {
    if (!this.advance(now)) return SILENCE;
    while (this.pending.length && this.pending[0].deadline <= now) {
      this.selected = this.pending.shift()!.peak;
    }
    return this.selected;
  }
}
