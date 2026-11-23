import { expect, it, vi } from "vitest";
import type { ProjectHistoryReply } from "../api/arrangement";
import { dispatchGlobalHistory, type GlobalHistoryAdapter, type HistoryEffect, type HistoryResult } from "./globalHistory";

const origin = { generation: 7, replacements: 2 };
function harness(effect: HistoryEffect = { kind: "notes", pattern: 81 }) {
  const invoke = vi.fn(async (): Promise<ProjectHistoryReply> => ({ generation: 7, revision: 12, status: "applied", effect }));
  const refresh = vi.fn(async () => {});
  const adapter: GlobalHistoryAdapter = { isCurrent: () => true, blocked: () => false, settle: async () => true,
    ordered: (command) => command(), stamp: () => ({ generation: 7, revision: 11 }), invoke, refresh };
  return { adapter, invoke, refresh };
}
it.each<HistoryEffect>([{ kind: "notes", pattern: 81 }, { kind: "samplerTrim", channel: 42 }, { kind: "arrangement" }])("refreshes the exact native target $kind", async (effect) => {
  const { adapter, invoke, refresh } = harness(effect);
  await dispatchGlobalHistory(origin, false, adapter);
  expect(invoke).toHaveBeenCalledWith({ generation: 7, revision: 11 }, true);
  expect(refresh).toHaveBeenCalledWith(effect, expect.objectContaining({ generation: 7, revision: 12 }), origin);
});
it("empty history refreshes nothing; redo uses native undo=false", async () => {
  const { adapter, invoke, refresh } = harness({ kind: "empty" });
  expect(await dispatchGlobalHistory(origin, true, adapter)).toMatchObject({ status: "applied", effect: { kind: "empty" } });
  expect(invoke).toHaveBeenCalledWith({ generation: 7, revision: 11 }, false);
  expect(refresh).not.toHaveBeenCalled();
});
it("failed settlement never queues or invokes native history", async () => {
  const { adapter, invoke } = harness();
  const ordered = vi.fn(adapter.ordered);
  expect(await dispatchGlobalHistory(origin, false, { ...adapter, settle: async () => false, ordered })).toEqual({ status: "unsettled" });
  expect(ordered).not.toHaveBeenCalled(); expect(invoke).not.toHaveBeenCalled();
});
it("native refusal is returned once without target refresh or retry", async () => {
  const { adapter, refresh } = harness();
  const reply: ProjectHistoryReply = { generation: 7, revision: 11, status: "refused", message: "Busy", reload: true };
  const invoke = vi.fn(async () => reply);
  expect(await dispatchGlobalHistory(origin, false, { ...adapter, invoke })).toBe(reply);
  expect(invoke).toHaveBeenCalledTimes(1); expect(refresh).not.toHaveBeenCalled();
});
it("samples the canonical stamp only when existing queues release the command", async () => {
  const { adapter, invoke } = harness(); let revision = 11;
  let release: () => Promise<HistoryResult> = async () => { throw new Error("not queued"); };
  const ordered = vi.fn((command: () => Promise<HistoryResult>) => new Promise<HistoryResult>((resolve) => {
    release = async () => { const result = await command(); resolve(result); return result; };
  }));
  const pending = dispatchGlobalHistory(origin, false, { ...adapter, ordered, stamp: () => ({ generation: 7, revision }) });
  await Promise.resolve(); expect(ordered).toHaveBeenCalledTimes(1); expect(invoke).not.toHaveBeenCalled();
  revision = 12; await release(); await pending;
  expect(invoke).toHaveBeenCalledWith({ generation: 7, revision: 12 }, true);
});
it.each(["replacement", "drag"] as const)("rechecks %s after waiting for ordering", async (cause) => {
  const { adapter, invoke } = harness(); let waiting = false;
  const result = await dispatchGlobalHistory(origin, false, { ...adapter,
    isCurrent: () => cause !== "replacement" || !waiting, blocked: () => cause === "drag" && waiting,
    ordered: (command) => { waiting = true; return command(); } });
  expect(result).toEqual({ status: cause === "replacement" ? "stale" : "blocked" });
  expect(invoke).not.toHaveBeenCalled();
});
it("a held reply from a replaced project cannot refresh a channel reused by the new project", async () => {
  const { adapter, refresh } = harness(); let current = true;
  let release: (reply: ProjectHistoryReply) => void = () => { throw new Error("not invoked"); };
  const invoke = vi.fn(() => new Promise<ProjectHistoryReply>((resolve) => { release = resolve; }));
  const pending = dispatchGlobalHistory(origin, false, { ...adapter, isCurrent: () => current, invoke });
  await Promise.resolve(); await Promise.resolve();
  expect(invoke).toHaveBeenCalledTimes(1); current = false;
  release({ generation: 7, revision: 12, status: "applied", effect: { kind: "samplerTrim", channel: 42 } });
  expect(await pending).toEqual({ status: "stale" }); expect(refresh).not.toHaveBeenCalled();
});
it.each([{ generation: 8, revision: 12 }, { generation: 7, revision: 10 }])("rejects stale history acknowledgement %j", async (stamp) => {
  const { adapter, refresh } = harness();
  expect(await dispatchGlobalHistory(origin, false, { ...adapter, invoke: async () => ({ ...stamp, status: "applied", effect: { kind: "notes", pattern: 81 } }) })).toEqual({ status: "stale" });
  expect(refresh).not.toHaveBeenCalled();
});
it("does not settle during an active drag or mutate captured replacement identity", async () => {
  const { adapter, invoke } = harness(); const settle = vi.fn(async () => true);
  expect(await dispatchGlobalHistory(origin, false, { ...adapter, blocked: () => true, settle })).toEqual({ status: "blocked" });
  expect(settle).not.toHaveBeenCalled(); expect(invoke).not.toHaveBeenCalled();
  const captured = { ...origin };
  const isCurrent = vi.fn((at: typeof origin) => at.replacements === 2);
  await dispatchGlobalHistory(captured, false, { ...adapter, isCurrent, settle: async () => { captured.replacements = 3; return true; } });
  expect(isCurrent.mock.calls.every(([at]) => at.replacements === 2)).toBe(true);
});

it("replacement during the execution stamp read aborts before native history", async () => {
  const { adapter, invoke } = harness(); let current = true;
  let release: (stamp: { generation: number; revision: number }) => void = () => { throw new Error("not reading"); };
  const stamp = vi.fn(() => new Promise<{ generation: number; revision: number }>((resolve) => { release = resolve; }));
  const pending = dispatchGlobalHistory(origin, false, { ...adapter, stamp, isCurrent: () => current });
  await Promise.resolve(); expect(stamp).toHaveBeenCalledTimes(1); current = false;
  release({ generation: 7, revision: 11 });
  expect(await pending).toEqual({ status: "stale" }); expect(invoke).not.toHaveBeenCalled();
});
it("active drag beginning during the native stamp read aborts before mutation", async () => {
  const { adapter, invoke } = harness(); let drag = false;
  const result = await dispatchGlobalHistory(origin, false, { ...adapter, blocked: () => drag,
    stamp: async () => { drag = true; return { generation: 7, revision: 11 }; } });
  expect(result).toEqual({ status: "blocked" }); expect(invoke).not.toHaveBeenCalled();
});
