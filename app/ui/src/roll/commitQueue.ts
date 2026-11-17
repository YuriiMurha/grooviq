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
export type NotesCommand = () => Promise<NotesAck | null>;

/** What waits in the queue: an edit of the roll's, or a command. */
type Queued = { edit: LocalEdit } | { command: NotesCommand };

/** Sends the piano roll's edits to the engine one at a time, in order. Each edit shows in the
 * store at once; added notes keep temporary negative ids until the engine's reply names them,
 * and later edits are translated before they go out. A reply asking for a reload, a failure, or
 * a newer notes revision announced by the engine refetches the notes once nothing is in flight.
 * Undo and redo take their turn in the same line ([`run`]), so they never overtake an edit. */
export class CommitQueue {
  private pending: Queued[] = [];
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

  constructor(
    private store: NoteStore,
    private api: NotesApi,
    private hooks: QueueHooks = { onError: () => {}, onView: () => {} },
  ) {
    this.seen = store.revision;
    this.announced = store.revision;
  }

  /** Where failures and returned views go (the piano roll sets them once mounted). */
  setHooks(hooks: QueueHooks) {
    this.hooks = hooks;
  }

  /** What the piano roll's held gesture shows but has not sent: a fetch replaces the store, so
   * these are put back on top of it, with the selection the gesture made. */
  setHeld(held: () => LocalEdit | null) {
    this.held = held;
  }

  commit(edit: LocalEdit) {
    this.store.apply(edit);
    this.pending.push({ edit });
    this.pump();
  }

  /** Runs `command` (undo, redo) once every edit before it has been answered; edits made after
   * wait for it. Its ack's view is kept and the notes are fetched again afterwards; `null`
   * (nothing to do) changes nothing. */
  run(command: NotesCommand) {
    this.pending.push({ command });
    this.pump();
  }

  /** The engine announced a notes revision (`notes-changed`). */
  notesChanged(revision: number) {
    if (revision > this.announced) this.announced = revision;
    this.pump();
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
    if (this.busy) return;
    const next = this.pending.shift();
    if (next && "command" in next) {
      this.runNow(next.command);
    } else if (next) {
      this.send(next.edit);
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

  private send(local: LocalEdit) {
    this.busy = true;
    const t = this.translate(local);
    const add: NewNote[] = t.add.map((n) => ({
      channel: n.channel,
      tick: n.tick,
      length: n.length,
      key: n.key,
      velocity: n.velocity,
      slide: n.slide,
    }));
    const edit: NotesEdit = { baseRevision: this.store.revision, add, update: t.update, delete: t.delete };
    this.api.notesEdit(edit).then(
      (ack) => {
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
        this.hooks.onError(errorText(e));
        // What was built on the failed edit cannot be sent: the engine's notes replace it all.
        this.pending = [];
        this.reload = true;
        this.done();
      },
    );
  }

  private runNow(command: NotesCommand) {
    this.busy = true;
    command().then(
      (ack) => {
        if (ack) {
          // The store's notes are the ones before the command: edits that go out before the
          // fetch keep the store's older revision, so the engine sees them as stale too.
          this.reload = true;
          this.hooks.onView(ack.view);
        }
        this.done();
      },
      (e: unknown) => {
        this.hooks.onError(errorText(e));
        this.done();
      },
    );
  }

  private fetch() {
    this.busy = true;
    this.reload = false;
    this.api.patternNotes().then(
      ({ revision, notes }) => {
        const selected = this.store.selectedIds();
        this.store.load(revision, notes);
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
