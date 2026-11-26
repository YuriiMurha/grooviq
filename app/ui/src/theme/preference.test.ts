import { expect, test } from "vitest";
import { readThemePreference, resolveThemeId } from "./preference";

test.each([null, undefined, "", "taste", "riso.css", " RISO ", '{"theme":"riso"}', 1, {}])(
  "unregistered preference %j falls back without coercing it",
  (value) => expect(resolveThemeId(value)).toBe("riso"),
);

test("the available Riso preference is retained", () => {
  expect(resolveThemeId("riso")).toBe("riso");
});

test("unavailable storage cannot stop appearance bootstrap", () => {
  expect(readThemePreference(() => { throw new DOMException("Storage unavailable", "SecurityError"); })).toBe("riso");
});
