/** Optimistic stepping follows the latest request; only its reply may roll it back. */
export class TempoCommit {
  target: number;
  private accepted: number;
  private serial = 0;
  private pending = 0;
  constructor(bpm: number) { this.target = this.accepted = bpm; }
  observe(bpm: number) {
    this.accepted = bpm;
    if (this.pending === 0) this.target = bpm;
  }
  reset(bpm: number) {
    this.serial++;
    this.target = this.accepted = bpm;
  }
  async send(next: number, commit: (bpm: number) => Promise<boolean>): Promise<void> {
    if (next === this.target) return;
    this.target = next;
    const request = ++this.serial;
    this.pending++;
    try {
      const ok = await commit(next);
      if (request === this.serial) {
        if (ok) this.accepted = next;
        this.target = ok ? next : this.accepted;
      }
    } finally { this.pending--; }
  }
}
