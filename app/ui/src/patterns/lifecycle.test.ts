import { expect, it, vi } from "vitest";
import type { PatternReply, PatternsView } from "../api/patterns";
import { acceptPickerReply, canPublishPatterns, performPickerAction, pickerIntent, type PickerState } from "./lifecycle";

function view(revision = 1, selected = 1): PatternsView {
  return { generation: 7, revision, selected, patterns: [1, 2].map((id) => ({ id, name: `Pattern ${id}`, revision: 1 })),
    view: { generation: 7, patternRevision: revision, name: "Original", path: null, dirty: false, bpm: 120,
      timeSignature: { beats: 4, unit: 4 }, channels: [], pattern: { id: selected, name: "Pattern", steps: 16, swing: 0, cells: {} },
      autosavedAt: null, key: null, notesRevision: 1, recording: false, canUndo: false, canRedo: false } };
}
const applied = (state: PatternsView): PatternReply => ({ status: "applied", state, settlement: { recordingDisarmed: false, waiting: false } });

it("publishes native selection rather than optimistic requested selection", () => {
  const base = view();
  const intent = pickerIntent(base, 0, { kind: "select", pattern: 2 });
  const state: PickerState = { canonical: base, pending: intent, refusal: null };
  expect(state.canonical?.selected).toBe(1);
  expect(acceptPickerReply(state, intent, 0, applied(view(2, 2))).canonical?.selected).toBe(2);
});
it.each(["unknownPattern", "engineBusy", "staleRevision", "projectReplaced", "pendingRecording"] as const)("keeps refusal %s without retrying or pretending selection", async (reason) => {
  const base = view(); const intent = pickerIntent(base, 0, { kind: "select", pattern: 2 });
  const reply: PatternReply = { status: "refused", reason, message: "Refused", state: base, settlement: { recordingDisarmed: false, waiting: false } };
  const action = vi.fn(async () => reply);
  const result = await performPickerAction(intent, { isCurrent: () => true, canonical: () => base, action });
  expect(action).toHaveBeenCalledTimes(1);
  expect(result).toBe(reply);
  expect(acceptPickerReply({ canonical: base, pending: intent, refusal: null }, intent, 0, reply)).toEqual({ canonical: base, pending: null, refusal: reply });
});
it("rejects delayed older revisions, generations and inconsistent selection", () => {
  const current = view(3, 2);
  expect(canPublishPatterns(current, view(2), 7)).toBe(false);
  expect(canPublishPatterns(current, { ...view(4), generation: 8 }, 7)).toBe(false);
  expect(canPublishPatterns(current, { ...view(4), selected: 99 }, 7)).toBe(false);
  expect(canPublishPatterns(current, view(3, 1), 7)).toBe(false);
  expect(canPublishPatterns({ ...current, view: { ...current.view, notesRevision: 5 } }, current, 7)).toBe(false);
});
it("an old reply cannot clear a newer pending identity or survive replacement", () => {
  const base = view(); const old = pickerIntent(base, 0, { kind: "select", pattern: 2 });
  const latest = pickerIntent(base, 0, { kind: "select", pattern: 1 });
  const state = { canonical: base, pending: latest, refusal: null };
  expect(acceptPickerReply(state, old, 0, applied(view(2, 2)))).toBe(state);
  expect(acceptPickerReply(state, latest, 1, applied(view(2)))).toBe(state);
});
it("does not send retired targets or rebind captured pattern to selection", async () => {
  const base = view(); const intent = pickerIntent(base, 0, { kind: "rename", pattern: 2, name: "Bass" });
  const action = vi.fn(async () => applied(view(3)));
  await performPickerAction(intent, { isCurrent: () => true, canonical: () => ({ ...base, patterns: base.patterns.filter((p) => p.id !== 2) }), action });
  expect(action).not.toHaveBeenCalled();
  await performPickerAction(intent, { isCurrent: () => true, canonical: () => base, action });
  expect(action).toHaveBeenCalledWith(base, { kind: "rename", pattern: 2, name: "Bass" });
});
it("drops a held reply after project replacement", async () => {
  let current = true; let release: (reply: PatternReply) => void = () => { throw new Error("not started"); };
  const base = view(); const intent = pickerIntent(base, 0, { kind: "create", name: "New" });
  const result = performPickerAction(intent, { isCurrent: () => current, canonical: () => base,
    action: () => new Promise((resolve) => { release = resolve; }) });
  current = false; release(applied(view(2, 2)));
  expect(await result).toBeNull();
});
