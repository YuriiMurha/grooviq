import { describe, expect, it, vi } from "vitest";
import { createGridWake, subscribeGridDpr } from "./gridWake";

function setup() {
  let nextId = 0;
  const frames = new Map<number, FrameRequestCallback>();
  const request = (callback: FrameRequestCallback) => {
    const id = nextId++;
    frames.set(id, callback);
    return id;
  };
  const cancel = (id: number) => { frames.delete(id); };
  const advance = () => {
    const pending = [...frames.values()];
    frames.clear();
    pending.forEach((frame) => frame(0));
  };
  let playhead: number | null = null;
  const draw = vi.fn<(playhead: number | null) => boolean>(() => true);
  const wake = createGridWake(() => playhead, draw, request, cancel);
  return { frames, advance, draw, wake, setPlayhead: (step: number | null) => { playhead = step; } };
}

describe("grid wake", () => {
  it("coalesces dirty changes and returns to rest after painting", () => {
    const { frames, advance, draw, wake } = setup();
    expect(frames.size).toBe(0);
    wake.markDirty();
    wake.markDirty();
    expect(frames.size).toBe(1);
    advance();
    expect(draw.mock.calls).toEqual([[null]]);
    expect(frames.size).toBe(0);
    wake.markDirty();
    advance();
    expect(draw).toHaveBeenCalledTimes(2);
  });

  it("polls during playback, draws on a step change and clears once on stop", () => {
    const { frames, advance, draw, wake, setPlayhead } = setup();
    wake.markDirty();
    advance();
    draw.mockClear();

    setPlayhead(1);
    wake.startPlayback();
    advance();
    expect(frames.size).toBe(1);
    advance();
    expect(draw.mock.calls).toEqual([[1]]);
    setPlayhead(2);
    advance();
    setPlayhead(null);
    advance();
    expect(draw.mock.calls).toEqual([[1], [2], [null]]);
    expect(frames.size).toBe(0);
  });

  it("sleeps when the renderer cannot draw at rest and cancels a queued wake", () => {
    const { frames, advance, draw, wake } = setup();
    draw.mockReturnValueOnce(false);
    wake.markDirty();
    advance();
    expect(frames.size).toBe(0);
    wake.markDirty(); // a restored renderer asks to draw again
    advance();
    expect(draw).toHaveBeenCalledTimes(2);
    wake.markDirty();
    wake.cancel();
    advance();
    expect(draw).toHaveBeenCalledTimes(2);
  });
});

it("re-arms the resolution query at each new DPR and removes both listeners on cleanup", () => {
  const host = Object.assign(new EventTarget(), {
    devicePixelRatio: 1,
    matchMedia: vi.fn<(query: string) => EventTarget>(() => new EventTarget()),
  });
  const changed = vi.fn();
  const stop = subscribeGridDpr(host, changed);
  expect(host.matchMedia.mock.calls).toEqual([["(resolution: 1dppx)"]]);
  expect(changed.mock.calls).toEqual([[1]]);
  changed.mockClear();

  const first = host.matchMedia.mock.results[0].value;
  host.devicePixelRatio = 2;
  first.dispatchEvent(new Event("change"));
  expect(changed.mock.calls).toEqual([[2]]);
  expect(host.matchMedia.mock.calls.at(-1)).toEqual(["(resolution: 2dppx)"]);

  const second = host.matchMedia.mock.results[1].value;
  host.devicePixelRatio = 1.5;
  first.dispatchEvent(new Event("change"));
  expect(changed).toHaveBeenCalledTimes(1);
  second.dispatchEvent(new Event("change"));
  expect(changed.mock.calls).toEqual([[2], [1.5]]);

  host.devicePixelRatio = 1;
  host.dispatchEvent(new Event("resize"));
  expect(changed.mock.calls).toEqual([[2], [1.5], [1]]);
  const last = host.matchMedia.mock.results.at(-1)?.value;
  stop();
  host.devicePixelRatio = 2;
  last?.dispatchEvent(new Event("change"));
  host.dispatchEvent(new Event("resize"));
  expect(changed).toHaveBeenCalledTimes(3);
});

it("catches DPR changes without browser events and stops checking after cleanup", () => {
  vi.useFakeTimers();
  const host = Object.assign(new EventTarget(), {
    devicePixelRatio: 1,
    matchMedia: vi.fn<(query: string) => EventTarget>(() => new EventTarget()),
  });
  const changed = vi.fn();
  const stop = subscribeGridDpr(host, changed);
  try {
    changed.mockClear(); // Only count changes after subscribing.
    host.devicePixelRatio = 2;
    vi.advanceTimersByTime(249);
    expect(changed).not.toHaveBeenCalled();
    vi.advanceTimersByTime(751);
    expect(changed.mock.calls).toEqual([[2]]);
    expect(host.matchMedia.mock.calls.at(-1)).toEqual(["(resolution: 2dppx)"]);

    vi.advanceTimersByTime(1000);
    expect(changed).toHaveBeenCalledTimes(1);
    stop();
    expect(vi.getTimerCount()).toBe(0);
    host.devicePixelRatio = 1;
    vi.advanceTimersByTime(1000);
    expect(changed).toHaveBeenCalledTimes(1);
  } finally {
    stop();
    vi.useRealTimers();
  }
});
