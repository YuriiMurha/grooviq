import { afterEach, describe, expect, it } from "vitest";
import type { ArrangementOperation, ArrangementPage, ArrangementReply } from "../api/arrangement";
import type { ProjectView } from "../api/project";
import { beginProjectTransition, ProjectCommandQueue, registerSettlement, settleProjectEdits } from "../project/pending";
import { PlaylistController, readArrangement, type PlaylistAdapter, type PlaylistState } from "./store";

const origin = { generation: 2, replacements: 4 };
const operations: readonly ArrangementOperation[] = [{ kind: "createTrack", name: "Track 1", color: 0 }];
function page(offset = 0, total = 0, ids: number[] = []): ArrangementPage {
  return { generation: 2, revision: 9, offset, total, clips: ids.map(id => ({ id, track: 11, name: `Clip ${id}`, startTick: id * 240, muted: false, content: { type: "pattern", pattern: 7, offsetTick: 0, lengthTicks: 240 } })),
    tracks: [{ id: 11, name: "Track 1", color: 0, mute: false }], audioAssets: [], loopRegion: null,
    playMode: "pattern", songError: null, pendingRequests: [], warnings: [] };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const cleanups: (() => void)[] = [];
afterEach(() => { for (const cleanup of cleanups.splice(0)) cleanup(); });

describe("coherent native arrangement pages", () => {
  it("collects every bounded page under one stamp and preserves non-pattern records", async () => {
    const called: number[] = [];
    const snapshot = await readArrangement(async offset => {
      called.push(offset);
      const result = page(offset, 3, offset === 0 ? [12, 13] : [14]);
      if (offset === 2) result.clips = [{ ...result.clips[0], content: { type: "automation", target: { type: "channelPan", channel: 1 }, priority: 20, offsetTick: 0, lengthTicks: 240, pointCount: 2 } }];
      return result;
    }, () => true, 2);
    expect(called).toEqual([0, 2]);
    expect(snapshot.clips.map(clip => clip.id)).toEqual([12, 13, 14]);
    expect(snapshot.clips[2].content.type).toBe("automation");
    expect(snapshot.total).toBe(3);
  });

  it.each(["generation", "revision", "total", "offset", "duplicate", "short", "track-header"] as const)("refuses %s drift instead of publishing a partial list", async kind => {
    await expect(readArrangement(async offset => {
      let result = page(offset, 3, offset === 0 ? [12, 13] : [14]);
      if (offset > 0) {
        if (kind === "generation") result = { ...result, generation: result.generation + 1 };
        if (kind === "revision") result = { ...result, revision: result.revision + 1 };
        if (kind === "total") result.total++;
        if (kind === "offset") result.offset = 0;
        if (kind === "duplicate") result.clips[0].id = 12;
        if (kind === "short") result.clips = [];
        if (kind === "track-header") result.tracks = [{ ...result.tracks[0], name: "Different" }];
      }
      return result;
    }, () => true, 2)).rejects.toThrow();
  });

  it("rejects unsafe totals and replacement during reads before a later request", async () => {
    let calls = 0;
    await expect(readArrangement(async () => { calls++; return page(0, 100_001); }, () => true)).rejects.toThrow();
    expect(calls).toBe(1);
    let current = true;
    await expect(readArrangement(async offset => { current = false; return page(offset, 3, [12, 13]); }, () => current, 2)).rejects.toThrow("project");
    expect(calls).toBe(1);
  });
});

function rig() {
  const queue = new ProjectCommandQueue();
  const effects: string[] = [];
  let current = true, blocked = false, snapshot = page();
  const views: PlaylistState[] = [];
  let invoke: (ops: readonly ArrangementOperation[]) => Promise<ArrangementReply> = async () => {
    effects.push("native-edit"); snapshot = { ...snapshot, revision: 10 };
    return { status: "applied", generation: 2, revision: 10, request: null, createdIds: [20], changed: true, reload: false, message: null, warnings: [] };
  };
  let read = async (): Promise<ArrangementPage> => { effects.push("read-arrangement"); return snapshot; };
  let sources = async () => ({ generation: snapshot.generation, arrangementRevision: snapshot.revision,
    patterns: [{ id: 7, name: "Clip source", revision: 1, steps: 16 }, { id: 8, name: "Other pattern", revision: 1, steps: 4 }] });
  const baseAdapter: Omit<PlaylistAdapter, "readPatterns"> = {
    origin: () => ({ ...origin }), current: at => current && at.generation === origin.generation && at.replacements === origin.replacements,
    blocked: () => blocked, beginTransition: beginProjectTransition,
    enqueueRead: command => queue.enqueue(command, false), enqueueMutation: command => queue.enqueue(command, true),
    readPage: () => read(), edit: (_stamp, ops) => invoke(ops),
    readProject: async () => { effects.push("read-project"); return { generation: 2 } as ProjectView; },
    publishProject: (_view, at) => { if (!current || at.generation !== 2) return false; effects.push("publish-project"); return true; },
    changed: state => { effects.push("publish-controller"); views.push(state); },
  };
  const adapter = { ...baseAdapter, readPatterns: () => sources() };
  const controller = new PlaylistController(adapter);
  cleanups.push(registerSettlement(() => queue.flush()), registerSettlement(controller.settle));
  return { controller, effects, views, current: (v: boolean) => { current = v; }, blocked: (v: boolean) => { blocked = v; },
    invoke: (fn: typeof invoke) => { invoke = fn; }, read: (fn: typeof read) => { read = fn; },
    sources: (fn: typeof sources) => { sources = fn; }, replaceSnapshot: (v: ArrangementPage) => { snapshot = v; } };
}

describe("playlist intent and native publication in the existing Session lane", () => {
  it("captures the actual clip source period rather than an unrelated selected pattern", async () => {
    const r = rig(); r.replaceSnapshot(page(0, 1, [12]));
    expect(await r.controller.reload()).toBe(true);
    expect(r.controller.state.snapshot).toMatchObject({ sourcePeriods: { 7: 3840 } });
    expect(r.effects).not.toContain("native-edit");
  });

  it.each(["generation", "arrangementRevision", "zeroSteps", "missingSource"] as const)("rejects %s source metadata before publishing editable clips", async kind => {
    const r = rig(); r.replaceSnapshot(page(0, 1, [12]));
    r.sources(async () => ({ generation: kind === "generation" ? 3 : 2,
      arrangementRevision: kind === "arrangementRevision" ? 10 : 9,
      patterns: kind === "missingSource" ? [] : [{ id: 7, name: "Clip source", revision: 1, steps: kind === "zeroSteps" ? 0 : 16 }] }));
    expect(await r.controller.reload()).toBe(false);
    expect(r.controller.state.snapshot).toBeNull();
    expect(r.controller.state.requiresRefresh).toBe(false);
    expect((await r.controller.mutate({ generation: 2, revision: 9 }, operations)).status).toBe("stale");
    expect(r.effects).not.toContain("native-edit");
  });

  it("cannot reuse source length read after the arrangement revision changed", async () => {
    const r = rig(); r.replaceSnapshot(page(0, 1, [12]));
    r.sources(async () => {
      r.replaceSnapshot({ ...page(0, 1, [12]), revision: 10 });
      return { generation: 2, arrangementRevision: 10, patterns: [{ id: 7, name: "Clip source", revision: 2, steps: 8 }] };
    });
    expect(await r.controller.reload()).toBe(false);
    expect(r.controller.state.snapshot).toBeNull();
    expect(r.effects).not.toContain("publish-project");
  });

  it("owns unsaved intent before awaiting and Save waits through canonical publication", async () => {
    const r = rig(); await r.controller.reload();
    r.effects.length = 0;
    const admitted = deferred<ArrangementReply>();
    r.invoke(async () => { r.effects.push("native-held"); return admitted.promise; });
    const command = r.controller.mutate({ generation: 2, revision: 9 }, operations);
    expect(r.controller.unsettled()).toBe(true);
    await Promise.resolve(); await Promise.resolve();
    const finishSave = beginProjectTransition();
    cleanups.push(finishSave);
    let saved = false;
    const saving = settleProjectEdits().then(ok => { expect(ok).toBe(true); r.effects.push("save-snapshot"); saved = true; });
    expect(saved).toBe(false);
    r.replaceSnapshot({ ...page(), revision: 10 });
    admitted.resolve({ status: "applied", generation: 2, revision: 10, changed: true, createdIds: [20], reload: false, message: null, request: null, warnings: [] });
    expect((await command).status).toBe("applied");
    await saving; finishSave();
    expect(r.effects.indexOf("publish-project")).toBeLessThan(r.effects.indexOf("save-snapshot"));
    expect(r.controller.state.snapshot?.revision).toBe(10);
    expect(r.controller.unsettled()).toBe(false);
  });

  it("refuses a second intent during preflight instead of creating a settlement cycle", async () => {
    const r = rig(); await r.controller.reload();
    const held = deferred<ArrangementReply>(); r.invoke(async () => held.promise);
    const first = r.controller.mutate({ generation: 2, revision: 9 }, operations);
    expect((await r.controller.mutate({ generation: 2, revision: 9 }, operations)).status).toBe("blocked");
    held.resolve({ status: "applied", generation: 2, revision: 10, changed: true, createdIds: [20], reload: false, message: null, request: null, warnings: [] });
    r.replaceSnapshot({ ...page(), revision: 10 });
    expect((await first).status).toBe("applied");
  });

  it("keeps native refusal distinct and consumes its failure once at Save settlement", async () => {
    const r = rig(); await r.controller.reload();
    r.invoke(async () => ({ status: "refused", generation: 2, revision: 9, changed: false, createdIds: [], reload: false, message: "Admission refused", request: null, warnings: [] }));
    expect((await r.controller.mutate({ generation: 2, revision: 9 }, operations)).status).toBe("refused");
    expect(r.controller.state.snapshot?.revision).toBe(9);
    expect(await settleProjectEdits()).toBe(false);
    expect(await settleProjectEdits()).toBe(true);
    expect(r.controller.state.message).toBe("Admission refused");
  });

  it("retains an applied-but-unpublished obligation and blocks Save until actual refresh", async () => {
    const r = rig(); await r.controller.reload();
    r.read(async () => { throw new Error("read lost"); });
    const result = await r.controller.mutate({ generation: 2, revision: 9 }, operations);
    expect(result.status).toBe("unpublished");
    if (result.status !== "unpublished") throw new Error("Expected known native acceptance");
    expect(result.native).toBe("applied");
    expect(r.controller.state.requiresRefresh).toBe(true);
    expect(await settleProjectEdits()).toBe(false);
    expect(await settleProjectEdits()).toBe(false);
    const count = r.effects.filter(e => e === "native-edit").length;
    expect((await r.controller.mutate({ generation: 2, revision: 9 }, operations)).status).toBe("blocked");
    expect(r.effects.filter(e => e === "native-edit")).toHaveLength(count);
    r.read(async () => ({ ...page(), revision: 10 }));
    await r.controller.reload();
    expect(r.controller.state.requiresRefresh).toBe(false);
    expect(await settleProjectEdits()).toBe(true);
  });

  it("never publishes a held old-project reply or retargets its captured clip", async () => {
    const r = rig(); await r.controller.reload();
    const started = deferred<void>(), held = deferred<ArrangementReply>();
    let captured: readonly ArrangementOperation[] = [];
    r.invoke(async ops => { captured = ops; started.resolve(); return held.promise; });
    const ops: ArrangementOperation[] = [{ kind: "move", clip: 12, track: 11, startTick: 240 }];
    const pending = r.controller.mutate({ generation: 2, revision: 9 }, ops);
    ops[0] = { kind: "move", clip: 99, track: 77, startTick: 999 };
    await started.promise;
    expect(captured).toEqual([{ kind: "move", clip: 12, track: 11, startTick: 240 }]);
    r.current(false); r.controller.retire();
    const count = r.effects.filter(e => e === "publish-project").length;
    held.resolve({ status: "applied", generation: 2, revision: 10, changed: true, createdIds: [], reload: false, message: null, request: null, warnings: [] });
    expect((await pending).status).toBe("stale");
    expect(r.effects.filter(e => e === "publish-project")).toHaveLength(count);
    expect(r.controller.state.snapshot).toBeNull();
  });

  it("rejects stale captured revisions before native I/O rather than using a newer stamp", async () => {
    const r = rig(); await r.controller.reload();
    expect((await r.controller.mutate({ generation: 2, revision: 8 }, operations)).status).toBe("stale");
    r.blocked(true);
    expect((await r.controller.mutate({ generation: 2, revision: 9 }, operations)).status).toBe("blocked");
    expect(r.effects).not.toContain("native-edit");
  });

  it("keeps unknown invocation outcome protected without claiming native refusal or retrying", async () => {
    const r = rig(); await r.controller.reload();
    r.invoke(async () => { r.effects.push("native-unknown"); throw new Error("IPC disconnected"); });
    const result = await r.controller.mutate({ generation: 2, revision: 9 }, operations);
    expect(result).toEqual({ status: "unpublished", native: "unknown", message: "IPC disconnected" });
    expect(r.controller.state.requiresRefresh).toBe(true);
    expect((await r.controller.mutate({ generation: 2, revision: 9 }, operations)).status).toBe("blocked");
    expect(r.effects.filter(effect => effect === "native-unknown")).toHaveLength(1);
    expect(await settleProjectEdits()).toBe(false);
  });

  it("a failed observation disables stale editing without inventing unsaved native work", async () => {
    const r = rig(); await r.controller.reload();
    r.read(async () => { throw new Error("page unavailable"); });
    expect(await r.controller.reload()).toBe(false);
    expect(r.controller.state.snapshot).toBeNull();
    expect(r.controller.state.requiresRefresh).toBe(false);
    expect((await r.controller.mutate({ generation: 2, revision: 9 }, operations)).status).toBe("stale");
    expect(r.effects).not.toContain("native-edit");
    expect(await settleProjectEdits()).toBe(true);
  });

  it("ordinary observations never re-adopt ProjectView in an observation loop", async () => {
    const r = rig();
    expect(await r.controller.reload()).toBe(true);
    expect(await r.controller.reload()).toBe(true);
    expect(r.effects.filter(effect => effect === "read-arrangement")).toHaveLength(2);
    expect(r.effects).not.toContain("publish-project");
    expect(r.effects).not.toContain("read-project");
  });
});
