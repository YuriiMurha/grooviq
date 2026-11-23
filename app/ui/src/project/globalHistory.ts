import type { ArrangementStamp, ProjectHistoryReply } from "../api/arrangement";
import type { PatternsView, PatternStamp } from "../api/patterns";
import type { ProjectView } from "../api/project";
import type { ProjectStamp } from "./context";

export type HistoryEffect = Extract<ProjectHistoryReply, { status: "applied" }>["effect"];
export type HistoryResult = ProjectHistoryReply | { status: "blocked" | "stale" | "unsettled" };
export type GlobalHistoryAdapter = Readonly<{
  isCurrent: (origin: ProjectStamp) => boolean;
  blocked: () => boolean;
  /** Flush existing sources before entering the queue; false aborts without native invocation. */
  settle: () => Promise<boolean>;
  /** Existing open-roll barrier THEN session queue, never a newly created parallel queue. */
  ordered: (command: () => Promise<HistoryResult>) => Promise<HistoryResult>;
  stamp: () => ArrangementStamp | Promise<ArrangementStamp>;
  invoke: (stamp: ArrangementStamp, undo: boolean) => Promise<ProjectHistoryReply>;
  /** Read-only, exact target; asynchronous observations must retain the origin fence. */
  refresh: (effect: HistoryEffect, stamp: ArrangementStamp, origin: ProjectStamp) => Promise<void>;
}>;

export async function dispatchGlobalHistory(origin: ProjectStamp, redo: boolean, adapter: GlobalHistoryAdapter): Promise<HistoryResult> {
  // Copy before awaiting: an adapter cannot rebind this intent to the replacement project.
  const captured = { ...origin };
  if (!adapter.isCurrent(captured)) return { status: "stale" };
  if (adapter.blocked()) return { status: "blocked" };
  if (!await adapter.settle()) return { status: "unsettled" };
  return adapter.ordered(async () => {
    if (!adapter.isCurrent(captured)) return { status: "stale" };
    if (adapter.blocked()) return { status: "blocked" };
    const stamp = await adapter.stamp();
    if (!adapter.isCurrent(captured)) return { status: "stale" };
    if (adapter.blocked()) return { status: "blocked" };
    if (stamp.generation !== captured.generation) return { status: "stale" };
    const reply = await adapter.invoke(stamp, !redo);
    if (!adapter.isCurrent(captured) || reply.generation !== captured.generation || reply.revision < stamp.revision) return { status: "stale" };
    if (reply.status === "applied" && ((reply.effect.kind === "notes" && (!Number.isSafeInteger(reply.effect.pattern) || reply.effect.pattern < 0)) ||
      (reply.effect.kind === "samplerTrim" && (!Number.isSafeInteger(reply.effect.channel) || reply.effect.channel < 0)))) throw new Error("Invalid native history target");
    if (reply.status === "applied" && reply.effect.kind !== "empty") await adapter.refresh(reply.effect, reply, captured);
    return adapter.isCurrent(captured) ? reply : { status: "stale" };
  });
}

/** A structured refusal still rejects within the mutation queue, so flush cannot certify it. */
class HistoryOutcome extends Error {
  constructor(readonly result: HistoryResult) { super("History was not applied"); }
}
export type HistoryLaneAdapter = Readonly<{
  enqueueMutation: (command: () => Promise<HistoryResult>) => Promise<HistoryResult>;
  current: () => boolean;
  readView: () => Promise<ProjectView>;
  readPatterns: () => Promise<PatternsView>;
  adopt: (view: ProjectView) => void;
  invalidate: (stamp: PatternStamp) => void;
}>;

/** Runs only in the existing session lane; no settlement or roll wait belongs here. */
export async function runHistoryLane(origin: ProjectStamp, command: () => Promise<HistoryResult>, adapter: HistoryLaneAdapter): Promise<HistoryResult> {
  const captured = { ...origin };
  const requireCurrent = () => { if (!adapter.current()) throw new HistoryOutcome({ status: "stale" }); };
  try {
    return await adapter.enqueueMutation(async () => {
      requireCurrent();
      const reply = await command();
      requireCurrent();
      if (reply.status !== "applied" && reply.status !== "refused") throw new HistoryOutcome(reply);
      if (reply.generation !== captured.generation) throw new HistoryOutcome({ status: "stale" });
      const view = await adapter.readView();
      requireCurrent();
      if (view.generation !== captured.generation) throw new HistoryOutcome({ status: "stale" });
      let notes: PatternStamp | null = null;
      if (reply.status === "applied" && reply.effect.kind === "notes") {
        const patternId = reply.effect.pattern;
        if (view.pattern.id === patternId) notes = { generation: view.generation, patternId, revision: view.notesRevision };
        else {
          const patterns = await adapter.readPatterns();
          requireCurrent();
          if (patterns.generation !== captured.generation) throw new HistoryOutcome({ status: "stale" });
          const target = patterns.patterns.find((pattern) => pattern.id === patternId);
          if (!target) throw new Error("The affected pattern is no longer available. Refresh the project.");
          notes = { generation: patterns.generation, patternId, revision: target.revision };
        }
      }
      // Publish the fresh native object, including same-value sampler parameters and capabilities.
      adapter.adopt(view);
      requireCurrent();
      if (notes) adapter.invalidate(notes);
      if (reply.status === "refused") throw new HistoryOutcome(reply);
      return reply;
    });
  } catch (error) {
    if (error instanceof HistoryOutcome) return error.result;
    throw error;
  }
}
