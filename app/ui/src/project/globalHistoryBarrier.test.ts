import type { ProjectView } from "../api/project";
import type { PatternsView } from "../api/patterns";
import { runHistoryLane, type HistoryResult } from "./globalHistory";
import { ProjectCommandQueue } from "./pending";
import { expect, it, vi } from "vitest";
import { CommitQueue } from "../roll/commitQueue";
import { NoteStore } from "../roll/noteStore";
import { setRollHistory, trackDrags, withRollHistoryBarrier } from "./noteHistory";

it("uses the registered barrier with original typed target and leaves legacy registration intact", async () => {
  const store = new NoteStore(); store.load(1, []);
  const queue = new CommitQueue(store, { notesEdit: async () => { throw new Error("unused"); }, patternNotes: async () => ({ revision: 1, notes: [] }) });
  const dispose = setRollHistory({ run: (command) => queue.run(command), barrier: (command, drag, accepted) => queue.barrier(command, drag, accepted), detachBarriers: () => queue.detachBarriers() });
  try {
    expect(await withRollHistoryBarrier(async () => ({ kind: "samplerTrim", channel: 42 }), () => true)).toEqual({ status: "completed", value: { kind: "samplerTrim", channel: 42 } });
  } finally { dispose(); }
});
it("a stale origin and an active drag cannot enter native command", async () => {
  const command = vi.fn(async () => 1);
  expect(await withRollHistoryBarrier(command, () => false)).toEqual({ status: "cancelled", reason: "stale" });
  const target = new EventTarget(); const dispose = trackDrags(target as unknown as Window);
  try {
    target.dispatchEvent(Object.assign(new Event("pointerdown"), { pointerId: 7 }));
    expect(await withRollHistoryBarrier(command, () => true)).toEqual({ status: "cancelled", reason: "blocked" });
    expect(command).not.toHaveBeenCalled();
  } finally { dispose(); }
});

const nativeView = (): ProjectView => ({ name: "History", path: null, dirty: true, bpm: 120,
  timeSignature: { beats: 4, unit: 4 }, channels: [], pattern: { id: 1, name: "A", steps: 16, swing: 0, cells: {} },
  autosavedAt: null, key: null, notesRevision: 8, recording: false, canUndo: true, canRedo: false, generation: 7, patternRevision: 1 });
