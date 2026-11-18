import { invoke } from "@tauri-apps/api/core";
import type { ProjectView } from "./project";
import { beginProjectTransition, settleProjectEdits } from "../project/pending";

export type PatternStamp = { generation: number; patternId: number; revision: number };

export interface PatternsView {
  generation: number;
  revision: number;
  selected: number;
  patterns: { id: number; name: string; revision: number }[];
  view: ProjectView;
}
export type PatternEdit =
  | { kind: "settle" }
  | { kind: "create"; name: string }
  | { kind: "duplicate" | "select" | "delete"; pattern: number }
  | { kind: "rename"; pattern: number; name: string }
  | { kind: "makeUnique"; clip: number };
export type PatternRefusal = "projectReplaced" | "staleRevision" | "unknownPattern" | "unknownClip" | "notPatternClip" |
  "lastPattern" | "referencedPattern" | "idExhausted" | "engineBusy" | "fileBudget" | "validation" | "recordingSettling" | "pendingRecording";
export type PatternReply = { status: "applied"; state: PatternsView; settlement: Settlement } |
  { status: "refused"; reason: PatternRefusal; message: string; state: PatternsView; settlement: Settlement };
type Settlement = { recordingDisarmed: boolean; waiting: boolean };
export const patternList = () => invoke<PatternsView>("pattern_list");
export const patternEdit = (base: PatternsView, edit: PatternEdit) =>
  invoke<PatternReply>("pattern_edit", { generation: base.generation, revision: base.revision, edit });

/** Ends recording and waits for its End/barrier. Admission/queue refusals stop the transition;
 * only the explicit recordingSettling acknowledgement is retried. */
export async function settlePattern(base?: PatternsView): Promise<PatternReply> {
  let state = base ?? await patternList();
  const until = performance.now() + 2000;
  let disarmed = false;
  for (;;) {
    const reply = await patternEdit(state, { kind: "settle" });
    disarmed ||= reply.settlement.recordingDisarmed;
    if (reply.status === "applied" || reply.reason !== "recordingSettling" || performance.now() >= until) {
      return { ...reply, settlement: { ...reply.settlement, recordingDisarmed: disarmed } };
    }
    state = reply.state;
    await new Promise<void>((resolve) => setTimeout(resolve, 16));
  }
}
async function action(base: PatternsView, edit: PatternEdit): Promise<PatternReply> {
  const finish = beginProjectTransition();
  try {
    if (!(await settleProjectEdits())) throw new Error("Pending edits were refused; the pattern change was aborted.");
    if (edit.kind === "select" || edit.kind === "delete") {
      const settled = await settlePattern(base);
      if (settled.status === "refused") return settled;
    }
    return await patternEdit(base, edit);
  } finally { finish(); }
}
export const patternCreate = (base: PatternsView, name: string) => action(base, { kind: "create", name });
export const patternDuplicate = (base: PatternsView, pattern: number) => action(base, { kind: "duplicate", pattern });
export const patternRename = (base: PatternsView, pattern: number, name: string) => action(base, { kind: "rename", pattern, name });
export const patternSelect = (base: PatternsView, pattern: number) => action(base, { kind: "select", pattern });
export const patternDelete = (base: PatternsView, pattern: number) => action(base, { kind: "delete", pattern });
export const patternMakeUnique = (base: PatternsView, clip: number) => action(base, { kind: "makeUnique", clip });
