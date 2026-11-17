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
  const api: NotesApi = {
    notesEdit: (edit) => {
      sent.push(structuredClone(edit));
      return new Promise((resolve, reject) => replies.push({ resolve, reject }));
    },
    patternNotes: () => {
      fetches++;
      return Promise.resolve(structuredClone(server));
    },
  };
  const ack = (a: Partial<NotesAck>) => {
    replies.shift()!.resolve({ revision: 0, addedIds: [], ignoredIds: [], reload: false, view: VIEW, ...a });
    return flush();
  };
  const fail = (e: unknown) => {
    replies.shift()!.reject(e);
    return flush();
  };
  return { api, sent, ack, fail, fetches: () => fetches, pending: () => replies.length };
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
      baseRevision: 4,
      add: [{ channel: 1, tick: 480, length: 240, key: 60, velocity: 0.8, slide: false }],
      update: [],
      delete: [],
    });

    await engine.ack({ revision: 5, addedIds: [40] });
    // The second edit names the engine's id, against the new revision.
    expect(engine.sent).toHaveLength(2);
    expect(engine.sent[1]).toEqual({ baseRevision: 5, add: [], update: [note(40, 960, 62)], delete: [] });
    expect(views).toEqual([VIEW]);

    await engine.ack({ revision: 6 });
    expect(engine.sent[2]).toEqual({
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
});
