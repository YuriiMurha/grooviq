import type { PatternStamp } from "../api/patterns";
import type { NotesAck } from "../api/notes";
import type { BarrierResult, NotesCommand } from "../roll/commitQueue";

/** The open piano roll, as undo and redo see it. */
export interface RollHistory {
  /** Runs the command in the roll's commit queue, after its edits in flight; the roll fetches
   * its notes again afterwards. */
  run(command: NotesCommand): void;
  barrier?<Result>(command: (still: () => boolean) => Promise<Result>, activeDrag: () => boolean,
    accepted?: (value: Result) => boolean): Promise<BarrierResult<Result>>;
  detachBarriers?: () => void;
  invalidateHistory?: (stamp: PatternStamp) => void;
}

let roll: RollHistory | null = null;
/** Pointers held down anywhere in the window: a drag is under way. */
const pressed = new Set<number>();

/** The piano roll registers itself while it is mounted; returns the unregister. */
export function setRollHistory(r: RollHistory): () => void {
  if (roll !== r) roll?.detachBarriers?.();
  roll = r;
  return () => {
    r.detachBarriers?.();
    if (roll === r) roll = null;
  };
}

/** Follows the pointers held down in `win` (a drag on the roll, the velocity lane, a knob, the
 * step grid); returns the stop. Losing the window lets go of them all. */
export function trackDrags(win: Pick<Window, "addEventListener" | "removeEventListener">): () => void {
  const down = (e: PointerEvent) => pressed.add(e.pointerId);
  const up = (e: PointerEvent) => pressed.delete(e.pointerId);
  const clear = () => pressed.clear();
  win.addEventListener("pointerdown", down, true);
  win.addEventListener("pointerup", up, true);
  win.addEventListener("pointercancel", up, true);
  win.addEventListener("blur", clear);
  return () => {
    win.removeEventListener("pointerdown", down, true);
    win.removeEventListener("pointerup", up, true);
    win.removeEventListener("pointercancel", up, true);
    win.removeEventListener("blur", clear);
    clear();
  };
}

/** Undoes or redoes a note change, unless a drag is under way (ignored: returns false).
 * `command` runs it in the project's queue, behind the step edits sent before it. With the
 * piano roll open it first waits its turn in the roll's queue, so it never overtakes a note edit
 * either, and the roll keeps the view and fetches the notes after. With the roll closed `adopt`
 * keeps the view; `fail` shows a refusal. */
export function stepNotesHistory(
  command: NotesCommand,
  adopt: (ack: NotesAck) => void,
  fail: (e: unknown) => void,
): boolean {
  if (pressed.size > 0) return false;
  if (roll) {
    roll.run(command);
  } else {
    command().then((ack) => ack && adopt(ack), fail);
  }
  return true;
}

export const historyDragActive = () => pressed.size > 0;
/** Caller settles shared edit sources before this barrier; never flush from its callback. */
export async function withRollHistoryBarrier<Result>(command: (still: () => boolean) => Promise<Result>,
  current: () => boolean, accepted: (value: Result) => boolean = () => true): Promise<BarrierResult<Result>> {
  if (!current()) return { status: "cancelled", reason: "stale" };
  if (historyDragActive()) return { status: "cancelled", reason: "blocked" };
  const opened = roll;
  if (opened) {
    if (!opened.barrier) return { status: "cancelled", reason: "stale" };
    const result = await opened.barrier((still) => {
      if (!current()) throw new Error("The project changed before history reached its turn.");
      return command(() => still() && current());
    }, historyDragActive, accepted);
    return current() ? result : { status: "cancelled", reason: "stale" };
  }
  const still = () => current() && !historyDragActive() && roll === opened;
  try {
    if (!still()) return { status: "cancelled", reason: "stale" };
    const value = await command(still);
    return still() ? { status: "completed", value } : { status: "cancelled", reason: "stale" };
  } catch (error) { return { status: "cancelled", reason: "failed", error }; }
}

/** Only the currently registered roll can consume the exact native effect target. */
export function invalidateRollHistory(stamp: PatternStamp) { roll?.invalidateHistory?.(stamp); }
