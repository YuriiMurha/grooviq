import { expect, it } from "vitest";
import { validateAudioTicket } from "../../api/arrangement";
import { nextMediaTicket } from "./mediaTicket";

const words = (...values: [number, number][]) => () => {
  const next = values.shift();
  if (!next) throw new Error("Out of random words");
  return next;
};

it("spans the whole native ticket range and never returns zero", () => {
  expect(nextMediaTicket(words([0xffff_ffff, 0xffff_ffff]))).toBe(Number.MAX_SAFE_INTEGER);
  expect(nextMediaTicket(words([0, 0], [0, 1]))).toBe(1);
  expect(nextMediaTicket(words([1, 0]))).toBe(0x1_0000_0000);
});

it("returns tickets the native wrapper accepts", () => {
  const seen = new Set<number>();
  for (let i = 0; i < 256; i += 1) {
    const ticket = nextMediaTicket();
    expect(() => validateAudioTicket(ticket)).not.toThrow();
    seen.add(ticket);
  }
  expect(seen.size).toBe(256);
});
