import { describe, expect, it } from "vitest";
import { keyboardLoop, LoopGesture, rulerTick } from "./ruler";
const guard = { pointerId: 1, stamp: { generation: 2, revision: 3 }, replacements: 0 };
describe("loop ruler", () => {
  it("maps scaled and scrolled input", () => {
    expect(rulerTick(150, { left: 100, width: 200 }, 400, { width: 400, height: 80, scrollTick: 3840, scrollTrack: 0, tickWidth: .1, rowHeight: 64 })).toBe(4840);
  });
  it("uses the painted ruler width when the canvas loses width to a scrollbar", () => {
    expect(rulerTick(510, { left: 0, width: 1000 }, 1000, { width: 983, height: 80, scrollTick: 0, scrollTrack: 0, tickWidth: .05, rowHeight: 64 })).toBe(10200);
  });
  it("creates a canonical keyboard loop and nudges either edge independently", () => {
    const created = keyboardLoop("Home", null, 3840, 960);
    expect(created).toEqual({ startTick: 3840, endTick: 4800, enabled: true });
    expect(keyboardLoop("ArrowRight", created, 0, 960)).toEqual({ startTick: 3840, endTick: 5760, enabled: true });
    expect(keyboardLoop("ArrowDown", created, 0, 960)).toEqual({ startTick: 2880, endTick: 4800, enabled: true });
    expect(keyboardLoop("ArrowUp", { startTick: 10, endTick: 11, enabled: false }, 0, 960))
      .toEqual({ startTick: 10, endTick: 11, enabled: false });
  });
  for (const [origin, end, expected] of [[1000, 4000, [960, 3840]], [4000, 1000, [960, 3840]], [1000, 1001, [960, 1920]], [38400000, 40000000, [38399040, 38400000]]] as const) it(`snaps ${origin} to ${end}`, () => {
    const gesture = new LoopGesture(guard, origin, 960, 0);
    gesture.update(end, 10);
    expect(gesture.take(guard)).toEqual([{ kind: "setLoop", region: { startTick: expected[0], endTick: expected[1], enabled: true } }]);
    expect(gesture.take(guard)).toBeNull();
  });
  it("keeps a disabled interval disabled when redrawn", () => {
    const gesture = new LoopGesture(guard, 0, 960, 0, false); gesture.update(3840, 20);
    expect(gesture.take(guard)).toEqual([{ kind: "setLoop", region: { startTick: 0, endTick: 3840, enabled: false } }]);
  });
  it("reserves clicks and cancels stale input", () => {
    const click = new LoopGesture(guard, 0, 960, 0);
    expect(click.update(960, 3)).toBeNull();
    expect(click.take(guard)).toBeNull();
    for (const stale of [{ ...guard, pointerId: 2 }, { ...guard, replacements: 1 }, { ...guard, stamp: { ...guard.stamp, revision: 4 } }, { ...guard, stamp: { ...guard.stamp, generation: 4 } }]) {
      const gesture = new LoopGesture(guard, 0, 960, 0); gesture.update(3840, 10); expect(gesture.take(stale)).toBeNull();
    }
    const gesture = new LoopGesture(guard, 0, 960, 0); gesture.update(3840, 10); gesture.cancel(); expect(gesture.take(guard)).toBeNull();
  });
});
