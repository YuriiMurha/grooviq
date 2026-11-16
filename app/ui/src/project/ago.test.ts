import { describe, expect, it } from "vitest";
import { formatAgo, formatClock } from "./ago";

describe("formatAgo", () => {
  it("rounds down into s, min and h", () => {
    expect(formatAgo(0)).toBe("just now");
    expect(formatAgo(4.9)).toBe("just now");
    expect(formatAgo(12)).toBe("12 s ago");
    expect(formatAgo(59)).toBe("59 s ago");
    expect(formatAgo(60)).toBe("1 min ago");
    expect(formatAgo(3599)).toBe("59 min ago");
    expect(formatAgo(7300)).toBe("2 h ago");
    expect(formatAgo(-3)).toBe("just now");
  });
});

describe("formatClock", () => {
  it("is the local time as HH:MM", () => {
    const d = new Date(2026, 10, 16, 9, 5);
    expect(formatClock(d.getTime() / 1000)).toBe("09:05");
  });
});
