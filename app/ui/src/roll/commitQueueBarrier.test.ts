import { expect, it, vi } from "vitest";
import type { NotesAck } from "../api/notes";
import { CommitQueue, type NotesApi } from "./commitQueue";
import { NoteStore } from "./noteStore";

function deferred<Value>() {
  let resolve: (value: Value) => void = () => { throw new Error("uninitialized"); };
  let reject: (error: unknown) => void = () => { throw new Error("uninitialized"); };
  const promise = new Promise<Value>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}
function setup() {
  const store = new NoteStore(); store.load(1, []);
  const api: NotesApi = { notesEdit: async () => { throw new Error("unexpected edit"); }, patternNotes: async () => ({ revision: 1, notes: [] }) };
  return new CommitQueue(store, api);
}
const turn = async () => { for (let index = 0; index < 8; index++) await Promise.resolve(); };

it("holds one position behind prior notes and ahead of later commands through refresh", async () => {
  const queue = setup(); const prior = deferred<NotesAck | null>(); const refresh = deferred<void>(); const trace: string[] = [];
  queue.run(() => { trace.push("prior"); return prior.promise; });
  const barrier = queue.barrier(async () => { trace.push("history"); await refresh.promise; trace.push("refresh"); return 42; }, () => false);
  queue.run(async () => { trace.push("later"); return null; });
  expect(trace).toEqual(["prior"]);
  prior.resolve(null); await turn(); expect(trace).toEqual(["prior", "history"]);
  refresh.resolve(); expect(await barrier).toEqual({ status: "completed", value: 42 });
  await queue.idle(); expect(trace).toEqual(["prior", "history", "refresh", "later"]);
});
it.each(["drag", "generation", "pattern"] as const)("refuses a queued barrier after %s changes", async (change) => {
  const queue = setup(); const prior = deferred<NotesAck | null>(); let drag = false;
  queue.run(() => prior.promise);
  const command = vi.fn(async () => 42); const barrier = queue.barrier(command, () => drag);
  if (change === "drag") drag = true;
  if (change === "generation") queue.setGeneration(2);
  if (change === "pattern") queue.setPatternId(9);
  prior.resolve(null); await turn();
  expect(await barrier).toMatchObject({ status: "cancelled" }); expect(command).not.toHaveBeenCalled();
  await queue.idle(); expect(await queue.flush()).toBe(false);
});
it("rejects invocation during an active drag without calling the callback", async () => {
  const queue = setup(); const command = vi.fn(async () => 1);
  expect(await queue.barrier(command, () => true)).toEqual({ status: "cancelled", reason: "blocked" });
  expect(command).not.toHaveBeenCalled();
});
it("prior note rejection cancels queued barriers and leaves flush usable", async () => {
  const queue = setup(); const prior = deferred<NotesAck | null>();
  queue.run(() => prior.promise); const command = vi.fn(async () => 1);
  const barrier = queue.barrier(command, () => false);
  prior.reject(new Error("native refused"));
  expect(await barrier).toEqual({ status: "cancelled", reason: "priorFailure" });
  await queue.idle(); expect(command).not.toHaveBeenCalled();
  expect(await queue.flush()).toBe(false); expect(await queue.flush()).toBe(true);
});
it("callback throws settle a discriminated failure and count against flush", async () => {
  const queue = setup(); const error = new Error("native refused");
  expect(await queue.barrier(async () => { throw error; }, () => false)).toEqual({ status: "cancelled", reason: "failed", error });
  expect(await queue.flush()).toBe(false);
});
it("native refusal is a returned value, never successful settlement", async () => {
  const queue = setup();
  const result = await queue.barrier(async (): Promise<{ status: "applied" | "refused" }> => ({ status: "refused" }), () => false, (value) => value.status === "applied");
  expect(result).toEqual({ status: "completed", value: { status: "refused" } });
  expect(await queue.flush()).toBe(false);
});
it("detach cancels only barriers and retains an ordinary queued edit command", async () => {
  const queue = setup(); const prior = deferred<NotesAck | null>(); const later = vi.fn(async () => null);
  queue.run(() => prior.promise);
  const barrier = queue.barrier(async () => 1, () => false); queue.run(later);
  queue.detachBarriers(); expect(await barrier).toEqual({ status: "cancelled", reason: "detached" });
  prior.resolve(null); await queue.idle(); expect(later).toHaveBeenCalledTimes(1);
});
it("replacement cancels inflight promise immediately and a late answer cannot free a new epoch", async () => {
  const queue = setup(); const old = deferred<number>(); const next = deferred<NotesAck | null>();
  let valid: () => boolean = () => true;
  const barrier = queue.barrier((still) => { valid = still; return old.promise; }, () => false);
  queue.replacing(); expect(await barrier).toEqual({ status: "cancelled", reason: "stale" }); expect(valid()).toBe(false);
  queue.setGeneration(2); queue.reopened(); await turn(); queue.run(() => next.promise);
  const later = vi.fn(async () => null); queue.run(later);
  old.resolve(1); await turn(); expect(later).not.toHaveBeenCalled(); expect(queue.unsettled()).toBe(true);
  next.resolve(null); await queue.idle(); expect(later).toHaveBeenCalledTimes(1);
});
it("barrier callback does not require flushing or settling its own queue", async () => {
  const queue = setup();
  expect(await queue.barrier(async (still) => still() ? 3 : 0, () => false)).toEqual({ status: "completed", value: 3 });
  await queue.idle();
});

