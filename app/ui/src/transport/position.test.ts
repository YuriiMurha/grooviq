import { describe, expect, it } from "vitest";
import { displayTicks, formatPosition } from "./position";

const FOUR_FOUR = { beats: 4, unit: 4 };
const SEVEN_EIGHT = { beats: 7, unit: 8 };

describe("formatPosition", () => {
  it("formats_ticks_as_bar_beat_step_in_4_4_and_7_8", () => {
    // 960 ticks a quarter note, 240 a step (a 16th).
    expect(formatPosition(0n, FOUR_FOUR)).toBe("1.1.01");
    expect(formatPosition(240n, FOUR_FOUR)).toBe("1.1.02");
    expect(formatPosition(960n, FOUR_FOUR)).toBe("1.2.01");
    expect(formatPosition(3839n, FOUR_FOUR)).toBe("1.4.04");
    expect(formatPosition(3840n, FOUR_FOUR)).toBe("2.1.01");
    // The mockup's "9.3.02": bar 9, beat 3, step 2.
    expect(formatPosition(8n * 3840n + 2n * 960n + 240n, FOUR_FOUR)).toBe("9.3.02");

    // 7/8: a beat is an eighth (480 ticks, two steps), a bar is 3360 ticks.
    expect(formatPosition(479n, SEVEN_EIGHT)).toBe("1.1.02");
    expect(formatPosition(480n, SEVEN_EIGHT)).toBe("1.2.01");
    expect(formatPosition(720n, SEVEN_EIGHT)).toBe("1.2.02");
    expect(formatPosition(3359n, SEVEN_EIGHT)).toBe("1.7.02");
    expect(formatPosition(3360n, SEVEN_EIGHT)).toBe("2.1.01");
  });

  it("keeps the step at 01 when a beat is shorter than a step", () => {
    // 5/32: a beat is 120 ticks.
    expect(formatPosition(120n, { beats: 5, unit: 32 })).toBe("1.2.01");
  });

  it("copes with a zero signature instead of dividing by zero", () => {
    expect(formatPosition(100n, { beats: 0, unit: 0 })).toBe("1.1.01");
  });
});

describe("displayTicks", () => {
  it("shifts_by_output_latency_and_never_goes_negative", () => {
    // 120 BPM: 1920 ticks a second. 48 000 frames at 48 kHz is one second: 1920 ticks earlier.
    expect(displayTicks(5000n, 48_000, 120, 48_000)).toBe(3080n);
    // 480 frames = 10 ms = 19.2 ticks, rounded.
    expect(displayTicks(1000n, 480, 120, 48_000)).toBe(981n);
    // Never below 0, including at the start of play.
    expect(displayTicks(100n, 48_000, 120, 48_000)).toBe(0n);
    expect(displayTicks(0n, 480, 120, 48_000)).toBe(0n);
    // No latency, nothing to shift.
    expect(displayTicks(777n, 0, 140, 44_100)).toBe(777n);
  });

  it("ignores a rate it cannot divide by", () => {
    expect(displayTicks(500n, 480, 120, 0)).toBe(500n);
  });
});
