import { registerSettlement } from "../project/pending";
import type { PatternStamp } from "../api/patterns";
import type { NewNote, NotesAck, NotesEdit, WireNote } from "../api/notes";
import { errorText, type ProjectView } from "../api/project";
import type { LocalEdit, NoteStore } from "./noteStore";

/** The engine's notes commands (api/notes.ts), passed in so tests and the bench can stand in. */
export interface NotesApi {
  notesEdit(edit: NotesEdit): Promise<NotesAck>;
  patternNotes(): Promise<{ revision: number; notes: WireNote[] }>;
}

export interface QueueHooks {
  /** A failed edit or fetch, fit to show. */
  onError(message: string): void;
  /** The project view an edit returned. */
  onView(view: ProjectView): void;
}

/** A command that changes the notes without an edit of the roll's own (undo, redo). */
export type NotesCommand = (origin?: PatternStamp) => Promise<NotesAck | null>;

/** The queues with an edit queued or unanswered, the roll that made them open or not. */
const unsettledQueues = new Set<CommitQueue>();

/** A piano-roll edit is queued or waits for its reply: the project may hold changes its view
 * does not show as unsaved yet. */
export function rollEditsUnsettled(): boolean {
  return unsettledQueues.size > 0;
}

/** Resolves once every piano-roll edit queued now has been answered: what a save must hold.
 * `false` if any of them was refused or failed (the project lacks what the person drew). */
export function rollEditsSettled(): Promise<boolean> {
  return Promise.all([...unsettledQueues].map((q) => q.flush())).then((all) => all.every(Boolean));
}

registerSettlement(rollEditsSettled);

/** What waits in the queue: an edit of the roll's, or a command, each for the project it was
 * made in (its generation). */
export type BarrierResult<Result> = { status: "completed"; value: Result } |
  { status: "cancelled"; reason: "stale" | "blocked" | "detached" | "priorFailure" | "failed"; error?: unknown };
type BarrierEntry = { epoch: number; valid: () => boolean; execute: () => Promise<void>;
  cancel: (reason: Extract<BarrierResult<never>, { status: "cancelled" }>["reason"]) => void };
type Queued = ({ edit: LocalEdit } | { command: NotesCommand } | { barrier: BarrierEntry }) & { stamp: PatternStamp };

/** Sends the piano roll's edits to the engine one at a time, in order. Each edit shows in the
 * store at once; added notes keep temporary negative ids until the engine's reply names them,
 * and later edits are translated before they go out. A reply asking for a reload, a failure, or
 * a newer notes revision announced by the engine refetches the notes once nothing is in flight.
 * Undo and redo take their turn in the same line ([`run`]), so they never overtake an edit.
 *
 * Everything belongs to one project: each edit carries the generation it was made in, and a New,
 * Open or Restore ([`replacing`]) drops what is queued, ignores replies still out and sends
 * nothing until it is over ([`reopened`]). A newer revision of the same project (recording added
 * notes) is only stale: the edit still goes, and the notes are fetched again. */
export class CommitQueue {
  private pending: Queued[] = [];
  private barriers = new Set<BarrierEntry>();
  private historyInvalidation: { stamp: PatternStamp; epoch: number } | null = null;
  private busy = false;
  /** Temporary id → the engine's id. */
  private real = new Map<number, number>();
  /** The newest revision the queue has been in step with, and the newest one announced. */
  private seen: number;
  private announced: number;
  private reload = false;
  private waiters: (() => void)[] = [];
  /** The notes a gesture holds that were never sent (a stamp or a paint stroke being held). */
  private held: () => LocalEdit | null = () => null;
  /** Moves on with every replacement: a reply to anything sent before it is ignored. */
  private epoch = 0;
  /** A replacement is on its way: nothing is queued or sent. */
  private closed = false;
  /** An edit or command was sent and its reply is not in yet. */
  private sending = false;
  /** Edits and commands refused or failed so far (see [`flush`]). */
  private failures = 0;
  private acknowledgedFailures = 0;
  private ownRevisions = new Map<number, number>();
  /** The open project (`ProjectView.generation`). */
  private generation = 0;
  private patternId = 0;

