import { describe, expect, it } from "vitest";
import { crossedThreshold, marqueeRect, marqueeSelection, PatternGroupGesture, toggleSelection, togglesSelection } from "./groupGesture";
import { MAX_SONG_TICKS, type PatternPreview } from "./geometry";
import type { GestureInput } from "./gesture";
const clip = (id: number, startTick: number, trackIndex: number, lengthTicks = 100): PatternPreview => ({ id, startTick, trackIndex, name: "Beat", muted: false, content: { type: "pattern", pattern: 0, offsetTick: 0, lengthTicks } });
const clips = [clip(1, 120, 0), clip(2, 420, 2)];
const input: GestureInput = { kind: "move", pointerId: 3, stamp: { generation: 1, revision: 4 }, replacements: 2, originTick: 120, originTrack: 0, quantum: 60, tracks: [{ id: 10 }, { id: 20 }, { id: 30 }, { id: 40 }], clip: clips[0] };
const guard = { pointerId: 3, stamp: input.stamp, replacements: 2 };
describe("pattern group gesture", () => {
  it("preserves spacing at shared left and bottom boundaries with one consumable batch", () => {
    const gesture = new PatternGroupGesture(input, clips);
    gesture.update(-900, 9);
    expect(gesture.previews.map(c => [c.startTick, c.trackIndex])).toEqual([[0, 1], [300, 3]]);
    expect(gesture.take(guard)).toEqual([{ kind: "move", clip: 1, track: 20, startTick: 0 }, { kind: "move", clip: 2, track: 40, startTick: 300 }]);
    expect(gesture.take(guard)).toBeNull();
  });
  it("constrains the entire copy at an off-grid horizon", () => {
    const originals = [clip(1, MAX_SONG_TICKS - 503, 1), clip(2, MAX_SONG_TICKS - 103, 2)];
    const gesture = new PatternGroupGesture({ ...input, kind: "copy" }, originals);
    gesture.update(9000, 0);
    expect(gesture.previews.map(c => c.startTick)).toEqual([MAX_SONG_TICKS - 500, MAX_SONG_TICKS - 100]);
    expect(gesture.previews.every(c => c.id === null)).toBe(true);
    expect(gesture.take(guard)?.map(op => op.kind)).toEqual(["copy", "copy"]);
    expect(originals[1].startTick).toBe(MAX_SONG_TICKS - 103);
  });
  it.each([{ ...guard, replacements: 3 }, { ...guard, stamp: { generation: 2, revision: 4 } }, { ...guard, stamp: { generation: 1, revision: 5 } }])("refuses an entire stale batch %j", current => {
    const gesture = new PatternGroupGesture(input, clips); gesture.update(360, 1);
    expect(gesture.take(current)).toBeNull(); expect(gesture.take(guard)).toBeNull();
  });
  it("cancels capture/Save and ignores a foreign pointer without consuming", () => {
    const gesture = new PatternGroupGesture(input, clips); gesture.update(360, 1);
    expect(gesture.take({ ...guard, pointerId: 99 })).toBeNull();
    gesture.cancel(); expect(gesture.take(guard)).toBeNull();
    const next = new PatternGroupGesture(input, clips);
    expect(next.take({ ...guard, pointerId: 99 })).toBeNull();
    expect(next.take(guard)).toEqual([]);
  });
});
it("chooses toggle modifiers and a four pixel drag independently of grid", () => {
  for (const key of ["shiftKey", "ctrlKey", "metaKey"]) expect(togglesSelection({ shiftKey: false, ctrlKey: false, metaKey: false, [key]: true })).toBe(true);
  expect(toggleSelection([1, 2], 1)).toEqual([2]); expect(toggleSelection([1], 2)).toEqual([1, 2]);
  expect(crossedThreshold({ x: 0, y: 0 }, { x: 3.99, y: 0 })).toBe(false);
  expect(crossedThreshold({ x: 0, y: 0 }, { x: 4, y: 0 })).toBe(true);
});
it("marquees pattern rectangles in either direction using the current camera", () => {
  const view = { width: 500, height: 300, tickWidth: 1, rowHeight: 64, scrollTick: 100, scrollTrack: 0 };
  expect(marqueeSelection(clips, marqueeRect({ x: 450, y: 190 }, { x: 0, y: 0 }), view)).toEqual([1, 2]);
  expect(marqueeSelection(clips, { x: 0, y: 0, width: 20, height: 18 }, view)).toEqual([]);
});
