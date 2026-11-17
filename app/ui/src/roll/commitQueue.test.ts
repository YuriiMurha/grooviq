import { describe, expect, it } from "vitest";
import type { NotesAck, NotesEdit, WireNote } from "../api/notes";
import type { ProjectView } from "../api/project";
import { CommitQueue, type NotesApi } from "./commitQueue";
import { NoteStore } from "./noteStore";

const note = (id: number, tick: number, key = 60): WireNote => ({ id, channel: 1, tick, length: 240, key, velocity: 0.8, slide: false });

const VIEW = { name: "v" } as ProjectView;

/** An engine whose replies the test hands out one at a time. */
function fakeApi(server: { revision: number; notes: WireNote[] }) {
  const sent: NotesEdit[] = [];
  const replies: { resolve: (a: NotesAck) => void; reject: (e: unknown) => void }[] = [];
  let fetches = 0;
  /** When set, fetches wait for `release` and answer with the server as it is then. */
  let hold = false;
  const held: (() => void)[] = [];
  const api: NotesApi = {
    notesEdit: (edit) => {
      sent.push(structuredClone(edit));
      return new Promise((resolve, reject) => replies.push({ resolve, reject }));
    },
    patternNotes: () => {
      fetches++;
      if (!hold) return Promise.resolve(structuredClone(server));
      return new Promise((resolve) => held.push(() => resolve(structuredClone(server))));
    },
  };
  const holdFetches = (on: boolean) => {
    hold = on;
  };
  /** Answers the oldest held fetch with the server's notes now. */
  const release = () => {
    held.shift()!();
    return flush();
  };
  const ack = (a: Partial<NotesAck>) => {
    replies.shift()!.resolve({ revision: 0, addedIds: [], ignoredIds: [], reload: false, view: VIEW, ...a });
    return flush();
  };
  const fail = (e: unknown) => {
    replies.shift()!.reject(e);
    return flush();
  };
  return { api, sent, ack, fail, holdFetches, release, fetches: () => fetches, pending: () => replies.length };
}

/** Lets every settled promise run its callbacks. */
const flush = () => new Promise<void>((r) => setTimeout(r, 0));

function setup(initial: WireNote[], revision = 4) {
  const store = new NoteStore();
  store.load(revision, initial);
  const server = { revision, notes: structuredClone(initial) };
  const engine = fakeApi(server);
  const errors: string[] = [];
  const views: ProjectView[] = [];
  const queue = new CommitQueue(store, engine.api, { onError: (m) => errors.push(m), onView: (v) => views.push(v) });
  return { store, server, engine, errors, views, queue };
}