  setPatternId(patternId: number) {
    if (patternId === this.patternId) return;
    this.replacing();
    this.patternId = patternId;
    this.seen = 0; this.announced = 0;
    this.reopened();
  }
  stamp(): PatternStamp { return { generation: this.generation, patternId: this.patternId, revision: this.store.revision }; }
  private revisionFrom(revision: number): number {
    const visited = new Set<number>();
    while (this.ownRevisions.has(revision) && !visited.has(revision)) {
      visited.add(revision); revision = this.ownRevisions.get(revision)!;
    }
    return revision;
  }
  rejectHeld() {
    this.failures += 1; this.reload = true;
    this.hooks.onError("The pattern changed before this gesture finished. Reload its notes.");
    this.pump();
  }
  accepts(origin: PatternStamp): boolean {
    return !this.closed && origin.generation === this.generation && origin.patternId === this.patternId
      && this.revisionFrom(origin.revision) === this.store.revision;
  }

  constructor(
    private store: NoteStore,
    private api: NotesApi,
    private hooks: QueueHooks = { onError: () => {}, onView: () => {} },
  ) {
    this.seen = store.revision;
    this.announced = store.revision;
  }

  /** The project shown now: edits made from here on are its. */
  setGeneration(generation: number) {
    this.generation = generation;
  }


  /** Where failures and returned views go (the piano roll sets them once mounted). */
  setHooks(hooks: QueueHooks) {
    this.hooks = hooks;
  }

  /** What the piano roll's held gesture shows but has not sent: a fetch replaces the store, so
   * these are put back on top of it, with the selection the gesture made. */
  setHeld(held: () => LocalEdit | null) {
    this.held = held;
    return () => { this.held = () => null; };
  }
  hasHeld(): boolean { return this.held() !== null; }

  /** Shows `edit` and queues it, for the project of `generation` (a held gesture's: the one it
   * started in). An edit for another project, or made while a replacement is on its way, is
   * dropped unshown. */
  commit(edit: LocalEdit, stamp: PatternStamp = this.stamp()) {
    if (!this.accepts(stamp)) { this.failures += 1; this.track(); return; }
    this.store.apply(edit);
    this.pending.push({ edit, stamp });
    this.pump();
  }

  /** Runs `command` (undo, redo) once every edit before it has been answered; edits made after
   * wait for it. Its ack's view is kept and the notes are fetched again afterwards; `null`
   * (nothing to do) changes nothing. */
  run(command: NotesCommand) {
    if (this.closed) return;
    this.pending.push({ command, stamp: this.stamp() });
    this.pump();
  }

  /** One entry in this queue; settlement must happen BEFORE requesting it. */
  barrier<Result>(command: (still: () => boolean) => Promise<Result>, activeDrag: () => boolean,
    accepted: (value: Result) => boolean = () => true): Promise<BarrierResult<Result>> {
    if (this.closed) return Promise.resolve({ status: "cancelled", reason: "stale" });
    if (activeDrag()) return Promise.resolve({ status: "cancelled", reason: "blocked" });
    const stamp = this.stamp(); const epoch = this.epoch;
    return new Promise((resolve) => {
      let settled = false;
      const finish = (result: BarrierResult<Result>) => {
        if (settled) return;
        settled = true; this.barriers.delete(entry); resolve(result);
      };
      const valid = () => !settled && !this.closed && this.epoch === epoch &&
        this.generation === stamp.generation && this.patternId === stamp.patternId && !activeDrag();
      const entry: BarrierEntry = { epoch, valid,
        cancel: (reason) => { if (!settled) { this.failures++; finish({ status: "cancelled", reason }); } },
        execute: async () => {
          if (!valid()) { entry.cancel(activeDrag() ? "blocked" : "stale"); return; }
          try {
            const value = await command(valid);
            if (!valid()) { entry.cancel(activeDrag() ? "blocked" : "stale"); return; }
            if (!accepted(value)) this.failures++;
            finish({ status: "completed", value });
          } catch (error) {
            if (!settled) { this.failures++; finish({ status: "cancelled", reason: "failed", error }); }
          }
        } };
      this.barriers.add(entry);
      this.pending.push({ barrier: entry, stamp }); this.pump();
    });
  }

  /** Detaching the view cancels only barriers; ordinary unmounted note edits still settle. */
  detachBarriers() {
    for (const barrier of [...this.barriers]) barrier.cancel("detached");
  }
  private cancelPendingBarriers(reason: "stale" | "priorFailure") {
    for (const next of this.pending) if ("barrier" in next) next.barrier.cancel(reason);
  }

  /** A New, Open or Restore is about to be sent: what is queued for this project is dropped, a
   * reply still out is ignored when it comes, and nothing goes out until [`reopened`]. */
  replacing() {
    this.closed = true;
    for (const barrier of [...this.barriers]) barrier.cancel("stale");
    this.epoch += 1;
    this.historyInvalidation = null;
    this.pending = [];
    this.real.clear();
    this.ownRevisions.clear();
    this.reload = true;
    // A reply or fetch still out is ignored: the queue is free for the project that comes next.
    this.sending = false;
    this.busy = false;
    this.track();
  }

