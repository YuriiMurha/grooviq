import { describe, expect, it } from "vitest";
import { MixerGestures, moveTarget, navTarget, renderIndices, revealScroll, visibleWindow, type ParamCommand, type ParamTarget } from "./mixerModel";

const level = (nodeId: string): ParamTarget => ({ kind: "level", nodeId });
const send = (nodeId: string, sendId: string): ParamTarget => ({ kind: "send", nodeId, sendId });

describe("visibleWindow", () => {
  const base = { pitch: 100, viewport: 350, overscan: 1 };

  it("is empty for an empty list", () => {
    expect(visibleWindow({ ...base, count: 0, scroll: 0 })).toEqual({ start: 0, end: 0 });
  });

  it("covers the first strips plus overscan at the start", () => {
    expect(visibleWindow({ ...base, count: 1000, scroll: 0 })).toEqual({ start: 0, end: 5 });
  });

  it("ends at the last strip when scrolled to the end", () => {
    const w = visibleWindow({ ...base, count: 1000, scroll: 1000 * 100 - 350 });
    expect(w.end).toBe(1000);
    expect(w.start).toBe(995);
  });

  it("clamps scroll past either end and ignores a nonfinite scroll or viewport", () => {
    expect(visibleWindow({ ...base, count: 10, scroll: 1e9 })).toEqual({ start: 5, end: 10 });
    expect(visibleWindow({ ...base, count: 10, scroll: -50 })).toEqual({ start: 0, end: 5 });
    expect(visibleWindow({ ...base, count: 10, scroll: Number.NaN })).toEqual({ start: 0, end: 5 });
    expect(visibleWindow({ ...base, count: 10, scroll: 0, viewport: Number.POSITIVE_INFINITY })).toEqual({ start: 0, end: 2 });
  });

  it("stays bounded by the viewport, not the list length", () => {
    for (const count of [10, 10_000, 1_000_000]) {
      const w = visibleWindow({ ...base, count, scroll: 0 });
      expect(w.end - w.start).toBeLessThanOrEqual(Math.ceil(350 / 100) + 2);
    }
  });
});

describe("renderIndices", () => {
  it("adds pinned strips outside the window, once, in order", () => {
    expect(renderIndices({ start: 4, end: 7 }, [9, 5, 1, -1, 99], 20)).toEqual([1, 4, 5, 6, 9]);
  });

  it("never reaches past a list shortened since the window was measured", () => {
    expect(renderIndices({ start: 5, end: 12 }, [], 8)).toEqual([5, 6, 7]);
    expect(renderIndices({ start: 5, end: 12 }, [], 3)).toEqual([]);
  });

  it("drops pinned indices past the list", () => {
    expect(renderIndices({ start: 0, end: 0 }, [0, 3], 0)).toEqual([]);
  });
});

describe("revealScroll", () => {
  it("keeps the scroll when the strip is already in view", () => {
    expect(revealScroll(3, 200, 350, 100)).toBe(200);
  });
  it("scrolls left to a strip before the view and right to one after it", () => {
    expect(revealScroll(1, 200, 350, 100)).toBe(100);
    expect(revealScroll(9, 200, 350, 100)).toBe(650);
  });
});

describe("navTarget", () => {
  const ids = ["a", "b", "c"];
  it("moves along the list and stops at its ends", () => {
    expect(navTarget(ids, "b", "ArrowRight")).toBe("c");
    expect(navTarget(ids, "b", "ArrowLeft")).toBe("a");
    expect(navTarget(ids, "c", "ArrowRight")).toBe("c");
    expect(navTarget(ids, "a", "ArrowLeft")).toBe("a");
    expect(navTarget(ids, "b", "Home")).toBe("a");
    expect(navTarget(ids, "a", "End")).toBe("c");
  });
  it("ignores other keys, an empty list, and starts from the first strip for a stale id", () => {
    expect(navTarget(ids, "b", "ArrowUp")).toBeNull();
    expect(navTarget([], "b", "ArrowRight")).toBeNull();
    expect(navTarget(ids, "gone", "ArrowRight")).toBe("a");
  });
});

describe("moveTarget", () => {
  const slots = [{ id: "x" }, { id: "y" }, { id: "z" }];
  it("names the slot to land before, by id", () => {
    expect(moveTarget(slots, "y", -1)).toEqual({ beforeSlotId: "x" });
    expect(moveTarget(slots, "x", 1)).toEqual({ beforeSlotId: "z" });
    expect(moveTarget(slots, "y", 1)).toEqual({ beforeSlotId: null });
  });
  it("refuses a move past either end or of a slot that is gone", () => {
    expect(moveTarget(slots, "x", -1)).toBeNull();
    expect(moveTarget(slots, "z", 1)).toBeNull();
    expect(moveTarget(slots, "gone", 1)).toBeNull();
    expect(moveTarget([], "x", 1)).toBeNull();
  });
});

