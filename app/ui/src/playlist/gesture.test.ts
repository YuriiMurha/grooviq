import { describe, expect, it } from "vitest";
import { PatternGesture, type GestureInput } from "./gesture";
import { MAX_SONG_TICKS } from "./geometry";

const input: GestureInput = { kind: "move", pointerId: 3, stamp: { generation: 2, revision: 9 }, replacements: 4,
  originTick: 1920, originTrack: 0, quantum: 240, sourceTicks: 3840, tracks: [{ id: 11 }, { id: 15 }],
  clip: { id: 12, name: "Beat", trackIndex: 0, startTick: 1920, muted: false, content: { type: "pattern", pattern: 7, offsetTick: 480, lengthTicks: 1920 } } };
const current = { pointerId: 3, stamp: { generation: 2, revision: 9 }, replacements: 4 };

describe("one native batch per released playlist gesture", () => {
  it.each([undefined, 0, 1.5, NaN])("blocks resize without a valid captured source period %s", sourceTicks => {
    expect(() => new PatternGesture({ ...input, kind: "resizeStart", sourceTicks })).toThrow();
    expect(() => new PatternGesture({ ...input, kind: "resizeEnd", sourceTicks })).toThrow();
  });

  it("captures duration by value and refuses release after a source-length revision change", () => {
    const captured = { ...input, kind: "resizeStart" as const, sourceTicks: 3840,
      clip: { ...input.clip, content: { ...input.clip.content, offsetTick: 0, lengthTicks: 7680 } } };
    const gesture = new PatternGesture(captured);
    captured.sourceTicks = 960;
    gesture.update(input.originTick + 2160, 0);
    expect(gesture.preview.content.offsetTick).toBe(2160);
    expect(gesture.take({ ...current, stamp: { ...current.stamp, revision: 10 } })).toBeNull();
    expect(gesture.take(current)).toBeNull();
  });

  it.each([
    { shift: 3600, offset: 3600 },
    { shift: 3840, offset: 0 },
    { shift: 4080, offset: 240 },
  ])("emits the native canonical source offset after a $shift tick trim", ({ shift, offset }) => {
    // The clip uses source7 even when another source is selected. Its captured duration,
    // rather than the clip's repeated length or a bar, defines the exact native phase.
    const captured = { ...input, kind: "resizeStart" as const, sourceTicks: 3840,
      clip: { ...input.clip, content: { ...input.clip.content, offsetTick: 0, lengthTicks: 11520 } } };
    const gesture = new PatternGesture(captured);
    gesture.update(input.originTick + shift, 0);
    expect(gesture.take(current)).toEqual([{ kind: "resize", clip: 12, startTick: 1920 + shift,
      content: { type: "pattern", pattern: 7, offsetTick: offset, lengthTicks: 11520 - shift } }]);
    expect(gesture.take(current)).toBeNull();
  });

  it("canonicalizes a valid loaded multi-cycle offset on right resize", () => {
    const captured = { ...input, kind: "resizeEnd" as const, sourceTicks: 3840,
      clip: { ...input.clip, content: { ...input.clip.content, offsetTick: 4320 } } };
    const gesture = new PatternGesture(captured);
    gesture.update(input.originTick + 240, 0);
    expect(gesture.take(current)).toEqual([{ kind: "resize", clip: 12, startTick: 1920,
      content: { type: "pattern", pattern: 7, offsetTick: 480, lengthTicks: 2160 } }]);
  });

  it("previews without editing its captured state and releases the original clip once", () => {
    const gesture = new PatternGesture(input);
    gesture.update(2420, 1);
    expect(gesture.preview.startTick).toBe(2400);
    expect(gesture.preview.trackIndex).toBe(1);
    expect(input.clip.startTick).toBe(1920);
    expect(gesture.take(current)).toEqual([{ kind: "move", clip: 12, track: 15, startTick: 2400 }]);
    expect(gesture.take(current)).toBeNull();
  });

  it("does not let a different pointer consume the owner's release", () => {
    const gesture = new PatternGesture(input);
    gesture.update(2160, 0);
    expect(gesture.take({ ...current, pointerId: 8 })).toBeNull();
    expect(gesture.take(current)).toEqual([{ kind: "move", clip: 12, track: 11, startTick: 2160 }]);
  });

  it.each([
    { domain: "generation", guard: { ...current, stamp: { generation: 3, revision: 9 } } },
    { domain: "revision", guard: { ...current, stamp: { generation: 2, revision: 10 } } },
    { domain: "replacement", guard: { ...current, replacements: 5 } },
  ])("drops a stale $domain rather than rebasing it", ({ guard }) => {
    const gesture = new PatternGesture(input);
    gesture.update(2160, 0);
    expect(gesture.take(guard)).toBeNull();
    expect(gesture.take(current)).toBeNull();
  });

  it("cancels on Escape/capture-loss/transition without a release command", () => {
    const gesture = new PatternGesture(input);
    gesture.update(2400, 1);
    gesture.cancel();
    expect(gesture.take(current)).toBeNull();
    gesture.update(3000, 0);
    expect(gesture.preview.startTick).toBe(2400);
  });

  it("copies a clip with the native linked-copy operation, not a new pattern", () => {
    const gesture = new PatternGesture({ ...input, kind: "copy" });
    gesture.update(3840, 1);
    expect(gesture.take(current)).toEqual([{ kind: "copy", clip: 12, track: 15, startTick: 3840 }]);
    expect(gesture.preview.content.pattern).toBe(7);
  });

  it("captures source and track identities by value, not changing UI selection", () => {
    const mutable = { ...input, stamp: { ...input.stamp }, tracks: [{ id: 11 }, { id: 15 }], clip: { ...input.clip, content: { ...input.clip.content } } };
    const gesture = new PatternGesture(mutable);
    mutable.clip.content.pattern = 99; mutable.tracks[1].id = 77; mutable.stamp.revision = 10;
    gesture.update(2160, 1);
    expect(gesture.preview.content.pattern).toBe(7);
    expect(gesture.take(current)).toEqual([{ kind: "move", clip: 12, track: 15, startTick: 2160 }]);
  });

  it("atomically places the first clip on a native-created track without inventing IDs", () => {
    const gesture = new PatternGesture({ ...input, kind: "place", tracks: [], originTick: 0, clip: { ...input.clip, id: null, startTick: 0, content: { ...input.clip.content, offsetTick: 0, lengthTicks: 3840 } } });
    gesture.update(980, 0);
    expect(gesture.preview.id).toBeNull();
    expect(gesture.take(current)).toEqual([
      { kind: "createTrack", name: "Track 1", color: 0 },
      { kind: "createClipOnCreatedTrack", trackOperation: 0, name: "Beat", startTick: 960, muted: false, content: { type: "pattern", pattern: 7, offsetTick: 0, lengthTicks: 3840 } },
    ]);
  });

  it("resizes the captured source and sends no command for an unchanged move", () => {
    expect(new PatternGesture(input).take(current)).toEqual([]);
    const gesture = new PatternGesture({ ...input, kind: "resizeStart" });
    gesture.update(2160, 1);
    expect(gesture.take(current)).toEqual([{ kind: "resize", clip: 12, startTick: 2160, content: { type: "pattern", pattern: 7, offsetTick: 720, lengthTicks: 1680 } }]);
  });

  it("clamps moved extents to the song horizon and rejects fake existing identities", () => {
    const gesture = new PatternGesture(input);
    gesture.update(MAX_SONG_TICKS * 2, 99);
    expect(gesture.preview.startTick).toBe(MAX_SONG_TICKS - 1920);
    expect(gesture.preview.trackIndex).toBe(1);
    expect(() => new PatternGesture({ ...input, clip: { ...input.clip, id: null } })).toThrow();
    expect(() => new PatternGesture({ ...input, kind: "place", tracks: [], clip: { ...input.clip, id: null, trackIndex: 5 } })).toThrow();
  });
});

