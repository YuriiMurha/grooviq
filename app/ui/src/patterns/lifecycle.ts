import type { PatternEdit, PatternReply, PatternsView } from "../api/patterns";

export type PickerAction = Extract<PatternEdit, { kind: "create" | "rename" }> |
  (Omit<Extract<PatternEdit, { kind: "duplicate" | "select" | "delete" }>, "kind"> & { kind: "duplicate" | "select" });
export type PickerIntent = Readonly<{
  identity: object;
  generation: number;
  replacements: number;
  action: PickerAction;
}>;
export type PickerState = Readonly<{
  canonical: PatternsView | null;
  pending: PickerIntent | null;
  refusal: Extract<PatternReply, { status: "refused" }> | null;
}>;

/** Capture the named pattern, never a later selection. Identity also fences same-project replies. */
export function pickerIntent(base: PatternsView, replacements: number, action: PickerAction): PickerIntent {
  return { identity: {}, generation: base.generation, replacements, action: { ...action } };
}

export function canPublishPatterns(previous: PatternsView | null, next: PatternsView, generation: number): boolean {
  if (next.generation !== generation || next.view.generation !== generation || next.view.pattern.id !== next.selected) return false;
  if (!next.patterns.some((pattern) => pattern.id === next.selected)) return false;
  if (!previous) return true;
  if (previous.generation !== generation) return false;
  if (next.revision < previous.revision) return false;
  // A delayed list reply can have the same bank revision but older selected-pattern notes.
  return next.revision !== previous.revision || next.selected === previous.selected &&
    next.view.notesRevision >= previous.view.notesRevision;
}

export function acceptPickerReply(state: PickerState, intent: PickerIntent, replacements: number, reply: PatternReply): PickerState {
  if (state.pending?.identity !== intent.identity || intent.replacements !== replacements) return state;
  const canonical = canPublishPatterns(state.canonical, reply.state, intent.generation) ? reply.state : state.canonical;
  return { canonical, pending: null, refusal: reply.status === "refused" ? reply : null };
}

export type PatternActionAdapter = Readonly<{
  isCurrent: (intent: PickerIntent) => boolean;
  canonical: () => PatternsView | null;
  /** Existing action helpers own settlement/transition; do not call them inside a queue they flush. */
  action: (base: PatternsView, action: PickerAction) => Promise<PatternReply>;
}>;

export async function performPickerAction(intent: PickerIntent, adapter: PatternActionAdapter): Promise<PatternReply | null> {
  if (!adapter.isCurrent(intent)) return null;
  const base = adapter.canonical();
  if (!base || base.generation !== intent.generation) return null;
  const action = intent.action;
  if ("pattern" in action && !base.patterns.some((pattern) => pattern.id === action.pattern)) return null;
  const reply = await adapter.action(base, intent.action);
  return adapter.isCurrent(intent) ? reply : null;
}