describe("commit queue", () => {
  it("temporary_ids_map_to_real_ids_before_the_next_send", async () => {
    const { store, engine, queue, views } = setup([note(1, 0)]);
    // Draw a note, then move it and delete note 1 before the engine has answered the first edit.
    queue.commit({ add: [note(-1, 480)], update: [], delete: [] });
    queue.commit({ add: [], update: [note(-1, 960, 62)], delete: [] });
    queue.commit({ add: [note(-2, 1440)], update: [], delete: [1, -1] });
    // Shown at once; one edit in flight.
    expect(store.get(-1)).toBeUndefined();
    expect(store.get(-2)).toEqual(note(-2, 1440));
    expect(engine.sent).toHaveLength(1);
    expect(engine.sent[0]).toEqual({
      generation: 0,
      baseRevision: 4,
      add: [{ channel: 1, tick: 480, length: 240, key: 60, velocity: 0.8, slide: false }],
      update: [],
      delete: [],
    });

    await engine.ack({ revision: 5, addedIds: [40] });
    // The second edit names the engine's id, against the new revision.
    expect(engine.sent).toHaveLength(2);
    expect(engine.sent[1]).toEqual({ generation: 0, baseRevision: 5, add: [], update: [note(40, 960, 62)], delete: [] });
    expect(views).toEqual([VIEW]);

    await engine.ack({ revision: 6 });
    expect(engine.sent[2]).toEqual({
      generation: 0,
      baseRevision: 6,
      add: [{ channel: 1, tick: 1440, length: 240, key: 60, velocity: 0.8, slide: false }],
      update: [],
      delete: [1, 40],
    });
    await engine.ack({ revision: 7, addedIds: [41] });
    await queue.idle();
    // The store holds the engine's ids now.
    expect(store.get(-2)).toBeUndefined();
    expect(store.get(41)).toEqual(note(41, 1440));
    expect(store.revision).toBe(7);
    expect(store.count).toBe(1);
    expect(engine.fetches()).toBe(0);
  });

  it("an_undo_waits_for_the_edit_in_flight_then_the_notes_are_refetched", async () => {
    const { store, server, engine, queue, views } = setup([note(1, 0)]);
    queue.commit({ add: [], update: [note(1, 480)], delete: [] });
    const order: string[] = [];
    let answer: (ack: NotesAck | null) => void = () => {};
    queue.run(() => {
      order.push(`undo after ${engine.sent.length} edit(s), ${engine.pending()} unanswered`);
      return new Promise((resolve) => (answer = resolve));
    });
    // The undo stays queued while the move is in flight.
    await flush();
    expect(order).toEqual([]);
    server.revision = 5;
    server.notes = [note(1, 480)];
    await engine.ack({ revision: 5 });
    expect(order).toEqual(["undo after 1 edit(s), 0 unanswered"]);
    // An edit made meanwhile waits behind the undo.
    queue.commit({ add: [], update: [note(1, 960)], delete: [] });
    expect(engine.sent).toHaveLength(1);
    server.revision = 6;
    server.notes = [note(1, 0)];
    answer({ revision: 6, addedIds: [], ignoredIds: [], reload: true, view: { name: "undone" } as ProjectView });
    await flush();
    expect(views.at(-1)?.name).toBe("undone");
    // The edit goes out against the revision the roll last held: stale, so the engine says reload.
    expect(engine.sent).toHaveLength(2);
    expect(engine.sent[1].baseRevision).toBe(5);
    server.revision = 7;
    server.notes = [note(1, 960)];
    await engine.ack({ revision: 7, reload: true });
    await queue.idle();
    expect(engine.fetches()).toBe(1);
    expect(store.revision).toBe(7);
    expect(store.get(1)).toEqual(note(1, 960));
  });

  it("an_undo_alone_refetches_the_notes", async () => {
    const { store, server, engine, queue } = setup([note(1, 0), note(2, 480)]);
    server.revision = 5;
    server.notes = [note(1, 0)];
    queue.run(() => Promise.resolve({ revision: 5, addedIds: [], ignoredIds: [], reload: true, view: VIEW }));
    await queue.idle();
    expect(engine.fetches()).toBe(1);
    expect(store.revision).toBe(5);
    expect(store.get(2)).toBeUndefined();
  });

  it("an_undo_with_nothing_to_undo_or_refused_fetches_nothing", async () => {
    const { engine, queue, errors } = setup([note(1, 0)]);
    queue.run(() => Promise.resolve(null));
    await queue.idle();
    queue.run(() => Promise.reject("The audio engine did not take the change (full)."));
    await queue.idle();
    expect(engine.fetches()).toBe(0);
    expect(errors).toEqual(["The audio engine did not take the change (full)."]);
  });

  it("a_reply_with_reload_refetches_the_notes", async () => {
    const { store, server, engine, queue } = setup([note(1, 0), note(2, 240)]);
    queue.commit({ add: [], update: [note(1, 3840)], delete: [] });
    // Meanwhile a recording changed the pattern: the engine applied the edit to a newer revision.
    server.revision = 9;
    server.notes = [note(1, 3840), note(2, 240), note(3, 720, 64)];
    await engine.ack({ revision: 9, reload: true });
    await queue.idle();
    expect(engine.fetches()).toBe(1);
    expect(store.revision).toBe(9);
    expect(store.count).toBe(3);
    expect(store.get(3)).toEqual(note(3, 720, 64));
    // The next edit is made against it.
    queue.commit({ add: [], update: [], delete: [3] });
    expect(engine.sent[1].baseRevision).toBe(9);
  });

  it("an_error_refetches_and_reports", async () => {
    const { store, server, engine, queue, errors } = setup([note(1, 0)]);
    queue.commit({ add: [note(-1, 480)], update: [], delete: [] });
    queue.commit({ add: [], update: [note(-1, 960)], delete: [] });
    expect(store.count).toBe(2);
    await engine.fail("A pattern holds at most 100000 notes.");
    await queue.idle();
    expect(errors).toEqual(["A pattern holds at most 100000 notes."]);
    // The store is the engine's again; the edit that built on the failed one is never sent.
    expect(engine.fetches()).toBe(1);
    expect(engine.sent).toHaveLength(1);
    expect(store.count).toBe(1);
    expect(store.get(1)).toEqual(note(1, 0));
    expect(store.revision).toBe(server.revision);
    // Later edits go out as usual.
    queue.commit({ add: [], update: [note(1, 240)], delete: [] });
    expect(engine.sent).toHaveLength(2);
  });

  it("a_newer_notes_changed_refetches_when_idle", async () => {
    const { store, server, engine, queue } = setup([note(1, 0)]);
    // Its own revision, or an older one, changes nothing.
    queue.notesChanged(4);
    queue.notesChanged(3);
    await flush();
    expect(engine.fetches()).toBe(0);

    // A newer one while an edit is in flight waits for it.
    queue.commit({ add: [], update: [note(1, 240)], delete: [] });
    server.revision = 6;
    server.notes = [note(1, 240), note(2, 960)];
    queue.notesChanged(6);
    await flush();
    expect(engine.fetches()).toBe(0);
    expect(store.get(1)).toEqual(note(1, 240));
    await engine.ack({ revision: 5 });
    await queue.idle();
    expect(engine.fetches()).toBe(1);
    expect(store.revision).toBe(6);
    expect(store.get(2)).toEqual(note(2, 960));

    // An edit's own revision announced afterwards is not newer.
    queue.commit({ add: [], update: [note(1, 480)], delete: [] });
    await engine.ack({ revision: 7 });
    queue.notesChanged(7);
    await queue.idle();
    expect(engine.fetches()).toBe(1);

    // Idle: fetched straight away.
    server.revision = 8;
    queue.notesChanged(8);
    await queue.idle();
    expect(engine.fetches()).toBe(2);
    expect(store.revision).toBe(8);
  });

  it("a_revision_announced_during_a_fetch_is_fetched_too", async () => {
    const { store, server, engine, queue } = setup([note(1, 0)]);
    engine.holdFetches(true);
    // Revision 5 starts a fetch; revision 6 is announced before that fetch answers with 5.
    server.revision = 5;
    server.notes = [note(1, 0), note(2, 240)];
    queue.notesChanged(5);
    await flush();
    expect(engine.fetches()).toBe(1);
    queue.notesChanged(6);
    await engine.release();
    // The engine moved on meanwhile: the queue fetches again rather than settle at 5.
    server.revision = 6;
    server.notes = [note(1, 0), note(2, 240), note(3, 480)];
    expect(engine.fetches()).toBe(2);
    await engine.release();
    await queue.idle();
    expect(store.revision).toBe(6);
    expect(store.get(3)).toEqual(note(3, 480));
  });

  it("edits_made_during_a_fetch_stay_in_the_mirror", async () => {
    const { store, server, engine, queue } = setup([note(1, 0), note(2, 240)]);
    engine.holdFetches(true);
    server.revision = 5;
    server.notes = [note(1, 0), note(2, 240), note(3, 3840, 70)];
    queue.notesChanged(5);
    await flush();
    // While the fetch runs: draw a note (selected), move it, delete note 2 and move note 1.
    queue.commit({ add: [note(-1, 480)], update: [], delete: [] });
    store.setSelection([-1, 1]);
    queue.commit({ add: [], update: [note(-1, 960, 62)], delete: [] });
    queue.commit({ add: [], update: [note(1, 120)], delete: [2] });
    await engine.release();
    // The engine's notes, with the edits on top, temporary id and all.
    expect(store.get(-1)).toEqual(note(-1, 960, 62));
    expect(store.get(1)).toEqual(note(1, 120));
    expect(store.get(2)).toBeUndefined();
    expect(store.get(3)).toEqual(note(3, 3840, 70));
    expect(store.selectedIds().sort()).toEqual([-1, 1]);

    // Then they go out, in order, with the engine's ids.
    expect(engine.sent[0].add).toEqual([{ channel: 1, tick: 480, length: 240, key: 60, velocity: 0.8, slide: false }]);
    await engine.ack({ revision: 6, addedIds: [40] });
    expect(engine.sent[1]).toMatchObject({ update: [note(40, 960, 62)], delete: [] });
    await engine.ack({ revision: 7 });
    expect(engine.sent[2]).toMatchObject({ update: [note(1, 120)], delete: [2] });
    await engine.ack({ revision: 8 });
    await queue.idle();
    // The mirror is what the engine now holds.
    const mirror = [1, 3, 40].map((id) => store.get(id));
    expect(mirror).toEqual([note(1, 120), note(3, 3840, 70), note(40, 960, 62)]);
    expect(store.count).toBe(3);
    expect(store.selectedIds().sort((a, b) => a - b)).toEqual([1, 40]);
  });

  it("a_refetch_keeps_the_notes_held_by_a_gesture_and_their_selection", async () => {
    const { store, server, engine, queue } = setup([note(1, 0)]);
    // A chord held down: two unsent notes in the store, selected, with the gesture owning them.
    const fresh = [note(-1, 960), note(-2, 960, 64)];
    store.apply({ add: fresh, update: [], delete: [] });
    store.setSelection([-1, -2]);
    queue.setHeld(() => ({ add: fresh, update: [], delete: [] }));
    // The engine announces a change from elsewhere: the notes are fetched and replace the store.
    server.revision = 5;
    server.notes = [note(1, 0), { ...note(9, 480), channel: 2 }];
    queue.notesChanged(5);
    await flush();
    expect(engine.fetches()).toBe(1);
    // The engine's notes are in; the held ones are still there, selected, and nothing was sent.
    expect(store.revision).toBe(5);
    expect(store.get(9)).toBeDefined();
    expect(store.get(-1)).toEqual(fresh[0]);
    expect(store.get(-2)).toEqual(fresh[1]);
    expect(store.selectedIds().sort((a, b) => a - b)).toEqual([-2, -1]);
    expect(engine.sent).toEqual([]);
    // Nothing held, nothing added back.
    queue.setHeld(() => null);
    queue.notesChanged(6);
    server.revision = 6;
    await flush();
    expect(store.get(-1)).toBeUndefined();
  });

  it("a_replacement_drops_queued_edits_and_ignores_the_reply_still_out", async () => {
    // Project A (generation 3): two notes drawn quickly, the first one's reply delayed.
    const { store, server, engine, queue, views } = setup([note(1, 0)]);
    queue.setGeneration(3);
    queue.commit({ add: [note(-1, 480)], update: [], delete: [] });
    queue.commit({ add: [note(-2, 960)], update: [], delete: [] });
    expect(engine.sent.map((e) => e.generation)).toEqual([3]);
    expect(queue.unsettled()).toBe(true);
    // Open B: the queue lets go of A's work, and takes nothing while B is on its way.
    queue.replacing();
    expect(queue.unsettled()).toBe(false);
    queue.commit({ add: [note(-3, 1440)], update: [], delete: [] });
    queue.run(() => Promise.resolve(null));
    expect(store.get(-3)).toBeUndefined();
    expect(queue.unsettled()).toBe(false);
    queue.setGeneration(4);
    server.revision = 9;
    server.notes = [note(7, 240, 72)];
    // A's late reply: its ids and its view are A's, and are not taken.
    await engine.ack({ revision: 5, addedIds: [2], view: { ...VIEW, generation: 3 } });
    expect(views).toEqual([]);
    expect(engine.sent).toHaveLength(1);
    expect(engine.fetches()).toBe(0);
    // B is in: its notes are fetched, and A's second note never went out.
    queue.reopened();
    await queue.idle();
    expect(engine.sent).toHaveLength(1);
    expect(store.revision).toBe(9);
    expect(store.get(7)).toEqual(note(7, 240, 72));
    expect(store.count).toBe(1);
    // B's own edits go out, as B's.
    queue.commit({ add: [], update: [note(7, 480, 72)], delete: [] });
    expect(engine.sent[1]).toMatchObject({ generation: 4, baseRevision: 9 });
  });

  it("an_edit_queued_in_another_project_is_never_sent", async () => {
    const { engine, queue } = setup([note(1, 0)]);
    queue.setGeneration(3);
    queue.commit({ add: [], update: [note(1, 240)], delete: [] });
    queue.commit({ add: [], update: [note(1, 480)], delete: [] });
    // The project changed without a replacement reaching the queue (it must still not cross).
    queue.setGeneration(4);
    await engine.ack({ revision: 5 });
    await flush();
    expect(engine.sent).toHaveLength(1);
  });

  it("a_failed_replacement_reopens_the_queue_for_the_same_project", async () => {
    const { engine, queue } = setup([note(1, 0)]);
    queue.replacing();
    queue.reopened();
    await queue.idle();
    expect(engine.fetches()).toBe(1);
    queue.commit({ add: [], update: [note(1, 240)], delete: [] });
    expect(engine.sent).toHaveLength(1);
    expect(queue.unsettled()).toBe(true);
    await engine.ack({ revision: 5 });
    await queue.idle();
    expect(queue.unsettled()).toBe(false);
  });
});