describe("single pattern Slice", () => {
  const captured: GestureInput = { ...input, kind: "slice", quantum: 960, originTick: 2940,
    clip: { ...input.clip, startTick: 1050, content: { ...input.clip.content, lengthTicks: 7680 } } };

  it("snaps absolute song time and sends one original-ID slice without moving or retargeting", () => {
    const gesture = new PatternGesture(captured);
    gesture.update(2940, 1);
    expect(gesture.preview).toEqual(captured.clip);
    expect(gesture.take(current)).toEqual([{ kind: "slice", clip: 12, tick: 2880, audioFirstFrames: null }]);
    expect(gesture.take(current)).toBeNull();
    expect(captured.clip.startTick).toBe(1050);
  });

  it.each([0, 960, 9600, 100000])("refuses snapped endpoint or outside cut %s rather than clamping a fragment", tick => {
    const gesture = new PatternGesture(captured);
    gesture.update(tick, 0);
    expect(gesture.take(current)).toEqual([]);
  });

  it("uses the snapped interior position even when the pointer lies just past an off-grid end", () => {
    const gesture = new PatternGesture(captured); gesture.update(9000, 0);
    expect(gesture.take(current)).toEqual([{ kind: "slice", clip: 12, tick: 8640, audioFirstFrames: null }]);
  });

  it.each([960, 8640])("does not cut at an exact aligned endpoint %s", tick => {
    const gesture = new PatternGesture({ ...captured, clip: { ...captured.clip, startTick: 960 } });
    gesture.update(tick, 0); expect(gesture.take(current)).toEqual([]);
  });

  it("does not invent an interior tick for a one-tick loaded clip", () => {
    const gesture = new PatternGesture({ ...captured, clip: { ...captured.clip,
      startTick: 960, content: { ...captured.clip.content, lengthTicks: 1 } } });
    gesture.update(960.5, 0);
    expect(gesture.take(current)).toEqual([]);
  });

  it("captures ID and stamp before a caller changes its source, selection or object", () => {
    const mutable = { ...captured, stamp: { ...captured.stamp }, clip: { ...captured.clip, content: { ...captured.clip.content } } };
    const gesture = new PatternGesture(mutable);
    mutable.clip.id = 99; mutable.clip.content.pattern = 88; mutable.stamp.revision = 30;
    gesture.update(2940, 1);
    expect(gesture.take(current)).toEqual([{ kind: "slice", clip: 12, tick: 2880, audioFirstFrames: null }]);
  });

  it("a foreign pointer cannot consume the original cut", () => {
    const gesture = new PatternGesture(captured); gesture.update(2940, 0);
    expect(gesture.take({ ...current, pointerId: 11 })).toBeNull();
    expect(gesture.take(current)).toEqual([{ kind: "slice", clip: 12, tick: 2880, audioFirstFrames: null }]);
  });

  it.each([
    { domain: "generation", guard: { ...current, stamp: { generation: 3, revision: 9 } } },
    { domain: "revision", guard: { ...current, stamp: { generation: 2, revision: 10 } } },
    { domain: "replacement", guard: { ...current, replacements: 5 } },
  ])("a stale $domain permanently retires a cut", ({ guard }) => {
    const gesture = new PatternGesture(captured); gesture.update(2940, 0);
    expect(gesture.take(guard)).toBeNull(); expect(gesture.take(current)).toBeNull();
  });

  it("cancellation cannot release or later revive a cut", () => {
    const gesture = new PatternGesture(captured); gesture.update(2940, 0);
    gesture.cancel(); gesture.update(4800, 0);
    expect(gesture.take(current)).toBeNull();
  });
});