it("native invalidation waits for barrier exit and never overwrites current store revision", async () => {
  const store = new NoteStore(); store.load(1, []);
  let fetches = 0;
  const queue = new CommitQueue(store, { notesEdit: async () => { throw new Error("unused"); },
    patternNotes: async () => { fetches++; return { revision: 2, notes: [] }; } });
  const refresh = deferred<void>();
  const barrier = queue.barrier(async () => {
    queue.invalidateHistory({ generation: 0, patternId: 0, revision: 2 });
    expect(fetches).toBe(0); expect(store.revision).toBe(1);
    await refresh.promise; return true;
  }, () => false);
  expect(fetches).toBe(0); refresh.resolve(); await barrier; await queue.idle();
  expect(fetches).toBe(1); expect(store.revision).toBe(2);
});
it.each([{ generation: 1, patternId: 0, revision: 5 }, { generation: 0, patternId: 9, revision: 5 }])("mismatched native target %j cannot refresh current notes", async (stamp) => {
  const store = new NoteStore(); store.load(1, []); const fetch = vi.fn(async () => ({ revision: 1, notes: [] }));
  const queue = new CommitQueue(store, { notesEdit: async () => { throw new Error("unused"); }, patternNotes: fetch });
  queue.invalidateHistory(stamp); await queue.idle();
  expect(fetch).not.toHaveBeenCalled(); expect(store.revision).toBe(1);
});
it("same-revision native invalidation still fetches actual notes after barrier exit", async () => {
  const store = new NoteStore(); store.load(1, []); const fetch = vi.fn(async () => ({ revision: 1, notes: [] }));
  const queue = new CommitQueue(store, { notesEdit: async () => { throw new Error("unused"); }, patternNotes: fetch });
  await queue.barrier(async () => { queue.invalidateHistory({ generation: 0, patternId: 0, revision: 1 }); return true; }, () => false);
  await queue.idle(); expect(fetch).toHaveBeenCalledTimes(1);
});
it("a deferred old-epoch invalidation cannot announce its revision in the replacement", async () => {
  const store = new NoteStore(); store.load(1, []);
  const fetch = vi.fn(async () => ({ revision: 1, notes: [] }));
  const queue = new CommitQueue(store, { notesEdit: async () => { throw new Error("unused"); }, patternNotes: fetch });
  const held = deferred<void>();
  const barrier = queue.barrier(async () => {
    queue.invalidateHistory({ generation: 0, patternId: 0, revision: 99 }); await held.promise;
  }, () => false);
  queue.replacing(); queue.setGeneration(1); queue.reopened(); held.resolve();
  expect(await barrier).toMatchObject({ status: "cancelled" });
  await queue.idle(); expect(store.revision).toBe(1); expect(fetch).toHaveBeenCalledTimes(1);
});
it("later optimistic edit keeps its old stamp and is honestly refused after native history", async () => {
  const store = new NoteStore(); store.load(1, []); const sent: number[] = [];
  const queue = new CommitQueue(store, {
    notesEdit: async (edit) => { sent.push(edit.baseRevision); throw new Error("stale notes revision"); },
    patternNotes: async () => ({ revision: 2, notes: [] }),
  });
  const held = deferred<void>();
  const barrier = queue.barrier(async () => {
    queue.invalidateHistory({ generation: 0, patternId: 0, revision: 2 }); await held.promise;
  }, () => false);
  queue.commit({ add: [], update: [], delete: [] });
  held.resolve(); await barrier; await queue.idle();
  expect(sent).toEqual([1]); expect(await queue.flush()).toBe(false); expect(store.revision).toBe(2);
});
