import { statSync } from "node:fs";
import { expect, test } from "vitest";
import { themes as shippedThemes } from "../src/theme/preference";
import { themes } from "./contract";

test("exactly the shipped themes have implemented gallery entries and token files", () => {
  const implemented = themes.filter((theme) => theme.status === "implemented");
  expect(implemented.map((theme) => theme.id).sort()).toEqual(shippedThemes.map((theme) => theme.id).sort());
  for (const theme of implemented) {
    expect(theme.tokens).toBe(`src/theme/${theme.id}.css`);
    expect(statSync(new URL(`../${theme.tokens}`, import.meta.url)).isFile()).toBe(true);
  }
});
