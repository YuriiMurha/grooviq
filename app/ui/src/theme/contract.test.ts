import { expect, test } from "vitest";
import { PALETTE_TOKENS } from "../rack/palette";
import { THEME_COLOR_TOKENS, themeTokenProblems } from "./contract";

const valid = (name: string) => name.startsWith("--reg-") || name.startsWith("--radius-") ? "2px" : "#123456";

test("the complete resolved color contract is accepted", () => {
  expect(themeTokenProblems(valid)).toEqual([]);
});

test.each(["", "var(--accent)", "color-mix(in srgb, red, blue)", "oklch(60% .2 30)"])(
  "missing or renderer-incompatible selected-note color %s is rejected",
  (color) => expect(themeTokenProblems((name) => name === "--roll-select" ? color : valid(name)))
    .toEqual([{ token: "--roll-select", reason: "color" }]),
);

test("alpha colors and zero registration offsets are valid", () => {
  expect(themeTokenProblems((name) => name.startsWith("--reg-") || name.startsWith("--radius-") ? "0px" : "rgba(243, 238, 228, 0.26)")).toEqual([]);
});

test.each(["", "auto", "2em", "NaN", "Infinity", "3px trailing"])("invalid canvas offset %s is rejected", (offset) => {
  expect(themeTokenProblems((name) => name === "--reg-x" ? offset : valid(name)))
    .toEqual([{ token: "--reg-x", reason: "pixels" }]);
});

test("grid renderer token consumers are covered by the contract", () => {
  const covered: readonly string[] = [...THEME_COLOR_TOKENS, "--reg-x", "--reg-y"];
  expect(PALETTE_TOKENS.filter((token) => !covered.includes(token))).toEqual([]);
});

test.each(["", "-4px", "auto", "1em"])("invalid radius %s is rejected", (radius) => {
  expect(themeTokenProblems((name) => name === "--radius-m" ? radius : valid(name)))
    .toEqual([{ token: "--radius-m", reason: "pixels" }]);
});
