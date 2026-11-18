import { invoke } from "@tauri-apps/api/core";
import type { ProjectView } from "./project";
import { rollEditsSettled } from "../roll/commitQueue";

export interface PatternsView {
  generation: number;
  revision: number;
  selected: number;
  patterns: { id: number; name: string; revision: number }[];
  view: ProjectView;
}
export type PatternEdit =
  | { kind: "create"; name: string }
  | { kind: "duplicate" | "select" | "delete"; pattern: number }
  | { kind: "rename"; pattern: number; name: string }
  | { kind: "makeUnique"; clip: number };
export const patternList = () => invoke<PatternsView>("pattern_list");
export async function patternEdit(base: PatternsView, edit: PatternEdit): Promise<PatternsView> {
  if (!(await rollEditsSettled())) throw new Error("Pending note edits were refused. Reload before changing patterns.");
  return invoke<PatternsView>("pattern_edit", { generation: base.generation, revision: base.revision, edit });
}