function historyLane() {
  const queue = new ProjectCommandQueue(); const view = nativeView();
  const invalidate = vi.fn(); const adopt = vi.fn();
  const readPatterns = vi.fn(async (): Promise<PatternsView> => ({ generation: 7, revision: 1, selected: 1,
    patterns: [{ id: 1, name: "A", revision: 8 }, { id: 2, name: "B", revision: 42 }], view }));
  const adapter = { enqueueMutation: (command: () => Promise<HistoryResult>) => queue.enqueue(command),
    current: () => true, readView: async () => view, readPatterns, adopt, invalidate };
  return { queue, view, adapter, adopt, invalidate, readPatterns };
}
it("publishes native view then invalidates only the exact selected native notes revision", async () => {
  const { adapter, adopt, invalidate, view, readPatterns } = historyLane();
  const command = vi.fn(async (): Promise<HistoryResult> => ({ status: "applied", generation: 7, revision: 12, effect: { kind: "notes", pattern: 1 } }));
  await runHistoryLane({ generation: 7, replacements: 0 }, command, adapter);
  expect(command).toHaveBeenCalledTimes(1); expect(adopt).toHaveBeenCalledWith(view);
  expect(invalidate).toHaveBeenCalledWith({ generation: 7, patternId: 1, revision: 8 });
  expect(readPatterns).not.toHaveBeenCalled();
  expect(adopt.mock.invocationCallOrder[0]).toBeLessThan(invalidate.mock.invocationCallOrder[0]);
});
it("nonselected effect uses its own native summary, never selection or current revision", async () => {
  const { adapter, invalidate, view } = historyLane();
  await runHistoryLane({ generation: 7, replacements: 0 }, async () => ({ status: "applied", generation: 7, revision: 12, effect: { kind: "notes", pattern: 2 } }), adapter);
  expect(invalidate).toHaveBeenCalledWith({ generation: 7, patternId: 2, revision: 42 });
  expect(view.pattern.id).toBe(1); expect(view.notesRevision).toBe(8);
});
it("native refusal rejects inside the actual mutation queue, preserves reply and fails settlement", async () => {
  const { adapter, queue, invalidate } = historyLane();
  const reply: HistoryResult = { status: "refused", generation: 7, revision: 12, message: "Busy", reload: true };
  expect(await runHistoryLane({ generation: 7, replacements: 0 }, async () => reply, adapter)).toBe(reply);
  expect(invalidate).not.toHaveBeenCalled(); expect(await queue.flush()).toBe(false);
  expect(await queue.flush()).toBe(true);
});
it("empty history adopts canonical native view without manufacturing a roll refresh", async () => {
  const { adapter, queue, adopt, invalidate } = historyLane();
  await runHistoryLane({ generation: 7, replacements: 0 }, async () => ({ status: "applied", generation: 7, revision: 12, effect: { kind: "empty" } }), adapter);
  expect(adopt).toHaveBeenCalledTimes(1); expect(invalidate).not.toHaveBeenCalled(); expect(await queue.flush()).toBe(true);
});
it("replacement while native read is held cannot publish or invalidate the new project", async () => {
  const { adapter, adopt, invalidate, view } = historyLane(); let current = true;
  let release: (view: ProjectView) => void = () => { throw new Error("not reading"); };
  const readView = vi.fn(() => new Promise<ProjectView>((resolve) => { release = resolve; }));
  const pending = runHistoryLane({ generation: 7, replacements: 0 }, async () => ({ status: "applied", generation: 7, revision: 12, effect: { kind: "notes", pattern: 1 } }), { ...adapter, current: () => current, readView });
  for (let turn = 0; turn < 4; turn++) await Promise.resolve();
  expect(readView).toHaveBeenCalledTimes(1); current = false; release(view);
  expect(await pending).toEqual({ status: "stale" }); expect(adopt).not.toHaveBeenCalled(); expect(invalidate).not.toHaveBeenCalled();
});
it("same numeric sampler state still publishes a fresh observed object, isolating its channel", async () => {
  const { adapter, view, adopt, invalidate } = historyLane();
  const channel = { id: 42, name: "Sampler", color: 0, sampleName: "generated.wav", missing: false, sampleFrames: 2,
    volume: 1, pan: 0, mute: false, solo: false, sampler: { start: 0, end: 1 }, kind: "sampler" as const, synth: {}, preset: null, choke: 0, roll: false };
  const before = { ...view, channels: [channel] };
  const fresh = { ...before, channels: [{ ...channel, sampler: { ...channel.sampler } }] };
  await runHistoryLane({ generation: 7, replacements: 0 }, async () => ({ status: "applied", generation: 7, revision: 12, effect: { kind: "samplerTrim", channel: 42 } }), { ...adapter, readView: async () => fresh });
  expect(adopt).toHaveBeenCalledWith(fresh); expect(fresh.channels[0].sampler).toEqual(before.channels[0].sampler);
  expect(fresh.channels[0].sampler).not.toBe(before.channels[0].sampler); expect(invalidate).not.toHaveBeenCalled();
});
it("callback rejection fails the existing mutation queue and does not publish a view", async () => {
  const { adapter, queue, adopt, invalidate } = historyLane(); const error = new Error("native rejection");
  await expect(runHistoryLane({ generation: 7, replacements: 0 }, async () => { throw error; }, adapter)).rejects.toBe(error);
  expect(adopt).not.toHaveBeenCalled(); expect(invalidate).not.toHaveBeenCalled(); expect(await queue.flush()).toBe(false);
});
it("a nonselected target summary arriving after replacement cannot invalidate current notes", async () => {
  const { adapter, adopt, invalidate, readPatterns } = historyLane(); let current = true;
  let release: (patterns: PatternsView) => void = () => { throw new Error("not reading"); };
  const held = vi.fn(() => new Promise<PatternsView>((resolve) => { release = resolve; }));
  const pending = runHistoryLane({ generation: 7, replacements: 0 }, async () => ({ status: "applied", generation: 7, revision: 12, effect: { kind: "notes", pattern: 2 } }), { ...adapter, current: () => current, readPatterns: held });
  for (let turn = 0; turn < 6; turn++) await Promise.resolve();
  expect(held).toHaveBeenCalledTimes(1); const summary = await readPatterns(); current = false; release(summary);
  expect(await pending).toEqual({ status: "stale" }); expect(adopt).not.toHaveBeenCalled(); expect(invalidate).not.toHaveBeenCalled();
});
