import { expect, test } from "vitest";
import { contrastRatio } from "./contrast";

test("black and white have the maximum contrast in either order", () => {
  const black = [0, 0, 0] as const;
  const white = [1, 1, 1] as const;
  expect(contrastRatio(black, white)).toBe(21);
  expect(contrastRatio(white, black)).toBe(21);
  expect(contrastRatio(white, white)).toBe(1);
});

test("contrast uses linear-light sRGB with distinct channel weights", () => {
  expect(contrastRatio([1, 0, 0], [0, 0, 0])).toBeCloseTo(5.252, 3);
  expect(contrastRatio([0, 1, 0], [0, 0, 0])).toBeCloseTo(15.304, 3);
  expect(contrastRatio([0, 0, 1], [0, 0, 0])).toBeCloseTo(2.444, 3);
});

test("adjacent gray values can fall on either side of AA without rounding", () => {
  const white = [1, 1, 1] as const;
  const passing = 118 / 255;
  const failing = 119 / 255;
  expect(contrastRatio([passing, passing, passing], white)).toBeGreaterThan(4.5);
  expect(contrastRatio([failing, failing, failing], white)).toBeLessThan(4.5);
});
