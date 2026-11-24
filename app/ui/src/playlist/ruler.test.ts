import { describe, expect, it } from "vitest";
import { LoopGesture, rulerTick } from "./ruler";
const guard = { pointerId: 1, stamp: { generation: 2, revision: 3 }, replacements: 0 };
describe("loop ruler", () => {
  it("maps scaled and scrolled input", () => {
    expect(rulerTick(150, { left: 100, width: 200 }, { width: 400, height: 80, scrollTick: 3840, scrollTrack: 0, tickWidth: .1, rowHeight: 64 })).toBe(4840);
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
    expect(new LoopGesture(guard, 0, 960, 0).take(guard)).toBeNull();
    for (const stale of [{ ...guard, pointerId: 2 }, { ...guard, replacements: 1 }, { ...guard, stamp: { ...guard.stamp, revision: 4 } }, { ...guard, stamp: { ...guard.stamp, generation: 4 } }]) {
      const gesture = new LoopGesture(guard, 0, 960, 0); gesture.update(3840, 10); expect(gesture.take(stale)).toBeNull();
    }
    const gesture = new LoopGesture(guard, 0, 960, 0); gesture.update(3840, 10); gesture.cancel(); expect(gesture.take(guard)).toBeNull();
  });
});
