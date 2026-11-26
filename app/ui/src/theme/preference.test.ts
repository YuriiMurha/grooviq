import { expect, test } from "vitest";
import { readThemePreference, resolveThemeId, themes } from "./preference";

test.each([null, undefined, "", "future-theme", "riso.css", " RISO ", '{"theme":"riso"}', 1, {}])(
  "unregistered preference %j falls back without coercing it",
  (value) => expect(resolveThemeId(value)).toBe("riso"),
);

test.each(themes)("the available $name preference is retained", ({ id }) => {
  expect(readThemePreference(() => id)).toBe(id);
});

test("unavailable storage cannot stop appearance bootstrap", () => {
  expect(readThemePreference(() => { throw new DOMException("Storage unavailable", "SecurityError"); })).toBe("riso");
});
