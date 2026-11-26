import { describe, expect, it } from "vitest";
import type { ArrangementStamp } from "../api/arrangement";
import type { MixerEdit, MixerEditReply, MixerInsertReply, MixerInsertTarget, MixerSnapshot } from "../api/mixer";
import type { ProjectView } from "../api/project";
import { ProjectCommandQueue } from "../project/pending";
import { bindMixerSnapshot } from "./mixerBinding";
import { createMixerHost, mixerHostControls } from "./mixerHost";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

const project = (generation = 1): ProjectView => ({
  generation, name: "Test", path: null, dirty: false, bpm: 120, timeSignature: { beats: 4, unit: 4 },
  channels: [], pattern: { id: 1, name: "Pattern 1", steps: 16, swing: 0, cells: {} }, autosavedAt: null, key: null,
  notesRevision: 1, recording: false, canUndo: false, canRedo: false,
});

function mixer(revision = 1, generation = 1, slots = [1, 2]): MixerSnapshot {
  const controls = { pan: 0, level: { db: 0 }, mute: false, solo: false };
  return {
    generation, revision, effects: ["eq"], sources: [],
    unsupported: { mute: "Mute", solo: "Solo", bypass: "Bypass", detector: "Detector" },
    mixer: {
      mode: "routed",
      tracks: [
        { ...controls, id: 0, name: "Master", output: null, inserts: [] },
        { ...controls, id: 1, name: "Track", output: 0, inserts: slots.map((id) => ({
          id, device: "eq", params: [], preset: null, seed: "0", bypass: false,
        })) },
      ],
      sends: [], sidechains: [], sources: [], nextTrackId: 2, nextInsertId: 3, nextSendId: 1,
    },
    routeInputs: [{ track: 0, from: [1] }, { track: 1, from: [] }],
  };
}

const applied = (state: MixerSnapshot): MixerEditReply => ({
  status: "applied", state, changed: true, createdIds: [], message: null,
});
const detail = (target: MixerInsertTarget): MixerInsertReply => ({
  status: "applied", detail: { ...target, effectId: "eq", active: true, params: [1], preset: null, factoryPresets: ["Init"] },
});

function setup() {
  const reads: ReturnType<typeof deferred<MixerSnapshot>>[] = [];
  const details: { target: MixerInsertTarget; reply: ReturnType<typeof deferred<MixerInsertReply>> }[] = [];
  const edits: { stamp: ArrangementStamp; edit: MixerEdit; reply: ReturnType<typeof deferred<MixerEditReply>> }[] = [];
  const queue = new ProjectCommandQueue();
  let projectReads = 0;
  let shown = project();
  let replacements = 0;
  let transitioning = false;
  const host = createMixerHost({
    stamp: () => ({ generation: shown.generation, replacements }),
    blocked: () => transitioning,
    replacing: () => false,
    readMixer: () => {
      const request = deferred<MixerSnapshot>();
      reads.push(request);
      return request.promise;
    },
    readInsert: (target) => {
      const reply = deferred<MixerInsertReply>();
      details.push({ target, reply });
      return reply.promise;
    },
    edit: (stamp, edit) => {
      const reply = deferred<MixerEditReply>();
      edits.push({ stamp, edit, reply });
      return reply.promise;
    },
    readProject: async () => {
      projectReads++;
      return { ...shown, dirty: true, projectCanUndo: true };
    },
    runFor: async (at, run) => {
      try {
        shown = await queue.enqueue(() => {
          if (transitioning || at.replacements !== replacements) throw new Error("stale");
          return run(at.generation);
        });
        host.observe(shown);
        return true;
      } catch { return false; }
    },
  });
  host.start();
  host.observe(shown);
  const strip = () => host.current().binding?.props.tracks[0];
  const seed = async () => { reads[0].resolve(mixer()); await tick(); };
  const replace = () => { host.retire(); replacements++; shown = project(2); host.observe(shown); };
  const transition = () => {
    host.invalidateGestures();
    transitioning = true;
    return () => { transitioning = false; };
  };
  const commit = (phase: "live" | "cancel" | "commit" = "commit") => host.command({
    type: "param", phase, value: 0.5, target: { kind: "pan", nodeId: strip()?.id ?? "missing" },
  });
  const open = (slot: number) => {
    const track = strip();
    if (!track) throw new Error("Mixer unavailable");
    return host.command({ type: "openInsert", nodeId: track.id, slotId: track.inserts[slot].id });
  };
  return { host, reads, edits, details, queue, seed, replace, transition, commit, open, strip, projectReads: () => projectReads, shown: () => shown };
}

