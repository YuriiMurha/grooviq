import { statSync } from "node:fs";
import { expect, test } from "vitest";
import { themes as shippedThemes } from "../src/theme/preference";
import { themes } from "./contract";

test("gallery theme ids are unique", () => {
  const ids = themes.map((theme) => theme.id);
  expect(ids).toEqual([...new Set(ids)]);
});

test("exactly the shipped themes have implemented gallery entries and token files", () => {
  expect(themes.map((theme) => theme.id).sort()).toEqual(shippedThemes.map((theme) => theme.id).sort());
  for (const theme of themes) {
    expect(theme.status).toBe("implemented");
    expect(theme.tokens).toBe(`src/theme/${theme.id}.css`);
    expect(statSync(new URL(`../${theme.tokens}`, import.meta.url)).isFile()).toBe(true);
  }
});
