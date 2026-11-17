import { afterEach, describe, expect, it } from "vitest";
import type { NotesAck } from "../api/notes";
import type { NotesCommand } from "../roll/commitQueue";
import { setRollHistory, stepNotesHistory, trackDrags } from "./noteHistory";

const ACK = { revision: 3, reload: true } as NotesAck;
const pointer = (type: string, pointerId: number) => Object.assign(new Event(type), { pointerId });
const flush = () => new Promise<void>((r) => setTimeout(r, 0));

const cleanups: (() => void)[] = [];
afterEach(() => {
  for (const c of cleanups.splice(0)) c();
});

describe("note history", () => {
  it("undo_is_ignored_during_a_drag_and_goes_through_the_open_roll", async () => {
    const win = new EventTarget() as unknown as Window;
    cleanups.push(trackDrags(win));
    const queued: NotesCommand[] = [];
    cleanups.push(setRollHistory({ run: (c) => queued.push(c) }));
    const adopted: NotesAck[] = [];
    const command = () => Promise.resolve(ACK);

    win.dispatchEvent(pointer("pointerdown", 1));
    expect(stepNotesHistory(command, (a) => adopted.push(a), () => {})).toBe(false);
    win.dispatchEvent(pointer("pointerup", 1));
    expect(stepNotesHistory(command, (a) => adopted.push(a), () => {})).toBe(true);
    // The roll's queue runs it (and keeps the view): nothing is adopted here.
    expect(queued).toEqual([command]);
    await flush();
    expect(adopted).toEqual([]);

    // A cancelled pointer and a lost window end the drag too.
    win.dispatchEvent(pointer("pointerdown", 2));
    win.dispatchEvent(pointer("pointercancel", 2));
    win.dispatchEvent(pointer("pointerdown", 3));
    win.dispatchEvent(new Event("blur"));
    expect(stepNotesHistory(command, () => {}, () => {})).toBe(true);
  });

  it("with_the_roll_closed_the_view_is_adopted_and_a_refusal_shown", async () => {
    const adopted: NotesAck[] = [];
    const failed: unknown[] = [];
    stepNotesHistory(() => Promise.resolve(ACK), (a) => adopted.push(a), (e) => failed.push(e));
    stepNotesHistory(() => Promise.resolve(null), (a) => adopted.push(a), (e) => failed.push(e));
    stepNotesHistory(() => Promise.reject("full"), (a) => adopted.push(a), (e) => failed.push(e));
    await flush();
    expect(adopted).toEqual([ACK]);
    expect(failed).toEqual(["full"]);
    // A roll that unmounted no longer takes them.
    const queued: NotesCommand[] = [];
    setRollHistory({ run: (c) => queued.push(c) })();
    stepNotesHistory(() => Promise.resolve(ACK), (a) => adopted.push(a), () => {});
    await flush();
    expect(queued).toEqual([]);
    expect(adopted).toHaveLength(2);
  });
});
