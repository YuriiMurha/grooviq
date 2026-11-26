import { readFileSync } from "node:fs";
import { expect, test } from "vitest";
import { THEME_COLOR_TOKENS, themeTokenProblems } from "./contract";
import { DEFAULT_THEME, themes } from "./preference";
import { parseThemeDeclarations, resolveThemeDeclarations } from "./themeSource";

test("reads custom properties from the selected rule, including multiline values", () => {
  const declarations = parseThemeDeclarations(`
    /* :root[data-theme="test"] { --ink: ignored; } */
    :root { --other: ignored; }
    :root[data-theme='test'] {
      --ink: #123; --paper: #456; /* --ignored: #789; */
      --shadow: 0 20px 40px rgba(0, 0, 0, 0.5),
        0 0 0 1px var(--ink)
    }
  `, ':root[data-theme="test"]');
  expect(declarations).toEqual(new Map([
    ["--ink", "#123"],
    ["--paper", "#456"],
    ["--shadow", "0 20px 40px rgba(0, 0, 0, 0.5),\n        0 0 0 1px var(--ink)"],
  ]));
});

test("resolves alias chains and multi-value properties against the overlaid palette", () => {
  const base = parseThemeDeclarations(":root { --ink: #123; --step: var(--ink); --device: var(--step); --shadow: 0 0 0 1px var(--device); }", ":root");
  const variant = parseThemeDeclarations(':root[data-theme="test"] { --ink: #456; }', ':root[data-theme="test"]');
  const resolved = resolveThemeDeclarations(new Map([...base, ...variant]));
  expect(resolved.get("--device")).toBe("#456");
  expect(resolved.get("--shadow")).toBe("0 0 0 1px #456");
});

test.each(["--absent", "--shadow"])("a reference to %s invalidates the whole declaration", (reference) => {
  const declarations = parseThemeDeclarations(`:root { --shadow: 0 0 0 1px var(${reference}); }`, ":root");
  expect(resolveThemeDeclarations(declarations).get("--shadow")).toBe("");
});

const readSource = (id: string) => readFileSync(new URL(`./${id}.css`, import.meta.url), "utf8");

test.each(themes)("$id source satisfies the renderer token contract", ({ id }) => {
  const base = parseThemeDeclarations(readSource(DEFAULT_THEME), ":root");
  const own = id === DEFAULT_THEME ? base : parseThemeDeclarations(readSource(id), `:root[data-theme="${id}"]`);
  expect(own.size, `${id} declares its palette`).toBeGreaterThan(0);

  // Shared Riso aliases resolve against this palette. A missing literal override would silently use Riso.
  const tokens = [...THEME_COLOR_TOKENS, "--reg-x", "--reg-y", "--radius-s", "--radius-m", "--radius-l"];
  const missing = tokens.filter((name) => !own.get(name) && !/^var\(\s*--[\w-]+\s*\)$/.test(base.get(name) ?? ""));
  expect(missing, `${id} declares every required token except shared aliases`).toEqual([]);

  const incompatible = [...own].filter(([, value]) => /\b(?:color-mix|oklch)\(/i.test(value)).map(([name]) => name);
  expect(incompatible, `${id} source uses renderer-compatible color syntax before the build`).toEqual([]);

  const resolved = resolveThemeDeclarations(new Map([...base, ...own]));
  expect(themeTokenProblems((name) => resolved.get(name) ?? "")).toEqual([]);
});
