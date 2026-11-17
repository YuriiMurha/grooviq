import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PLAY_GRACE_MS, PlayGuard } from "./playGuard";

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

describe("the play guard", () => {
  it("stopped_frames_are_believed_before_any_play", () => {
    const guard = new PlayGuard();
    expect(guard.believe(false)).toBe(true);
    expect(guard.believe(true)).toBe(true);
    expect(guard.believe(false)).toBe(true);
  });

  it("after_play_stopped_frames_wait_for_the_window", () => {
    expect(PLAY_GRACE_MS).toBe(300);
    const guard = new PlayGuard();
    guard.played();
    // A frame rendered before Play took effect: not believed.
    expect(guard.believe(false)).toBe(false);
    vi.advanceTimersByTime(PLAY_GRACE_MS - 1);
    expect(guard.believe(false)).toBe(false);
    // No playing frame in 300 ms: the engine really stopped (or never started).
    vi.advanceTimersByTime(1);
    expect(guard.believe(false)).toBe(true);
    expect(guard.believe(false)).toBe(true);
  });

  it("a_playing_frame_ends_the_wait_at_once", () => {
    const guard = new PlayGuard();
    guard.played();
    vi.advanceTimersByTime(20);
    expect(guard.believe(true)).toBe(true);
    // From now on a stopped frame is newer than the Play: believed at once.
    expect(guard.believe(false)).toBe(true);
    // A second Play starts a new wait.
    guard.played();
    expect(guard.believe(false)).toBe(false);
  });
});