  /** The replacement finished or failed: edits go out again, and the notes are fetched. */
  reopened() {
    this.closed = false;
    this.reload = true;
    this.pump();
  }

  /** An edit is queued or waits for its reply: the engine's project may not have it yet. */
  unsettled(): boolean {
    return this.sending || this.pending.length > 0;
  }

  /** Keeps [`rollEditsUnsettled`] up to date. */
  private track() {
    if (this.unsettled() || this.failures > this.acknowledgedFailures) unsettledQueues.add(this);
    else unsettledQueues.delete(this);
  }

  /** The engine announced a notes revision (`notes-changed`). */
  /** Native effect target only. Do not certify the store's stale notes as this revision. */
  invalidateHistory(stamp: PatternStamp) {
    if (this.closed || stamp.generation !== this.generation || stamp.patternId !== this.patternId) return;
    this.historyInvalidation = { stamp: { ...stamp }, epoch: this.epoch };
    if (!this.busy) this.pump();
  }

  notesChanged(revision: number) {
    if (revision > this.announced) this.announced = revision;
    this.pump();
  }

  /** Resolves once everything queued now has been answered: `true` if all of it was applied,
   * `false` if any edit or command was refused or failed (and what was built on it dropped). */
  async flush(): Promise<boolean> {
    const before = this.acknowledgedFailures;
    await this.idle();
    this.acknowledgedFailures = this.failures;
    this.track();
    return this.failures === before;
  }

  /** Resolves once nothing is queued, in flight or being fetched. */
  idle(): Promise<void> {
    if (!this.busy && !this.pending.length && !this.needsFetch()) return Promise.resolve();
    return new Promise((resolve) => this.waiters.push(resolve));
  }

  private needsFetch() {
    return this.reload || this.announced > this.seen;
  }

  private pump() {
    this.track();
    if (this.busy || this.closed) return;
    const invalidation = this.historyInvalidation;
    this.historyInvalidation = null;
    if (invalidation && invalidation.epoch === this.epoch &&
      invalidation.stamp.generation === this.generation && invalidation.stamp.patternId === this.patternId) {
      this.reload = true;
      this.announced = Math.max(this.announced, invalidation.stamp.revision);
    }
    // Made in a project that is not the open one: never sent (the engine would refuse it too).
    while (this.pending.length && (this.pending[0].stamp.generation !== this.generation || this.pending[0].stamp.patternId !== this.patternId)) {
      const dropped = this.pending.shift();
      if (dropped && "barrier" in dropped) dropped.barrier.cancel("stale");
      this.failures += 1;
    }
    const next = this.pending.shift();
    if (next && "barrier" in next) {
      this.runBarrier(next.barrier);
      this.track();
    } else if (next && "command" in next) {
      this.runNow(next.command, next.stamp);
      this.track();
    } else if (next) {
      this.send(next.edit, next.stamp);
      this.track();
    } else if (this.needsFetch()) {
      this.fetch();
    } else {
      for (const w of this.waiters.splice(0)) w();
    }
  }

  private mapId = (id: number) => (id < 0 ? this.real.get(id) : id);

  /** The edit in the store's ids: temporary ids the engine has named become its ids; the rest
   * stay temporary, as the store still holds them. */
  private localize(edit: LocalEdit): LocalEdit {
    const local = (id: number) => this.mapId(id) ?? id;
    return {
      add: edit.add,
      update: edit.update.map((u) => ({ ...u, id: local(u.id) })),
      delete: edit.delete.map(local),
    };
  }

  /** The edit for the engine, which knows only its own ids: parts naming a note whose add never
   * landed are dropped. */
  private translate(edit: LocalEdit): LocalEdit {
    const update: WireNote[] = [];
    for (const u of edit.update) {
      const id = this.mapId(u.id);
      if (id !== undefined) update.push({ ...u, id });
    }
    const del: number[] = [];
    for (const d of edit.delete) {
      const id = this.mapId(d);
      if (id !== undefined) del.push(id);
    }
    return { add: edit.add, update, delete: del };
  }

