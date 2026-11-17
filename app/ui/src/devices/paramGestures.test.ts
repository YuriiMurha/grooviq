import { describe, expect, it } from "vitest";
import { ParamGestures, type GestureSink } from "./paramGestures";

/** A sink that records what the owner sends; edits resolve when the test says so. */
function harness(stored: Record<string, number>, suspended: () => boolean = () => false) {
  const log: string[] = [];
  const edits: { id: string; v: number; resolve: (ok: boolean) => void }[] = [];
  let shown: Record<string, number> = {};
  const sink: GestureSink = {
    stored: (id) => stored[id],
    live: (id, v) => log.push(`live ${id} ${v}`),
    dropLive: (id) => log.push(`drop ${id}`),
    restore: (id, v) => log.push(`restore ${id} ${v}`),
    edit: (id, v) =>
      new Promise<boolean>((resolve) => {
        log.push(`edit ${id} ${v}`);
        edits.push({
          id,
          v,
          resolve: (ok) => {
            if (ok) stored[id] = v;
            resolve(ok);
          },
        });
      }),
    shown: (m) => {
      shown = { ...m };
    },
  };
  const g = new ParamGestures(sink, suspended);
  const settle = async (ok = true) => {
    for (const e of edits.splice(0)) e.resolve(ok);
    await Promise.resolve();
    await Promise.resolve();
  };
  return { g, log, shown: () => shown, settle, stored };
}

describe("parameter gestures", () => {
  it("a_gesture_previews_live_and_commits_once", async () => {
    const { g, log, shown, settle } = harness({ cutoff: 650 });
    const drag = g.begin("cutoff");
    drag.input(800);
    drag.input(900);
    expect(shown()).toEqual({ cutoff: 900 });
    drag.commit(900);
    expect(log).toEqual(["live cutoff 800", "live cutoff 900", "drop cutoff", "edit cutoff 900"]);
    // The moved value shows until the project has it.
    expect(shown()).toEqual({ cutoff: 900 });
    await settle();
    expect(shown()).toEqual({});
    // A finished gesture does nothing more.
    expect(drag.alive()).toBe(false);
    expect(drag.input(1000)).toBe(false);
    drag.commit(1000);
    drag.cancel();
    expect(log.length).toBe(4);
  });

  it("a_cancelled_gesture_restores_its_start_without_an_edit", () => {
    const { g, log, shown } = harness({ cutoff: 650 });
    const drag = g.begin("cutoff");
    drag.input(2992);
    drag.cancel();
    expect(log).toEqual(["live cutoff 2992", "drop cutoff", "restore cutoff 650"]);
    expect(shown()).toEqual({});
    // Its late end (a pointerup, a wheel timer) is ignored.
    expect(drag.input(3000)).toBe(false);
    drag.commit(3000);
    expect(log.length).toBe(3);
  });

  it("a_cancel_before_any_move_sends_nothing", () => {
    const { g, log } = harness({ cutoff: 650 });
    g.begin("cutoff").cancel();
    expect(log).toEqual(["drop cutoff"]);
  });

  it("ending_where_it_started_puts_the_engine_back_without_an_edit", () => {
    const { g, log } = harness({ cutoff: 650 });
    const drag = g.begin("cutoff");
    drag.input(700);
    drag.commit(650);
    expect(log).toEqual(["live cutoff 700", "drop cutoff", "restore cutoff 650"]);
  });

  it("cancel_all_and_dispose_end_every_gesture_once", () => {
    const { g, log, shown } = harness({ a: 1, b: 2 });
    const one = g.begin("a");
    const two = g.begin("b");
    one.input(5);
    two.input(6);
    g.dispose();
    expect(log.filter((l) => l.startsWith("restore"))).toEqual(["restore a 1", "restore b 2"]);
    expect(log.filter((l) => l.startsWith("edit"))).toEqual([]);
    expect(shown()).toEqual({});
    // After dispose nothing goes out, whatever the controls still do.
    const before = log.length;
    one.commit(5);
    two.cancel();
    g.begin("a").input(9);
    g.set("a", 3);
    g.cancelAll();
    expect(log.length).toBe(before);
  });

  it("a_refused_edit_puts_the_engine_back_on_the_stored_value", async () => {
    const { g, log, shown, settle } = harness({ cutoff: 650 });
    const drag = g.begin("cutoff");
    drag.input(900);
    drag.commit(900);
    await settle(false);
    expect(log.slice(-1)).toEqual(["restore cutoff 650"]);
    expect(shown()).toEqual({});
  });

  it("a_one_shot_set_cancels_a_running_gesture_on_that_parameter", () => {
    const { g, log } = harness({ cutoff: 650 });
    const drag = g.begin("cutoff");
    drag.input(900);
    g.set("cutoff", 4000);
    expect(log).toEqual(["live cutoff 900", "drop cutoff", "restore cutoff 650", "edit cutoff 4000"]);
    drag.commit(950);
    expect(log.length).toBe(4);
    // A set to the value it already shows is not an edit.
    g.set("cutoff", 4000);
    expect(log.length).toBe(4);
  });

  it("abandoning_sends_nothing_at_all", async () => {
    const { g, log, settle } = harness({ cutoff: 650 });
    const drag = g.begin("cutoff");
    drag.input(900);
    const before = log.length;
    const wheel = g.begin("res");
    g.abandon();
    drag.commit(900);
    wheel.commit(0.5);
    g.set("cutoff", 1);
    expect(log.slice(before)).toEqual(["drop cutoff", "drop res"]);
    // An edit already on its way that fails is not "restored" into another project.
    const h = harness({ cutoff: 650 });
    const d = h.g.begin("cutoff");
    d.input(900);
    d.commit(900);
    h.g.abandon();
    const n = h.log.length;
    await h.settle(false);
    expect(h.log.length).toBe(n);
    await settle();
  });

  it("a_new_gesture_keeps_the_start_of_one_it_replaces", () => {
    const { g, log } = harness({ cutoff: 650 });
    const wheel = g.begin("cutoff");
    wheel.input(700);
    const drag = g.begin("cutoff");
    expect(wheel.input(710)).toBe(false);
    drag.input(800);
    drag.cancel();
    expect(log.slice(-1)).toEqual(["restore cutoff 650"]);
  });
});

