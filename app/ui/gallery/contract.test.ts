import { statSync } from "node:fs";
import { expect, test } from "vitest";
import { themes as shippedThemes } from "../src/theme/preference";
import { themes } from "./contract";

test("gallery theme ids are unique", () => {
  const ids = themes.map((theme) => theme.id);
  expect(ids).toEqual([...new Set(ids)]);
});

test("shipped themes are never proposed in the gallery", () => {
  const shippedIds = new Set<string>(shippedThemes.map((theme) => theme.id));
  const proposedShippedIds = themes.filter((theme) => theme.status === "proposed" && shippedIds.has(theme.id))
    .map((theme) => theme.id);
  expect(proposedShippedIds).toEqual([]);
});

test("exactly the shipped themes have implemented gallery entries and token files", () => {
  const implemented = themes.filter((theme) => theme.status === "implemented");
  expect(implemented.map((theme) => theme.id).sort()).toEqual(shippedThemes.map((theme) => theme.id).sort());
  for (const theme of implemented) {
    expect(theme.tokens).toBe(`src/theme/${theme.id}.css`);
    expect(statSync(new URL(`../${theme.tokens}`, import.meta.url)).isFile()).toBe(true);
  }
});