/** A host with a stored value per target key; `respond` decides each commit's promise. */
function host(initial: Record<string, number>) {
  const stored = new Map(Object.entries(initial));
  const locked = new Set<string>();
  const sent: ParamCommand[] = [];
  const settlers: ((ok: boolean) => void)[] = [];
  let async = false;
  const key = (t: ParamTarget) => (t.kind === "send" ? `${t.nodeId}/${t.sendId}` : `${t.nodeId}.${t.kind}`);
  let shownChanges = 0;
  const g = new MixerGestures();
  g.connect({
    emit: (cmd) => {
      sent.push(cmd);
      if (cmd.phase !== "commit" || !async) return;
      return new Promise<boolean>((resolve) => settlers.push(resolve));
    },
    stored: (t) => stored.get(key(t)),
    locked: (t) => locked.has(t.nodeId),
    changed: () => shownChanges++,
  });
  return {
    g,
    sent,
    stored,
    locked,
    useAsync: () => (async = true),
    settle: (ok: boolean, i = settlers.length - 1) => settlers[i]?.(ok),
    shownChanges: () => shownChanges,
  };
}

const flush = () => new Promise((r) => setTimeout(r, 0));

describe("MixerGestures", () => {
  it("previews live, then commits once on the target it began on", () => {
    const h = host({ "a.level": 0 });
    const gesture = h.g.begin(level("a"));
    expect(gesture.input(-3)).toBe(true);
    expect(h.g.shown(level("a"))).toBe(-3);
    gesture.commit(-3);
    expect(h.sent).toEqual([
      { target: level("a"), value: -3, phase: "live" },
      { target: level("a"), value: -3, phase: "commit" },
    ]);
    // A synchronous host is authoritative at once.
    expect(h.g.shown(level("a"))).toBeUndefined();
  });

  it("refuses nonfinite user values without emitting", () => {
    const h = host({ "a.level": 0 });
    const gesture = h.g.begin(level("a"));
    expect(gesture.input(Number.NaN)).toBe(true);
    expect(gesture.input(Number.POSITIVE_INFINITY)).toBe(true);
    gesture.commit(Number.NEGATIVE_INFINITY);
    h.g.set(level("a"), Number.NaN);
    expect(h.sent).toEqual([]);
    expect(h.g.shown(level("a"))).toBeUndefined();
  });

  it("a gesture on a removed target ends silently; its late events reach nobody", () => {
    const h = host({ "a.level": 0, "b.level": 0 });
    const gesture = h.g.begin(level("a"));
    gesture.input(-6);
    h.stored.delete("a.level");
    h.g.retain();
    expect(gesture.alive()).toBe(false);
    expect(gesture.input(-9)).toBe(false);
    gesture.commit(-9);
    gesture.cancel();
    expect(h.sent).toEqual([{ target: level("a"), value: -6, phase: "live" }]);
    expect(h.g.shown(level("a"))).toBeUndefined();
  });

  it("a replacement under a new id cannot inherit a running gesture", () => {
    const h = host({ "a.level": 0 });
    const gesture = h.g.begin(level("a"));
    gesture.input(-2);
    h.stored.delete("a.level");
    h.stored.set("a2.level", 0);
    h.g.retain();
    gesture.commit(-4);
    expect(h.sent.filter((c) => c.target.nodeId === "a2")).toEqual([]);
    expect(h.sent.filter((c) => c.phase === "commit")).toEqual([]);
  });

  it("a removed send ends its gesture even while its strip stays", () => {
    const h = host({ "a/s1": 0.2, "a.level": 0 });
    const gesture = h.g.begin(send("a", "s1"));
    gesture.input(0.5);
    h.stored.delete("a/s1");
    h.g.retain();
    gesture.commit(0.5);
    expect(h.sent.map((c) => c.phase)).toEqual(["live"]);
  });

  it("a cancel after live values restores the engine to where it began", () => {
    const h = host({ "a.level": -1 });
    const gesture = h.g.begin(level("a"));
    gesture.input(-5);
    gesture.cancel();
    expect(h.sent.at(-1)).toEqual({ target: level("a"), value: -1, phase: "cancel" });
    expect(h.g.shown(level("a"))).toBeUndefined();
  });

  it("a gesture that never moved, or came back to its start, makes no edit", () => {
    const h = host({ "a.level": -1 });
    h.g.begin(level("a")).cancel();
    h.g.begin(level("a")).commit(-1);
    expect(h.sent).toEqual([]);
  });

  it("a locked (pending) target starts nothing and takes no one-shot change", () => {
    const h = host({ "a.level": 0 });
    h.locked.add("a");
    expect(h.g.begin(level("a")).input(-3)).toBe(false);
    h.g.set(level("a"), -3);
    expect(h.sent).toEqual([]);
  });

  it("a target locked mid-gesture is cancelled on its own id, then stays quiet", () => {
    const h = host({ "a.level": 0 });
    const gesture = h.g.begin(level("a"));
    gesture.input(-3);
    h.locked.add("a");
    h.g.retain();
    gesture.commit(-3);
    expect(h.sent.map((c) => [c.target.nodeId, c.phase])).toEqual([
      ["a", "live"],
      ["a", "cancel"],
    ]);
  });

  it("shows a committed value until the host settles, then the supplied value, with no follow-up", async () => {
    const h = host({ "a.level": 0 });
    h.useAsync();
    const gesture = h.g.begin(level("a"));
    gesture.input(-8);
    gesture.commit(-8);
    expect(h.g.shown(level("a"))).toBe(-8);
    h.settle(false); // refused: the store still holds 0
    await flush();
    expect(h.g.shown(level("a"))).toBeUndefined();
    expect(h.sent.map((c) => c.phase)).toEqual(["live", "commit"]);
  });

  it("an older settlement does not clear a newer pending value", async () => {
    const h = host({ "a.level": 0 });
    h.useAsync();
    h.g.set(level("a"), -2);
    h.g.set(level("a"), -4);
    h.settle(true, 0);
    await flush();
    expect(h.g.shown(level("a"))).toBe(-4);
    h.settle(true, 1);
    await flush();
    expect(h.g.shown(level("a"))).toBeUndefined();
  });

  it("a key press during a drag ends the drag first, restoring its live value", () => {
    const h = host({ "a.level": 0 });
    const drag = h.g.begin(level("a"));
    drag.input(-6);
    h.g.set(level("a"), -1);
    expect(drag.alive()).toBe(false);
    expect(h.sent.map((c) => [c.value, c.phase])).toEqual([
      [-6, "live"],
      [0, "cancel"],
      [-1, "commit"],
    ]);
  });

  it("dispose cancels running gestures and refuses new ones", () => {
    const h = host({ "a.level": 0 });
    h.g.begin(level("a")).input(-3);
    h.g.dispose();
    expect(h.sent.at(-1)?.phase).toBe("cancel");
    expect(h.g.begin(level("a")).alive()).toBe(false);
    expect(h.shownChanges()).toBeGreaterThan(0);
  });

  it("cancelling a newer drag restores the outstanding commit preview", async () => {
    const h = host({ "a.level": 0 });
    h.useAsync();
    h.g.set(level("a"), -2);
    const drag = h.g.begin(level("a"));
    drag.input(-4);
    drag.cancel();
    expect(h.sent.at(-1)).toEqual({ target: level("a"), value: -2, phase: "cancel" });
    expect(h.g.shown(level("a"))).toBe(-2);
    h.settle(false);
    await flush();
    expect(h.g.shown(level("a"))).toBeUndefined();
  });

  it("a refused earlier commit cannot become a newer drag's cancel value", async () => {
    const h = host({ "a.level": 0 });
    h.useAsync();
    h.g.set(level("a"), -2);
    const drag = h.g.begin(level("a"));
    drag.input(-4);
    h.settle(false);
    await flush();
    drag.cancel();
    expect(h.sent.at(-1)).toEqual({ target: level("a"), value: 0, phase: "cancel" });
    expect(h.g.shown(level("a"))).toBeUndefined();
  });

  it("an accepted earlier commit becomes the authoritative cancel value", async () => {
    const h = host({ "a.level": 0 });
    h.useAsync();
    h.g.set(level("a"), -2);
    const drag = h.g.begin(level("a"));
    drag.input(-4);
    h.stored.set("a.level", -2);
    h.settle(true);
    await flush();
    drag.cancel();
    expect(h.sent.at(-1)).toEqual({ target: level("a"), value: -2, phase: "cancel" });
    expect(h.g.shown(level("a"))).toBeUndefined();
  });

  it("pending host state suppresses an outstanding commit preview without another command", async () => {
    const h = host({ "a.level": 0 });
    h.useAsync();
    h.g.set(level("a"), -8);
    h.locked.add("a");
    h.g.retain();
    expect(h.g.shown(level("a"))).toBeUndefined();
    expect(h.sent).toEqual([{ target: level("a"), value: -8, phase: "commit" }]);
    h.locked.delete("a");
    h.g.retain();
    expect(h.g.shown(level("a"))).toBeUndefined();
    h.settle(false);
    await flush();
    expect(h.g.shown(level("a"))).toBeUndefined();
  });


  it("cancelAll ends live gestures but accepts new input and keeps issued commits", async () => {
    const h = host({ "a.level": 0, "b.level": 0 });
    h.useAsync();
    h.g.set(level("a"), -2);
    const drag = h.g.begin(level("a"));
    drag.input(-4);
    const other = h.g.begin(level("b"));
    other.input(-6);
    expect(h.g.cancelAll()).toBe(true);
    expect(h.g.cancelAll()).toBe(false);
    drag.commit(-4);
    other.commit(-6);
    expect(h.g.shown(level("a"))).toBe(-2);
    expect(h.g.shown(level("b"))).toBeUndefined();
    expect(h.sent.filter(c => c.phase === "commit")).toHaveLength(1);
    h.settle(false);
    await flush();
    expect(h.g.shown(level("a"))).toBeUndefined();
    const next = h.g.begin(level("b"));
    expect(next.input(-1)).toBe(true);
    next.commit(-1);
    expect(h.sent.at(-1)).toEqual({ target: level("b"), value: -1, phase: "commit" });
  });

});