describe("gesture groups (one pointer drag over several parameters)", () => {
  it("a_group_cancelled_before_it_moves_accepts_nothing", () => {
    const { g, log } = harness({ cutoff: 650, res: 0.05 });
    const drag = g.group();
    g.cancelAll();
    expect(drag.alive()).toBe(false);
    expect(drag.input("cutoff", 900)).toBe(false);
    expect(drag.input("res", 0.5)).toBe(false);
    drag.commit();
    expect(log).toEqual([]);
  });

  it("once_the_owner_ends_it_no_new_member_can_start", () => {
    const { g, log } = harness({ cutoff: 650, res: 0.05 });
    const drag = g.group();
    expect(drag.input("cutoff", 900)).toBe(true);
    g.cancelAll();
    // The pointer now moves only up and down: resonance was never part of the drag.
    expect(drag.input("res", 0.5)).toBe(false);
    drag.commit();
    expect(log).toEqual(["live cutoff 900", "drop cutoff", "restore cutoff 650"]);
  });

  it("a_member_ended_elsewhere_ends_and_restores_the_whole_group", () => {
    const { g, log } = harness({ decay: 4500, sustain: 0.12 });
    const drag = g.group();
    drag.input("sustain", 0.4);
    drag.input("decay", 3000);
    // A one-shot change of one member (a key on its field) ends the drag for both.
    g.set("sustain", 0.5);
    expect(drag.alive()).toBe(false);
    expect(drag.alive()).toBe(false);
    expect(drag.input("decay", 2000)).toBe(false);
    drag.commit();
    expect(log).toEqual([
      "live sustain 0.4",
      "live decay 3000",
      "drop sustain",
      "restore sustain 0.12",
      // At once, not when the pointer next moves.
      "drop decay",
      "restore decay 4500",
      "edit sustain 0.5",
    ]);
  });

  it("a_group_commits_each_member_once_and_cancels_together", async () => {
    const { g, log, settle } = harness({ cutoff: 650, res: 0.05 });
    const drag = g.group();
    drag.input("cutoff", 900);
    drag.input("res", 0.3);
    drag.commit();
    drag.commit();
    drag.cancel();
    expect(log.filter((l) => l.startsWith("edit"))).toEqual(["edit cutoff 900", "edit res 0.3"]);
    await settle();
    const other = g.group();
    other.input("cutoff", 1000);
    other.cancel();
    expect(other.alive()).toBe(false);
    expect(log.slice(-2)).toEqual(["drop cutoff", "restore cutoff 900"]);
  });

  it("a_closed_owner_hands_out_dead_groups", () => {
    const { g, log } = harness({ cutoff: 650 });
    g.dispose();
    const drag = g.group();
    expect(drag.input("cutoff", 900)).toBe(false);
    expect(log).toEqual([]);
  });
});

describe("a suspended owner (a project replacement on its way)", () => {
  it("starts_nothing_and_changes_nothing", () => {
    let paused = true;
    const { g, log, shown } = harness({ cutoff: 650 }, () => paused);
    expect(g.begin("cutoff").input(900)).toBe(false);
    expect(g.group().input("cutoff", 900)).toBe(false);
    g.set("cutoff", 900);
    expect(log).toEqual([]);
    expect(shown()).toEqual({});
    // A failed replacement: the editor works again.
    paused = false;
    g.set("cutoff", 900);
    expect(log).toEqual(["edit cutoff 900"]);
  });
});
