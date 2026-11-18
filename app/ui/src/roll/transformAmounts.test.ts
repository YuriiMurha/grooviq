import { expect, it } from "vitest";
import { amount, DEFAULT_AMOUNTS } from "./transformAmounts";
import { humanize, strum } from "./transforms";
const notes = [60, 64, 67].map((key, i) => ({ id: i + 1, channel: 1, tick: 480, key, length: 240, velocity: 0.8, slide: false }));
it("preserves the incumbent defaults and clamps invalid amounts", () => {
  expect(DEFAULT_AMOUNTS).toEqual({ spread: 30, timing: 20, velocity: 0.1 });
  expect(amount("spread", -1)).toBe(0);
  expect(amount("timing", 999)).toBe(120);
  expect(amount("velocity", 2)).toBe(1);
  expect(amount("spread", NaN)).toBe(30);
  expect(amount("spread", 30.6)).toBe(31);
});
it("amounts change independent axes and preserve seeded defaults", () => {
  expect(strum(notes, amount("spread", 60), "up").map(n => n.tick)).toEqual([480, 540, 600]);
  expect(strum(notes, amount("spread", 0), "up")).toEqual(notes);
  const base = humanize(notes, DEFAULT_AMOUNTS.timing, DEFAULT_AMOUNTS.velocity, 7);
  expect(base).toEqual(humanize(notes, 20, 0.1, 7));
  const timing = humanize(notes, 120, 0, 7);
  expect(timing.map(n => n.velocity)).toEqual(notes.map(n => n.velocity));
  expect(timing.some(n => n.tick !== 480)).toBe(true);
  const velocity = humanize(notes, 0, 1, 7);
  expect(velocity.map(n => n.tick)).toEqual([480, 480, 480]);
  expect(velocity.some(n => n.velocity !== 0.8)).toBe(true);
});
