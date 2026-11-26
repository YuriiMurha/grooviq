import { describe, expect, it } from "vitest";
import type { AudioContent } from "../../api/arrangement";
import { MAX_SONG_TICKS } from "../geometry";
import { AudioEditGesture, type AudioEditInput } from "./editGesture";

const content: AudioContent = {
  type: "audio", asset: 4, sourceStart: 24000, sourceEnd: 72000,
  gain: 0.75, reverse: false, fadeIn: 100, fadeOut: 200,
};
const guard = { pointerId: 3, stamp: { generation: 1, revision: 2 }, replacements: 0 };
const input: AudioEditInput = {
  ...guard, kind: "resizeStart", originTick: 1920, quantum: 1,
  clip: { id: 5, startTick: 1920, kind: "audio", trackIndex: 0, name: "Break", muted: false, ink: 0, lengthTicks: 1920 },
  content, sourceRate: 48000, sourceFrames: 96000, bpm: 120,
};

describe("audio edit gestures", () => {
  it.each([
    { kind: "resizeStart", reverse: false, delta: 480, start: 36000, end: 72000, onset: 2400 },
    { kind: "resizeEnd", reverse: false, delta: -480, start: 24000, end: 60000, onset: 1920 },
    { kind: "resizeStart", reverse: true, delta: 480, start: 24000, end: 60000, onset: 2400 },
    { kind: "resizeEnd", reverse: true, delta: -480, start: 36000, end: 72000, onset: 1920 },
  ] as const)("trims $kind chronologically with reverse=$reverse", ({ kind, reverse, delta, start, end, onset }) => {
    const gesture = new AudioEditGesture({ ...input, kind, content: { ...content, reverse } });
    gesture.update(input.originTick + delta);
    expect(gesture.preview).toMatchObject({ startTick: onset, lengthTicks: 1440 });
    expect(gesture.take(guard)).toEqual([{ kind: "resize", clip: 5, startTick: onset,
      content: { ...content, reverse, sourceStart: start, sourceEnd: end } }]);
    expect(gesture.take(guard)).toBeNull();
  });

  it.each([
    { kind: "resizeStart", delta: 5000, start: 71999, end: 72000, onset: 3839 },
    { kind: "resizeEnd", delta: -5000, start: 24000, end: 24001, onset: 1920 },
  ] as const)("clamps $kind and both fades to the last frame", ({ kind, delta, start, end, onset }) => {
    const gesture = new AudioEditGesture({ ...input, kind });
    gesture.update(input.originTick + delta);
    expect(gesture.preview).toMatchObject({ startTick: onset, lengthTicks: 1 });
    expect(gesture.take(guard)).toEqual([{ kind: "resize", clip: 5, startTick: onset,
      content: { ...content, sourceStart: start, sourceEnd: end, fadeIn: 1, fadeOut: 1 } }]);
  });

  it.each([
    { rate: 44100, start: 9503, length: 1384 },
    { rate: 48000, start: 9813, length: 1257 },
  ])("uses project f32 tempo and the asset rate $rate", ({ rate, start, length }) => {
    const source = { ...content, sourceStart: 6000, sourceEnd: 37513 };
    const gesture = new AudioEditGesture({ ...input, content: source, sourceRate: rate, bpm: 136.123456789 });
    gesture.update(input.originTick + 173);
    expect(gesture.preview).toMatchObject({ startTick: 2093, lengthTicks: length });
    expect(gesture.take(guard)).toEqual([{ kind: "resize", clip: 5, startTick: 2093,
      content: { ...source, sourceStart: start } }]);
  });

  it("fits a right extension within the song horizon and a left extension within tick zero", () => {
    const right = new AudioEditGesture({ ...input, kind: "resizeEnd", clip: { ...input.clip, startTick: MAX_SONG_TICKS - 1921 } });
    right.update(input.originTick + 100);
    expect(right.preview).toMatchObject({ startTick: MAX_SONG_TICKS - 1921, lengthTicks: 1921 });
    expect(right.take(guard)).toEqual([{ kind: "resize", clip: 5, startTick: MAX_SONG_TICKS - 1921,
      content: { ...content, sourceEnd: 72025 } }]);
    const left = new AudioEditGesture({ ...input, clip: { ...input.clip, startTick: 1 } });
    left.update(input.originTick - 100);
    expect(left.preview).toMatchObject({ startTick: 0, lengthTicks: 1921 });
    expect(left.take(guard)).toEqual([{ kind: "resize", clip: 5, startTick: 0,
      content: { ...content, sourceStart: 23975 } }]);
  });

  it("commits a source change even when both timeline edges round to the same ticks", () => {
    const source = { ...content, sourceEnd: 24026, fadeIn: 0, fadeOut: 0 };
    const gesture = new AudioEditGesture({ ...input, kind: "resizeEnd", content: source, sourceFrames: 24049 });
    gesture.update(input.originTick + 1);
    expect(gesture.preview).toMatchObject({ startTick: 1920, lengthTicks: 2 });
    expect(gesture.take(guard)).toEqual([{ kind: "resize", clip: 5, startTick: 1920,
      content: { ...source, sourceEnd: 24049 } }]);
  });

  it.each([
    { rate: 48000, bpm: 120, tick: 2880, frames: 24000 },
    { rate: 8000, bpm: 100, tick: 1921, frames: 5 },
    { rate: 8000, bpm: 137, tick: 1921, frames: 3 },
    { rate: 8000, bpm: 137, tick: 1922, frames: 7 },
  ])("slices exactly at $tick with $frames frames at $rate Hz and $bpm BPM", ({ rate, bpm, tick, frames }) => {
    const gesture = new AudioEditGesture({ ...input, kind: "slice", sourceRate: rate, bpm });
    gesture.update(tick);
    expect(gesture.sliceTick).toBe(tick);
    expect(gesture.take(guard)).toEqual([{ kind: "slice", clip: 5, tick, audioFirstFrames: frames }]);
  });

  it.each([1920, 3840])("refuses a slice at clip edge %i", tick => {
    const gesture = new AudioEditGesture({ ...input, kind: "slice", quantum: 960 });
    gesture.update(tick + 1);
    expect(gesture.sliceTick).toBeNull();
    expect(gesture.take(guard)).toEqual([]);
  });

  it("refuses a slice whose rounded right fragment would exceed the song horizon", () => {
    const source = { ...content, sourceEnd: 24014, fadeIn: 0, fadeOut: 0 };
    const gesture = new AudioEditGesture({
      ...input, kind: "slice", content: source, sourceRate: 8000, bpm: 137,
      clip: { ...input.clip, startTick: MAX_SONG_TICKS - 4 },
    });
    gesture.update(MAX_SONG_TICKS - 3);
    expect(gesture.take(guard)).toEqual([]);
  });

  it("ignores a foreign pointer and cancels on Escape or a stale arrangement", () => {
    const gesture = new AudioEditGesture(input);
    gesture.update(2400);
    expect(gesture.take({ ...guard, pointerId: 4 })).toBeNull();
    gesture.cancel();
    expect(gesture.take(guard)).toBeNull();
    const stale = new AudioEditGesture(input);
    stale.update(2400);
    expect(stale.take({ ...guard, stamp: { ...guard.stamp, revision: 3 } })).toBeNull();
    expect(stale.take(guard)).toBeNull();
    expect(new AudioEditGesture(input).take(guard)).toEqual([]);
  });
});
