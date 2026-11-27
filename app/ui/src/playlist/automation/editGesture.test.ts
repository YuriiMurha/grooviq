import { expect, it } from "vitest";
import type { CurvePoint } from "../../api/arrangement";
import { MAX_SONG_TICKS } from "../geometry";
import { AutomationEditGesture, type AutomationEditInput } from "./editGesture";

const points: readonly CurvePoint[] = [
  { tick: 480, value: 0.5, mode: "linear" },
  { tick: 5280, value: 0.75, mode: "hold" },
  { tick: 9600, value: 1, mode: "linear" },
];
const guard = { pointerId: 2, stamp: { generation: 1, revision: 3 }, replacements: 1 };
const input = {
  ...guard, kind: "resizeStart", originTick: 1920, quantum: 960,
  clip: { id: 9, startTick: 1920, lengthTicks: 3840, name: "Curve", kind: "automation", trackIndex: 0, ink: 0, muted: false },
  content: { type: "automation", offsetTick: 4800, lengthTicks: 3840, target: { type: "channelVolume", channel: 1 }, priority: 7, points },
} satisfies AutomationEditInput;

it.each([
  { kind: "resizeStart", delta: 960, startTick: 2880, offsetTick: 5760, lengthTicks: 2880 },
  { kind: "resizeStart", delta: -960, startTick: 960, offsetTick: 3840, lengthTicks: 4800 },
  { kind: "resizeEnd", delta: -960, startTick: 1920, offsetTick: 4800, lengthTicks: 2880 },
] as const)("submits the full curve with linear phase on $kind ($delta ticks)", ({ kind, delta, startTick, offsetTick, lengthTicks }) => {
  const gesture = new AutomationEditGesture({ ...input, kind });
  gesture.update(input.originTick + delta);
  expect(gesture.preview).toMatchObject({ startTick, lengthTicks });
  expect(gesture.take(guard)).toEqual([
    { kind: "resize", clip: 9, startTick, content: { ...input.content, offsetTick, lengthTicks } },
  ]);
  expect(gesture.take(guard)).toBeNull();
});

it.each([
  { kind: "resizeStart", start: 840, offset: 1920, delta: -9600, result: [0, 1080, 4680] },
  { kind: "resizeStart", start: 1920, offset: 480, delta: -9600, result: [1440, 0, 4320] },
  { kind: "resizeStart", start: 1920, offset: 4800, delta: 9600, result: [5759, 8639, 1] },
  { kind: "resizeEnd", start: 1920, offset: 4800, delta: -9600, result: [1920, 4800, 1] },
  { kind: "resizeEnd", start: MAX_SONG_TICKS - 4001, offset: 4800, delta: 9600, result: [MAX_SONG_TICKS - 4001, 4800, 4001] },
  { kind: "resizeEnd", start: 1920, offset: MAX_SONG_TICKS - 4001, delta: 9600, result: [1920, MAX_SONG_TICKS - 4001, 4001] },
] as const)("bounds $kind to a positive interval in both domains ($start, $offset, $delta)", ({ kind, start, offset, delta, result }) => {
  const gesture = new AutomationEditGesture({
    ...input, kind, clip: { ...input.clip, startTick: start }, content: { ...input.content, offsetTick: offset },
  });
  const [startTick, offsetTick, lengthTicks] = result;
  gesture.update(input.originTick + delta);
  expect(gesture.take(guard)).toEqual([
    { kind: "resize", clip: 9, startTick, content: { ...input.content, offsetTick, lengthTicks } },
  ]);
});

it("only sends a native slice for a snapped tick strictly inside the clip", () => {
  for (const tick of [2000, 5700]) {
    const gesture = new AutomationEditGesture({ ...input, kind: "slice", originTick: tick });
    expect(gesture.sliceTick).toBeNull();
    expect(gesture.take(guard)).toEqual([]);
  }
  const gesture = new AutomationEditGesture({ ...input, kind: "slice", originTick: 3800 });
  expect(gesture.sliceTick).toBe(3840);
  expect(gesture.take(guard)).toEqual([{ kind: "slice", clip: 9, tick: 3840, audioFirstFrames: null }]);
});

it.each([
  { ...guard, replacements: 2 },
  { ...guard, stamp: { generation: 2, revision: 3 } },
  { ...guard, stamp: { generation: 1, revision: 4 } },
])("retires a stale captured curve and geometry %j", current => {
  const gesture = new AutomationEditGesture(input);
  gesture.update(2880);
  expect(gesture.take(current)).toBeNull();
  expect(gesture.take(guard)).toBeNull();
});

it("ignores foreign release, skips a no-op, and drops a cancelled preview", () => {
  const gesture = new AutomationEditGesture(input);
  expect(gesture.take({ ...guard, pointerId: 3 })).toBeNull();
  gesture.update(1925);
  expect(gesture.take(guard)).toEqual([]);
  const cancelled = new AutomationEditGesture(input);
  cancelled.update(2880);
  cancelled.cancel();
  expect(cancelled.take(guard)).toBeNull();
});
