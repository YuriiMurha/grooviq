import { eventWarns, type AudioEvent, type LogEntry } from "../audio";

type Stop = () => void;

/** One launch's delivered IDs. Snapshot and live delivery can overlap and arrive out of order.
 * Keep only the native ring's worth of IDs; older entries cannot re-enter a fresh snapshot. */
export class AudioReplay {
  private readonly seen = new Set<number>();
  private floor = 0;

  private claim(id: number): boolean {
    if (id <= this.floor || this.seen.has(id)) return false;
    this.seen.add(id);
    if (this.seen.size > 200) {
      const oldest = Math.min(...this.seen);
      this.seen.delete(oldest);
      this.floor = Math.max(this.floor, oldest);
    }
    return true;
  }

  connect(
    listen: (handler: (event: AudioEvent) => void) => Promise<Stop>,
    snapshot: () => Promise<LogEntry[]>,
    show: (message: string, warn: boolean) => void,
    refresh: () => void,
  ): Stop {
    let active = true;
    const listening = listen((event) => {
      if (!active) return;
      if (this.claim(event.id)) show(event.message, eventWarns(event.kind));
      refresh();
    });
    void listening.then(async () => {
      if (!active) return;
      try {
        const entries = await snapshot();
        if (!active) return;
        for (const entry of entries) {
          if (entry.level !== "info" && this.claim(entry.id)) show(entry.message, true);
        }
      } catch { /* A failed history query must leave live warnings connected. */ }
    }, () => {});
    return () => {
      active = false;
      void listening.then((stop) => stop(), () => {});
    };
  }
}