  private send(local: LocalEdit, stamp: PatternStamp) {
    this.busy = true;
    this.sending = true;
    const epoch = this.epoch;
    const t = this.translate(local);
    const add: NewNote[] = t.add.map((n) => ({
      channel: n.channel,
      tick: n.tick,
      length: n.length,
      key: n.key,
      velocity: n.velocity,
      slide: n.slide,
    }));
    const edit: NotesEdit = { generation: stamp.generation, patternId: stamp.patternId, baseRevision: this.revisionFrom(stamp.revision), add, update: t.update, delete: t.delete };
    this.api.notesEdit(edit).then(
      (ack) => {
        // Sent for a project that was replaced since: its ids and view are that project's.
        if (epoch !== this.epoch) return;
        this.sending = false;
        if ((ack.view.generation !== undefined && ack.view.generation !== stamp.generation) ||
            (ack.view.pattern && ack.view.pattern.id !== stamp.patternId)) {
          this.failures += 1; this.cancelPendingBarriers("priorFailure"); this.pending = []; this.reload = true; this.done(); return;
        }
        if (ack.revision !== edit.baseRevision) this.ownRevisions.set(edit.baseRevision, ack.revision);
        if (this.ownRevisions.size > 200) this.ownRevisions.delete(this.ownRevisions.keys().next().value!);
        local.add.forEach((n, i) => {
          const id = ack.addedIds[i];
          if (id === undefined) return;
          this.real.set(n.id, id);
          this.store.remap(n.id, id);
        });
        this.store.revision = ack.revision;
        this.seen = Math.max(this.seen, ack.revision);
        if (ack.reload) this.reload = true;
        this.hooks.onView(ack.view);
        this.done();
      },
      (e: unknown) => {
        if (epoch !== this.epoch) return;
        this.sending = false;
        this.failures += 1;
        this.hooks.onError(errorText(e));
        // What was built on the failed edit cannot be sent: the engine's notes replace it all.
        this.cancelPendingBarriers("priorFailure");
        this.pending = [];
        this.reload = true;
        this.done();
      },
    );
  }

  private runBarrier(barrier: BarrierEntry) {
    this.busy = true; this.sending = true;
    void barrier.execute().finally(() => {
      // A late old-project answer cannot release a newer epoch's in-flight command.
      if (this.epoch !== barrier.epoch) return;
      this.sending = false; this.done();
    });
  }

  private runNow(command: NotesCommand, origin: PatternStamp) {
    this.busy = true;
    this.sending = true;
    const epoch = this.epoch;
    const stamp = { ...origin, revision: this.revisionFrom(origin.revision) };
    command(stamp).then(
      (ack) => {
        if (epoch !== this.epoch) return;
        this.sending = false;
        if (ack) {
          if ((ack.view.generation !== undefined && ack.view.generation !== stamp.generation) ||
              (ack.view.pattern && ack.view.pattern.id !== stamp.patternId)) {
            this.failures += 1; this.cancelPendingBarriers("priorFailure"); this.pending = []; this.reload = true; this.done(); return;
          }
          if (ack.revision !== stamp.revision) this.ownRevisions.set(stamp.revision, ack.revision);
          this.store.revision = ack.revision;
          // The store's notes are the ones before the command: edits that go out before the
          // fetch keep the store's older revision, so the engine sees them as stale too.
          this.reload = true;
          this.hooks.onView(ack.view);
        }
        this.done();
      },
      (e: unknown) => {
        if (epoch !== this.epoch) return;
        this.sending = false;
        this.failures += 1;
        this.hooks.onError(errorText(e));
        this.cancelPendingBarriers("priorFailure");
        this.done();
      },
    );
  }

  private fetch() {
    this.busy = true;
    this.reload = false;
    const epoch = this.epoch;
    this.api.patternNotes().then(
      ({ revision, notes }) => {
        // Fetched before a replacement: the queue fetches again once it is over.
        if (epoch !== this.epoch) return;
        const selected = this.store.selectedIds();
        this.store.load(revision, notes);
        this.ownRevisions.clear();
        // Edits made while the fetch ran are not in it yet: show them again on top, with the
        // temporary ids the store and the selection know them by.
        for (const e of this.pending) if ("edit" in e) this.store.apply(this.localize(e.edit));
        const held = this.held();
        if (held) this.store.apply(held);
        this.store.setSelection(selected);
        const progressed = revision > this.seen;
        this.seen = Math.max(this.seen, revision);
        // A revision announced while the fetch ran is fetched next, unless the engine's own
        // revision stopped moving (an announcement it can never reach).
        if (!progressed) this.announced = Math.min(this.announced, this.seen);
        this.done();
      },
      (e: unknown) => {
        if (epoch !== this.epoch) return;
        this.hooks.onError(errorText(e));
        this.announced = this.seen;
        this.done();
      },
    );
  }

  private done() {
    this.busy = false;
    this.pump();
  }
}
