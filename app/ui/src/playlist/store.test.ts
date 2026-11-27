import { afterEach, describe, expect, it } from "vitest";
import type { ArrangementOperation, ArrangementPage, ArrangementReply, ArrangementStamp } from "../api/arrangement";
import type { ProjectView } from "../api/project";
import type { PatternsView } from "../api/patterns";
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
  let projectOrigin = origin;
  let readQueue: PlaylistAdapter["enqueueRead"] = command => queue.enqueue(command, false);
  let seekNative: PlaylistAdapter["seek"] = async () => { effects.push("native-seek"); return { status: "applied", generation: 2, revision: 9, changed: false, request: null, createdIds: [], reload: false, message: null, warnings: [] }; };
  let current = true, blocked = false, seekBlocked = false, snapshot = page();
  const views: PlaylistState[] = [];
  let invoke: (ops: readonly ArrangementOperation[], stamp: ArrangementStamp) => Promise<ArrangementReply> = async () => {
    effects.push("native-edit"); snapshot = { ...snapshot, revision: 10 };
    return { status: "applied", generation: 2, revision: 10, request: null, createdIds: [20], changed: true, reload: false, message: null, warnings: [] };
  };
  let audioImport: PlaylistAdapter["importAudio"] = async () => { throw new Error("Unexpected import"); };
  let cancelAudio: PlaylistAdapter["cancelAudio"] = async () => { throw new Error("Unexpected cancel"); };
  let read = async (): Promise<ArrangementPage> => { effects.push("read-arrangement"); return snapshot; };
  let sources = async () => ({ generation: snapshot.generation, arrangementRevision: snapshot.revision,
    patterns: [{ id: 7, name: "Clip source", revision: 1, steps: 16 }, { id: 8, name: "Other pattern", revision: 1, steps: 4 }] });
  let selected = 8;
  let select: PlaylistAdapter["selectPattern"] = async (_base, pattern) => {
    selected = pattern;
    snapshot = { ...snapshot, revision: snapshot.revision + 1 };
    return { status: "applied", state: await bank(), settlement: { recordingDisarmed: false, waiting: false } };
  };
  const project = () => ({ generation: projectOrigin.generation, pattern: { id: selected } }) as ProjectView;
  const bank = async (): Promise<PatternsView> => ({ ...await sources(), revision: snapshot.revision, selected, view: project() });
  const baseAdapter: Omit<PlaylistAdapter, "readPatterns"> = {
    origin: () => ({ ...projectOrigin }), current: at => current && at.generation === projectOrigin.generation && at.replacements === projectOrigin.replacements,
    blocked: () => blocked, seekBlocked: () => seekBlocked, beginTransition: beginProjectTransition,
    enqueueRead: command => readQueue(command), enqueueMutation: command => queue.enqueue(command, true),
    seek: (stamp, tick) => seekNative(stamp, tick),
    readPage: () => read(), edit: (stamp, ops) => invoke(ops, stamp),
    selectPattern: (base, pattern) => select(base, pattern),
    importAudio: (stamp, input, ticket) => audioImport(stamp, input, ticket), relinkAudio: async () => { throw new Error("Unexpected relink"); },
    cancelAudio: (ticket: number) => cancelAudio(ticket),
    readProject: async () => { effects.push("read-project"); return project(); },
    publishProject: (_view, at) => { if (!current || at.generation !== projectOrigin.generation) return false; effects.push("publish-project"); return true; },
    changed: state => { effects.push("publish-controller"); views.push(state); },
  };
  const adapter = { ...baseAdapter, readPatterns: bank };
  const controller = new PlaylistController(adapter);
  cleanups.push(registerSettlement(() => queue.flush()), registerSettlement(controller.settle));
  return { controller, effects, views, audioImport: (fn: typeof audioImport) => { audioImport = fn; }, readQueue: (fn: typeof readQueue) => { readQueue = fn; }, seekNative: (fn: typeof seekNative) => { seekNative = fn; }, current: (v: boolean) => { current = v; }, blocked: (v: boolean) => { blocked = v; },
    cancelAudio: (fn: typeof cancelAudio) => { cancelAudio = fn; },
    seekBlocked: (v: boolean) => { seekBlocked = v; },
    invoke: (fn: typeof invoke) => { invoke = fn; }, read: (fn: typeof read) => { read = fn; },
    sources: (fn: typeof sources) => { sources = fn; }, replaceSnapshot: (v: ArrangementPage) => { snapshot = v; },
    select: (fn: typeof select) => { select = fn; }, selectNormally: select,
    replaceProject: (next: ArrangementPage) => {
      controller.retire();
      projectOrigin = { generation: next.generation, replacements: projectOrigin.replacements + 1 };
      snapshot = next;
      controller.replaceMedia();
    } };
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

  it("observes the settled revision after cancellation before allowing a fresh Apply and Save", async () => {
    const r = rig();
    await r.controller.reload();
    const settling = deferred<void>();
    const settled = deferred<boolean>();
    const unregister = registerSettlement(() => { settling.resolve(); return settled.promise; });
    cleanups.push(unregister);
    const settings: readonly ArrangementOperation[] = [{
      kind: "setAutomationClip", clip: 12, target: { type: "channelPan", channel: 1 }, priority: 8,
    }];
    const edits: ArrangementStamp[] = [];
    r.invoke(async (_ops, stamp) => {
      edits.push(stamp);
      r.replaceSnapshot({ ...page(), revision: 11 });
      return { status: "applied", generation: 2, revision: 11, changed: true, request: null, createdIds: [], reload: false, message: null, warnings: [] };
    });
    const pending = r.controller.mutate({ generation: 2, revision: 9 }, settings, () => false);
    await settling.promise;
    r.replaceSnapshot({ ...page(), revision: 10 });
    const observedDuringSettlement = await r.controller.reload();
    settled.resolve(true);
    expect(observedDuringSettlement).toBe(false);
    expect(await pending).toEqual({ status: "cancelled" });
    expect(edits).toEqual([]);
    expect(r.controller.state.snapshot?.revision).toBe(10);
    expect((await r.controller.mutate({ generation: 2, revision: 10 }, settings)).status).toBe("applied");
    expect(edits).toEqual([{ generation: 2, revision: 10 }]);
    expect(await settleProjectEdits()).toBe(true);
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

describe("Make unique through the playlist publication ticket", () => {
  const makeUnique = [{ kind: "makeUnique", clip: 12 }] as const;
  const at = { generation: 2, revision: 9 };
  async function clonedRig(retarget = 20) {
    const r = rig(); r.replaceSnapshot(page(0, 1, [12]));
    expect(await r.controller.reload()).toBe(true);
    r.effects.length = 0;
    let revision = 10;
    r.invoke(async ops => {
      expect(ops).toEqual(makeUnique);
      r.effects.push("native-clone");
      const response = page(0, 1, [12]);
      const [clip] = response.clips;
      response.clips = [{ ...clip, content: { type: "pattern", pattern: retarget, offsetTick: 0, lengthTicks: 240 } }];
      r.replaceSnapshot({ ...response, revision });
      r.sources(async () => ({ generation: 2, arrangementRevision: revision,
        patterns: [{ id: 7, name: "Clip source", revision: 1, steps: 16 }, { id: 8, name: "Other", revision: 1, steps: 4 },
          { id: 20, name: "Clip source", revision: 1, steps: 16 }] }));
      return { status: "applied", generation: 2, revision, changed: true, createdIds: [20], reload: false,
        message: null, request: null, warnings: [] };
    });
    return { ...r, selected: (base: PatternsView, target: number) => { revision++; return r.selectNormally(base, target); } };
  }

  it("keeps Save behind the held clone selection and publishes only the completed edit", async () => {
    const r = await clonedRig();
    const started = deferred<void>(), release = deferred<void>();
    r.select(async (base, target) => {
      expect(target).toBe(20); expect(base.arrangementRevision).toBe(10);
      r.effects.push("selection-started"); started.resolve(); await release.promise;
      r.effects.push("selection-applied"); return r.selected(base, target);
    });
    const work = r.controller.mutate(at, makeUnique);
    expect(await Promise.race([started.promise.then(() => "select"), work.then(() => "published-before-select")])).toBe("select");
    const finish = beginProjectTransition(); cleanups.push(finish);
    const saved = settleProjectEdits().then(ok => { r.effects.push("save"); return ok; });
    expect(r.controller.unsettled()).toBe(true);
    expect(r.effects).not.toContain("publish-project"); expect(r.effects).not.toContain("save");
    release.resolve(); expect((await work).status).toBe("applied"); expect(await saved).toBe(true);
    expect(r.effects.filter(e => ["native-clone", "selection-started", "selection-applied", "publish-project", "save"].includes(e)))
      .toEqual(["native-clone", "selection-started", "selection-applied", "publish-project", "save"]);
    expect(r.controller.state.snapshot?.revision).toBe(11);
  });

  it.each(["without", "with"] as const)("keeps the accepted clone recoverable %s an earlier Save attempt", async waiting => {
    const r = await clonedRig();
    r.select(async base => ({ status: "refused", state: base, reason: "engineBusy", message: "selection refused",
      settlement: { recordingDisarmed: false, waiting: false } }));
    expect(await r.controller.mutate(at, makeUnique)).toEqual({ status: "unpublished", native: "applied", message: "selection refused" });
    expect(r.controller.state.requiresRefresh).toBe(true);
    if (waiting === "with") expect(await settleProjectEdits()).toBe(false);
    expect((await r.controller.mutate(at, makeUnique)).status).toBe("blocked");
    expect(r.effects.filter(e => e === "native-clone")).toHaveLength(1);
    expect(await r.controller.reload()).toBe(true);
    expect(r.controller.state.snapshot?.clips[0].content).toMatchObject({ pattern: 20 });
    expect(r.controller.state.requiresRefresh).toBe(false);
    expect(await settleProjectEdits()).toBe(true);
  });

  it("never selects an acknowledgement ID absent from the captured clip's new source relation", async () => {
    const r = await clonedRig(7);
    r.select(async () => { r.effects.push("wrong-selection"); throw new Error("unexpected selection"); });
    expect((await r.controller.mutate(at, makeUnique)).status).toBe("unpublished");
    expect(r.effects).not.toContain("wrong-selection"); expect(r.effects).not.toContain("publish-project");
    expect(r.controller.state.requiresRefresh).toBe(true);
  });

  it("retires a held source-selection reply when the project is replaced", async () => {
    const r = await clonedRig();
    const started = deferred<void>(), selection = deferred<Awaited<ReturnType<PlaylistAdapter["selectPattern"]>>>();
    r.select(async () => { started.resolve(); return selection.promise; });
    const work = r.controller.mutate(at, makeUnique);
    expect(await Promise.race([started.promise.then(() => "select"), work.then(() => "finished")])).toBe("select");
    r.current(false); r.controller.retire();
    selection.reject(new Error("old selection reply"));
    expect((await work).status).toBe("stale");
    expect(r.controller.state.snapshot).toBeNull();
    expect(r.controller.state.requiresRefresh).toBe(false);
    expect(r.effects).not.toContain("publish-project");
  });
});

it("continues arranging reads while a modal blocks seek", async () => {
  const r = rig();
  r.seekBlocked(true);
  expect(await r.controller.reload()).toBe(true);
  expect(r.effects).toContain("read-arrangement");
  await r.controller.seek(page(), 960);
  expect(r.effects).not.toContain("native-seek");
});

it("seeks without mutation, history, project reads or unsaved settlement", async () => {
  const r = rig(); await r.controller.reload(); r.effects.length = 0;
  await r.controller.seek(page(), 960);
  expect(r.effects).toEqual(["native-seek", "publish-controller"]);
  expect(r.controller.unsettled()).toBe(false);
  expect(await r.controller.settle()).toBe(true);
  r.blocked(true); await r.controller.seek(page(), 960);
  expect(r.effects).toEqual(["native-seek", "publish-controller"]);
  r.blocked(false); r.current(false); await r.controller.seek(page(), 960);
  expect(r.effects).toEqual(["native-seek", "publish-controller"]);
});

for (const stale of ["replacement", "retirement", "modal"] as const) it(`drops queued seek after ${stale}`, async () => {
  const r = rig(); await r.controller.reload();
  const gate = deferred<void>();
  r.readQueue(async command => { await gate.promise; return command(); });
  const pending = r.controller.seek(page(), 960);
  if (stale === "replacement") r.current(false);
  else if (stale === "modal") r.seekBlocked(true);
  else r.controller.retire();
  gate.resolve(); await pending;
  expect(r.effects).not.toContain("native-seek");
});
it("reports native refusal and failed IPC without unsaved work", async () => {
  const r = rig(); await r.controller.reload();
  r.seekNative(async () => ({ status: "refused", generation: 2, revision: 9, changed: false, request: null, createdIds: [], reload: true, message: "Pattern mode", warnings: [] }));
  await r.controller.seek(page(), 960);
  expect(r.controller.state.message).toBe("Pattern mode");
  expect(r.controller.unsettled()).toBe(false);
  r.seekNative(async () => { throw new Error("queue full"); });
  await r.controller.seek(page(), 960);
  expect(r.controller.state.message).toBe("queue full");
  expect(r.controller.unsettled()).toBe(false);
});

it("refuses a stale revision before admitting a seek", async () => {
  const r = rig(); await r.controller.reload();
  await r.controller.seek({ generation: 2, revision: 8 }, 960);
  expect(r.effects).not.toContain("native-seek");
});

describe("playlist audio admission and publication", () => {
  const input = { track: 11, startTick: 960, name: "Break", path: "/qa/Break.wav" };
  const applied: ArrangementReply = { status: "applied", generation: 2, revision: 10,
    request: 1, createdIds: [20, 21], changed: true, reload: false, message: null, warnings: [] };
  const cancelled: ArrangementReply = { ...applied, status: "refused", revision: 9, changed: false,
    createdIds: [], message: "Media preparation failed: Cancelled", reload: true };

  it.each([true, false])("keeps the import pending until its own reply when ticket cancellation returns %s", async accepted => {
    const r = rig();
    const decode = deferred<ArrangementReply>();
    const invoked = deferred<number>();
    const calls: number[] = [];
    await r.controller.reload();
    const before = r.controller.state.snapshot!;
    r.audioImport((_stamp, _input, ticket) => { invoked.resolve(ticket); return decode.promise; });
    r.cancelAudio(async ticket => { calls.push(ticket); return accepted; });
    const result = r.controller.importAudio(before, input);
    const ticket = await invoked.promise;
    expect(Number.isSafeInteger(ticket) && ticket > 0).toBe(true);
    await r.controller.cancelMedia();
    expect(calls).toEqual([ticket]);
    expect(r.controller.state).toMatchObject({ snapshot: before, media: "import", cancelRequested: true });
    expect(r.controller.unsettled()).toBe(true);
    decode.resolve(cancelled);
    expect(await result).toEqual({ status: "cancelled", reply: cancelled });
    expect(r.controller.state).toMatchObject({ snapshot: before, media: null, cancelRequested: false,
      cancelledMedia: "import", message: null, requiresRefresh: false });
    expect(r.effects).not.toContain("publish-project");
    expect(await r.controller.settleMedia()).toBe(true);
  });

  it("keeps refresh guidance when a cancelled reply reports a newer revision", async () => {
    const r = rig();
    const decode = deferred<ArrangementReply>();
    const invoked = deferred<void>();
    await r.controller.reload();
    const snapshot = r.controller.state.snapshot!;
    r.audioImport(() => { invoked.resolve(); return decode.promise; });
    r.cancelAudio(async () => true);
    const result = r.controller.importAudio(snapshot, input);
    await invoked.promise;
    await r.controller.cancelMedia();
    const reply = { ...cancelled, revision: snapshot.revision + 1 };
    decode.resolve(reply);
    expect(await result).toEqual({ status: "cancelled", reply });
    expect(r.controller.state).toMatchObject({ snapshot, cancelledMedia: "import", requiresRefresh: true,
      message: expect.stringMatching(/refresh the playlist/i) });
    expect(await r.controller.settleMedia()).toBe(false);
  });

  it.each([
    { cancelRequested: false, message: "Older or cancelled media request" },
    { cancelRequested: true, message: "Decode failed" },
  ])("shows a refusal with reason '$message' when user cancel is $cancelRequested", async ({ cancelRequested, message }) => {
    const r = rig();
    const decode = deferred<ArrangementReply>();
    const invoked = deferred<void>();
    await r.controller.reload();
    r.audioImport(() => { invoked.resolve(); return decode.promise; });
    r.cancelAudio(async () => true);
    const result = r.controller.importAudio(r.controller.state.snapshot!, input);
    await invoked.promise;
    if (cancelRequested) await r.controller.cancelMedia();
    const reply = { ...cancelled, message };
    decode.resolve(reply);
    expect(await result).toEqual({ status: "refused", reply });
    expect(r.controller.state).toMatchObject({ cancelledMedia: null, message, requiresRefresh: false });
  });

  it("allows retrying cancellation after the live cancel command rejects", async () => {
    const r = rig();
    const decode = deferred<ArrangementReply>();
    const invoked = deferred<number>();
    const attempts: number[] = [];
    await r.controller.reload();
    r.audioImport((_stamp, _input, ticket) => { invoked.resolve(ticket); return decode.promise; });
    r.cancelAudio(async ticket => {
      attempts.push(ticket);
      if (attempts.length === 1) throw new Error("Lost connection");
      return true;
    });
    const result = r.controller.importAudio(r.controller.state.snapshot!, input);
    const ticket = await invoked.promise;
    await r.controller.cancelMedia();
    expect(r.controller.state).toMatchObject({ media: "import", cancelRequested: false });
    await r.controller.cancelMedia();
    expect(attempts).toEqual([ticket, ticket]);
    decode.resolve(cancelled);
    expect(await result).toEqual({ status: "cancelled", reply: cancelled });
  });

  it.each(["unsettled", "stale", "project replaced"] as const)("handles pre-send cancellation when %s", async status => {
    const r = rig();
    const settlement = deferred<boolean>();
    const settling = deferred<void>();
    let imports = 0;
    await r.controller.reload();
    const snapshot = r.controller.state.snapshot!;
    cleanups.push(registerSettlement(() => { settling.resolve(); return settlement.promise; }));
    r.audioImport(async () => { imports++; return applied; });
    r.cancelAudio(async () => false);
    const result = r.controller.importAudio(snapshot, input);
    await settling.promise;
    await r.controller.cancelMedia();
    if (status === "stale") {
      r.replaceSnapshot({ ...page(), revision: snapshot.revision + 1 });
      await r.controller.reload();
    } else if (status === "project replaced") {
      r.replaceProject({ ...page(), generation: snapshot.generation + 1, revision: 0 });
      expect(await r.controller.reload()).toBe(true);
    }
    settlement.resolve(status !== "unsettled");
    expect(await result).toEqual({ status: "cancelled" });
    expect(imports).toBe(0);
    expect(r.controller.state).toMatchObject({ media: null, cancelledMedia: status === "project replaced" ? null : "import", message: null });
  });

  it("keeps a native-published edit when cancellation loses the race", async () => {
    const r = rig();
    const decode = deferred<ArrangementReply>();
    const invoked = deferred<number>();
    const calls: number[] = [];
    await r.controller.reload();
    r.audioImport((_stamp, _input, ticket) => { invoked.resolve(ticket); return decode.promise; });
    r.cancelAudio(async ticket => { calls.push(ticket); return false; });
    const result = r.controller.importAudio(r.controller.state.snapshot!, input);
    const ticket = await invoked.promise;
    r.replaceSnapshot({ ...page(), revision: 10 });
    await r.controller.cancelMedia();
    decode.resolve(applied);
    expect(await result).toEqual({ status: "applied", reply: applied });
    const after = r.controller.state;
    await r.controller.cancelMedia();
    expect(calls).toEqual([ticket]);
    expect(r.controller.state).toBe(after);
    expect(after).toMatchObject({ snapshot: { revision: 10 }, media: null, cancelledMedia: null, message: null });
    expect(r.effects.filter(effect => effect === "publish-project")).toHaveLength(1);
  });

  it("sends one cancel for two requests, even before the import command starts", async () => {
    const r = rig();
    const decode = deferred<ArrangementReply>();
    const invoked = deferred<number>();
    const ack = deferred<boolean>();
    const commands: [string, number][] = [];
    await r.controller.reload();
    r.audioImport((_stamp, _input, ticket) => { commands.push(["import", ticket]); invoked.resolve(ticket); return decode.promise; });
    r.cancelAudio(ticket => { commands.push(["cancel", ticket]); return ack.promise; });
    const result = r.controller.importAudio(r.controller.state.snapshot!, input);
    const first = r.controller.cancelMedia();
    const second = r.controller.cancelMedia();
    const ticket = await invoked.promise;
    expect(commands).toEqual([["cancel", ticket], ["import", ticket]]);
    ack.resolve(false);
    await Promise.all([first, second]);
    await r.controller.cancelMedia();
    expect(commands).toEqual([["cancel", ticket], ["import", ticket]]);
    decode.resolve({ ...cancelled, message: "Media ticket cancelled" });
    expect((await result).status).toBe("cancelled");
  });

  it("ignores a retired import and a late cancel rejection while the next project is importing", async () => {
    const r = rig();
    const oldReply = deferred<ArrangementReply>();
    const oldInvoked = deferred<void>();
    const oldCancel = deferred<boolean>();
    const nextReply = deferred<ArrangementReply>();
    const nextCancel = deferred<boolean>();
    const cancelCalls: number[] = [];
    await r.controller.reload();
    r.audioImport(() => { oldInvoked.resolve(); return oldReply.promise; });
    r.cancelAudio(ticket => { cancelCalls.push(ticket); return oldCancel.promise; });
    const old = r.controller.importAudio(r.controller.state.snapshot!, input);
    await oldInvoked.promise;
    const cancellingOld = r.controller.cancelMedia();
    r.current(false);
    r.controller.retire();
    r.controller.replaceMedia();
    expect(await r.controller.settleMedia()).toBe(true);
    r.current(true);
    await r.controller.reload();
    r.audioImport(() => nextReply.promise);
    r.cancelAudio(ticket => { cancelCalls.push(ticket); return nextCancel.promise; });
    const next = r.controller.importAudio(r.controller.state.snapshot!, input);
    const cancellingNext = r.controller.cancelMedia();
    const state = r.controller.state;
    oldReply.resolve(cancelled);
    oldCancel.reject(new Error("Lost connection"));
    expect(await old).toEqual({ status: "stale" });
    await cancellingOld;
    expect(r.controller.state).toBe(state);
    expect(cancelCalls).toHaveLength(2);
    expect(cancelCalls[0]).not.toBe(cancelCalls[1]);
    nextCancel.resolve(true);
    nextReply.resolve(cancelled);
    await cancellingNext;
    expect((await next).status).toBe("cancelled");
  });

  it("admits only one decode, keeps Save waiting and publishes the accepted project", async () => {
    const r = rig();
    const decode = deferred<ArrangementReply>();
    await r.controller.reload();
    r.audioImport(() => { r.effects.push("native-import"); return decode.promise; });
    const first = r.controller.importAudio(r.controller.state.snapshot!, input);
    expect(r.controller.state.media).toBe("import");
    const duplicate = r.controller.importAudio(r.controller.state.snapshot!, input);
    expect(await Promise.race([duplicate, Promise.resolve("pending")])).toEqual({ status: "blocked" });
    const settled = r.controller.settleMedia();
    await Promise.resolve();
    expect(await Promise.race([settled, Promise.resolve("pending")])).toBe("pending");
    r.replaceSnapshot({ ...page(), revision: 10 });
    decode.resolve(applied);
    expect(await first).toEqual({ status: "applied", reply: applied });
    expect(await settled).toBe(true);
    expect(r.effects.filter(effect => effect === "native-import" || effect === "publish-project")).toEqual(["native-import", "publish-project"]);
    expect(r.controller.state.media).toBeNull();
  });

  it("keeps the previous snapshot and settles after a refused decode", async () => {
    const r = rig();
    await r.controller.reload();
    const snapshot = r.controller.state.snapshot!;
    r.audioImport(async () => ({ ...applied, status: "refused", revision: 9, createdIds: [], changed: false,
      reload: true, message: "Decode failed" }));
    expect((await r.controller.importAudio(snapshot, input)).status).toBe("refused");
    expect(r.controller.state).toMatchObject({ snapshot, media: null, requiresRefresh: false, message: "Decode failed" });
    expect(r.effects).not.toContain("publish-project");
    expect(await r.controller.settleMedia()).toBe(true);
  });

  it("discards an old acknowledgement after replacement without delaying the new project", async () => {
    const r = rig();
    const decode = deferred<ArrangementReply>();
    const invoked = deferred<void>();
    await r.controller.reload();
    r.audioImport(() => { invoked.resolve(); return decode.promise; });
    const first = r.controller.importAudio(r.controller.state.snapshot!, input);
    await invoked.promise;
    r.current(false);
    r.controller.retire();
    r.controller.retireMedia();
    expect(await r.controller.settleMedia()).toBe(true);
    decode.resolve(applied);
    expect(await first).toEqual({ status: "stale" });
    expect(r.controller.state.snapshot).toBeNull();
    expect(r.effects).not.toContain("publish-project");
  });
});
