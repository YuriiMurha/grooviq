// The one owner of an editor's parameter gestures. A gesture (a drag, a wheel turn, a point on a
// graph) previews live in the engine while it moves and ends exactly once: committed (one project
// edit) or cancelled (the engine back on the value it started from, no edit). Every way an editor
// can stop a gesture early goes through here: Escape, closing, a lost window focus, a pointer
// cancel, a preset, a removed channel or a replaced project. A gesture's own late events (its
// pointerup, its wheel timer) find it ended and do nothing.

/** What the owner talks to: the engine (live values) and the project (edits). */
export interface GestureSink {
  /** The value the project holds now. */
  stored(id: string): number;
  /** Sends a live value soon (coalesced to one per frame). */
  live(id: string, value: number): void;
  /** Drops a live value that has not gone out yet. */
  dropLive(id: string): void;
  /** Sends a live value now: the engine back on a value the project holds. */
  restore(id: string, value: number): void;
  /** Stores a value in the project; resolves to whether it was applied. */
  edit(id: string, value: number): Promise<boolean>;
  /** The values to show instead of the project's, by id, changed. */
  shown(moving: Readonly<Record<string, number>>): void;
}

/** One gesture on one parameter. After it ends every call does nothing. */
export interface Gesture {
  /** It has not ended yet. */
  alive(): boolean;
  /** A new value while it moves; `false` once the gesture has ended (stop tracking it). */
  input(value: number): boolean;
  /** Ends it, storing `value`. */
  commit(value: number): void;
  /** Ends it, putting the value back where it started. */
  cancel(): void;
}

interface Active {
  token: number;
  /** What was shown when it started: where a cancel puts it back. */
  origin: number;
  /** The engine heard a live value of it. */
  heard: boolean;
}

export const DEAD_GESTURE: Gesture = { alive: () => false, input: () => false, commit: () => {}, cancel: () => {} };

export class ParamGestures {
  private active = new Map<string, Active>();
  /** Values shown instead of the project's: moving, or committed and on their way. */
  private moving: Record<string, number> = {};
  /** The latest edit on its way per id: its number and value. */
  private pending = new Map<string, { seq: number; value: number }>();
  private nextToken = 1;
  private nextSeq = 1;
  private closed = false;
  /** The project was replaced: nothing may reach the engine or the project any more. */
  private abandoned = false;

  constructor(private readonly sink: GestureSink) {}

  /** Starts a gesture. A gesture already running on `id` (a wheel turn, then a drag) hands over
   * to it and keeps its start. */
  begin(id: string): Gesture {
    if (this.closed) return DEAD_GESTURE;
    const before = this.active.get(id);
    const token = this.nextToken++;
    this.active.set(id, { token, origin: before?.origin ?? this.shownValue(id), heard: before?.heard ?? false });
    const mine = () => !this.closed && this.active.get(id)?.token === token;
    return {
      alive: mine,
      input: (value) => {
        if (!mine()) return false;
        this.active.get(id)!.heard = true;
        this.moving = { ...this.moving, [id]: value };
        this.sink.live(id, value);
        this.sink.shown(this.moving);
        return true;
      },
      commit: (value) => {
        if (!mine()) return;
        const a = this.active.get(id)!;
        this.active.delete(id);
        this.sink.dropLive(id);
        if (value === a.origin) {
          if (a.heard) this.sink.restore(id, value);
          this.showSettled(id);
          return;
        }
        this.store(id, value, a.heard);
      },
      cancel: () => {
        if (mine()) this.end(id, true);
      },
    };
  }

  /** A one-shot change (a click, a key): one edit, after ending any gesture on `id`. */
  set(id: string, value: number) {
    if (this.closed) return;
    if (this.active.has(id)) this.end(id, true);
    if (value === this.shownValue(id)) return;
    this.store(id, value, false);
  }

  /** Cancels every running gesture (a preset is loading, the window lost focus). */
  cancelAll() {
    if (this.closed) return;
    for (const id of [...this.active.keys()]) this.end(id, true);
  }

  /** The editor closes: cancels every running gesture and ends the owner. */
  dispose() {
    this.cancelAll();
    this.closed = true;
  }

  /** The project was replaced: drops everything and sends nothing, not even a restore. */
  abandon() {
    for (const id of [...this.active.keys()]) this.sink.dropLive(id);
    this.active.clear();
    this.pending.clear();
    this.moving = {};
    this.closed = true;
    this.abandoned = true;
  }

  private shownValue(id: string): number {
    return this.moving[id] ?? this.sink.stored(id);
  }

  /** Ends a running gesture without storing it. */
  private end(id: string, restore: boolean) {
    const a = this.active.get(id)!;
    this.active.delete(id);
    this.sink.dropLive(id);
    if (restore && a.heard) this.sink.restore(id, a.origin);
    this.showSettled(id);
  }

  /** Stops showing a value of `id` unless an edit of it is still on its way. */
  private showSettled(id: string) {
    const p = this.pending.get(id);
    const next = { ...this.moving };
    if (p) next[id] = p.value;
    else delete next[id];
    this.moving = next;
    this.sink.shown(this.moving);
  }

  private store(id: string, value: number, heard: boolean) {
    const seq = this.nextSeq++;
    this.pending.set(id, { seq, value });
    this.moving = { ...this.moving, [id]: value };
    this.sink.shown(this.moving);
    void this.sink.edit(id, value).then((ok) => {
      if (this.abandoned) return;
      // A refused edit leaves the engine on the live value: put the stored one back.
      if (!ok && heard) this.sink.restore(id, this.sink.stored(id));
      if (this.pending.get(id)?.seq !== seq) return;
      this.pending.delete(id);
      if (!this.active.has(id)) this.showSettled(id);
    });
  }
}