async function tick() {
  await Promise.resolve();
  await Promise.resolve();
}

describe("mixer host ordering", () => {
  it("rejects a held read after a newer edit and skips its own ProjectView notification", async () => {
    const { host, seed, reads, edits, commit, projectReads, shown } = setup();
    await seed();
    const stale = host.refresh();
    const result = commit();
    await tick();
    edits[0].reply.resolve(applied(mixer(2)));
    expect(await result).toBe(true);
    reads[1].resolve(mixer(1));
    await stale;
    expect(host.current().binding?.stamp.revision).toBe(2);
    expect(host.current().pending).toBe(false);
    expect(projectReads()).toBe(1);
    expect(shown()).toMatchObject({ dirty: true, projectCanUndo: true });
    expect(reads).toHaveLength(2);
  });

  it("retires queued work and late reads across replacement with reused numeric IDs", async () => {
    const { host, reads, edits, queue, seed, replace, commit, strip } = setup();
    await seed();
    const oldId = strip()?.id ?? "missing";
    const holdLane = deferred<void>();
    void queue.enqueue(() => holdLane.promise, false);
    const stale = host.refresh();
    const settled = commit();
    replace();
    reads[2].resolve(mixer(1, 2));
    holdLane.resolve();
    reads[1].resolve(mixer(7));
    await stale;
    expect(await settled).toBe(false);
    expect(edits).toHaveLength(0);
    expect(strip()?.id).not.toBe(oldId);
    expect(host.current().binding?.stamp).toEqual({ generation: 2, revision: 1 });
  });

  it("drops an in-flight edit response when the replacement is already authoritative", async () => {
    const { host, reads, edits, seed, replace, commit } = setup();
    await seed();
    const old = commit();
    await tick();
    replace();
    reads[1].resolve(mixer(1, 2));
    await tick();
    edits[0].reply.resolve(applied(mixer(2)));
    expect(await old).toBe(false);
    expect(host.current().binding?.stamp).toEqual({ generation: 2, revision: 1 });
    expect(host.current().pending).toBe(false);
  });

  it("drops an intent at the front of the shared lane when a rival revision landed", async () => {
    const { host, reads, edits, queue, seed, commit } = setup();
    await seed();
    const holdLane = deferred<void>();
    void queue.enqueue(() => holdLane.promise, false);
    const settled = commit();
    const refresh = host.refresh();
    reads[1].resolve(mixer(2));
    await refresh;
    holdLane.resolve();
    expect(await settled).toBe(false);
    expect(edits).toHaveLength(0);
    expect(host.current().pending).toBe(false);
  });

  it("lets an already-entered edit publish while a Save waits and fences held gestures", async () => {
    const { host, seed, edits, commit, transition } = setup();
    await seed();
    const settled = commit();
    await tick();
    const finish = transition();
    void commit("live");
    edits[0].reply.resolve(applied(mixer(2)));
    expect(await settled).toBe(true);
    expect(host.current().binding?.stamp.revision).toBe(2);
    finish();
    const stale = commit();
    await tick();
    expect(edits).toHaveLength(1);
    await stale;
  });

  it("coalesces external ProjectView notifications into one trailing read", async () => {
    const { host, reads } = setup();
    host.observe({ ...project(), dirty: true });
    host.observe({ ...project(), projectCanUndo: true });
    expect(reads).toHaveLength(1);
    reads[0].resolve(mixer());
    await tick();
    expect(reads).toHaveLength(2);
    expect(host.current().binding).toBeNull();
    reads[1].resolve(mixer(3));
    await tick();
    expect(host.current().binding?.stamp.revision).toBe(3);
    expect(reads).toHaveLength(2);
  });

  it("invalidates held gestures when history publishes a view, before the read returns", async () => {
    const { host, seed, commit, edits } = setup();
    await seed();
    void commit("live");
    host.observe({ ...project(), projectCanRedo: true });
    const stale = commit();
    await tick();
    expect(edits).toHaveLength(0);
    await stale;
  });

  it("surfaces a scoped refusal with its authoritative snapshot and clears pending", async () => {
    const { host, seed, edits, commit, strip, projectReads } = setup();
    await seed();
    const settled = commit();
    await tick();
    expect(host.current().pending).toBe(true);
    edits[0].reply.resolve({ status: "refused", state: mixer(4), changed: false, createdIds: [], message: "Route refused" });
    expect(await settled).toBe(false);
    expect(host.current()).toMatchObject({
      pending: false, error: { nodeId: strip()?.id, message: "Route refused" },
      binding: { stamp: { generation: 1, revision: 4 } },
    });
    expect(projectReads()).toBe(1);
  });

  it("correlates detail with the latest open slot and drops a reply after removal", async () => {
    const { host, seed, details, reads, open } = setup();
    await seed();
    void open(0);
    void open(1);
    details[1].reply.resolve(detail(details[1].target));
    await tick();
    details[0].reply.resolve(detail(details[0].target));
    await tick();
    expect(host.current().insert?.detail?.insert).toBe(2);
    void open(0);
    const refresh = host.refresh();
    reads[1].resolve(mixer(2, 1, [2]));
    await refresh;
    details[2].reply.resolve(detail(details[2].target));
    await tick();
    expect(host.current().insert).toBeNull();
  });

  it("does no native work for live/cancel and drops a held gesture across a revision", async () => {
    const { host, seed, edits, reads, commit } = setup();
    await seed();
    void commit("live");
    void commit("cancel");
    expect(edits).toHaveLength(0);
    void commit("live");
    const refresh = host.refresh();
    reads[1].resolve(mixer(2));
    await refresh;
    void commit("live"); // A continuation of the retired gesture cannot rebase onto revision 2.
    const stale = commit();
    await tick();
    expect(edits).toHaveLength(0);
    await stale;
    const oneShot = commit();
    await tick();
    expect(edits[0].stamp.revision).toBe(2);
    edits[0].reply.resolve(applied(mixer(3)));
    expect(await oneShot).toBe(true);
  });

  it("binds editor actions to the checked owner and stamp before entering the lane", async () => {
    const { host, seed, edits, details, open } = setup();
    await seed();
    void open(0);
    details[0].reply.resolve(detail(details[0].target));
    await tick();
    const owner = host.current().insert;
    if (!owner) throw new Error("Missing checked owner");
    host.closeInsert();
    expect(await host.applyInsert(owner, { kind: "applyPreset", preset: "Init" })).toBe(false);
    void open(1);
    details[1].reply.resolve(detail(details[1].target));
    await tick();
    const current = host.current().insert;
    if (!current) throw new Error("Missing checked owner");
    host.closeInsert(owner);
    expect(host.current().insert).toBe(current);
    const params = [1, 2];
    const result = host.applyInsert(current, { kind: "setInsert", params });
    params[0] = 4;
    await tick();
    expect(edits[0]).toMatchObject({ stamp: { generation: 1, revision: 1 }, edit: { kind: "setInsert", track: 1, insert: 2, params: [1, 2] } });
    edits[0].reply.resolve(applied(mixer(2)));
    expect(await result).toBe(true);
    expect(details[2].target).toMatchObject({ track: 1, insert: 2, revision: 2 });
  });

  it("does not publish an in-flight edit to an unmounted host", async () => {
    const { host, edits, seed, commit } = setup();
    await seed();
    const finished = commit();
    await tick();
    let published = 0;
    const unsubscribe = host.subscribe(() => { published++; });
    host.stop();
    edits[0].reply.resolve(applied(mixer(2)));
    expect(await finished).toBe(false);
    expect(published).toBe(0);
    unsubscribe();
  });

  it("keeps route traversal lazy when locking strips for a pending edit", () => {
    let reads = 0;
    const snapshot = mixer();
    const binding = bindMixerSnapshot({ ...snapshot, routeInputs: [
      { track: 0, from: [1] },
      { track: 1, get from() { reads++; return []; } },
    ] });
    const pending = mixerHostControls(binding, true);
    expect(pending.tracks[0].pending).toBe(true);
    expect(reads).toBe(0);
    expect(pending.tracks[0].outputs).toEqual([{ id: pending.master.id, name: "Master" }]);
    expect(reads).toBe(1);
  });
});
