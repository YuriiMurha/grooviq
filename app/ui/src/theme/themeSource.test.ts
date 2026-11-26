import { readFileSync } from "node:fs";
import { expect, test } from "vitest";
import { REQUIRED_THEME_TOKENS, themeTokenProblems } from "./contract";
import { DEFAULT_THEME, themes } from "./preference";
import { parseThemeDeclarations, resolveThemeDeclarations } from "./themeSource";

const testSelector = ':root[data-theme="test"]';

test("reads custom properties from the theme rule, including multiline values", () => {
  const declarations = parseThemeDeclarations(`
    /* :root[data-theme="test"] { --ink: ignored; } */
    :root[data-theme='test'] {
      --ink: #123; --paper: #456; /* --ignored: #789; */
      --font: "Name {with; punctuation}", sans-serif;
      --grouped: example([name; --ignored: #abc]);
      --shadow: 0 20px 40px rgba(0, 0, 0, 0.5),
        0 0 0 1px var(--ink)
    }
  `, testSelector);
  expect(declarations).toEqual(new Map([
    ["--ink", "#123"],
    ["--paper", "#456"],
    ["--font", '"Name {with; punctuation}", sans-serif'],
    ["--grouped", "example([name; --ignored: #abc])"],
    ["--shadow", "0 20px 40px rgba(0, 0, 0, 0.5),\n        0 0 0 1px var(--ink)"],
  ]));
});

test("keeps comment markers and intervening declarations inside quoted values", () => {
  const css = `${testSelector} { --font: "a/*b"; --ink: #123; --mark: '*/'; }`;
  expect(parseThemeDeclarations(css, testSelector)).toEqual(new Map([
    ["--font", '"a/*b"'],
    ["--ink", "#123"],
    ["--mark", "'*/'"],
  ]));
});

test.each([
  ":root[data-theme=test]",
  'html[data-theme="test"]',
  `${testSelector}, :root`,
])("rejects an extra rule with selector %s", (extraSelector) => {
  const css = `${testSelector} { --ink: #123; } ${extraSelector} { --ink: #456; }`;
  expect(() => parseThemeDeclarations(css, testSelector)).toThrow(`unexpected selector ${JSON.stringify(extraSelector)}`);
});

test("rejects a second rule with the same selector", () => {
  const css = `${testSelector} { --ink: #123; } ${testSelector} { --ink: #456; }`;
  expect(() => parseThemeDeclarations(css, testSelector)).toThrow(`${testSelector} must be the only rule`);
});

test("rejects imports before the theme rule", () => {
  const css = `@import "palette.css"; ${testSelector} { --ink: #123; }`;
  expect(() => parseThemeDeclarations(css, testSelector)).toThrow("Unexpected top-level statement");
});

test("rejects an important value that would bypass a later override", () => {
  const css = `${testSelector} { --ink: oklch(0.7 0.1 20) !important; --ink: #123; }`;
  expect(() => parseThemeDeclarations(css, testSelector)).toThrow(`Unsupported CSS "!" while reading ${testSelector}`);
});

test("rejects a bare ! in a value that would overwrite a valid declaration", () => {
  const css = `${testSelector} { --ink-1:#123; --ink-1:#456 ! x; }`;
  expect(() => parseThemeDeclarations(css, testSelector)).toThrow(`Unsupported CSS "!" while reading ${testSelector}`);
});

test("rejects an escaped custom-property name that would override a plain name", () => {
  const css = `${testSelector} { --ink-1: #123; --ink\\-1: #456; }`;
  expect(() => parseThemeDeclarations(css, testSelector)).toThrow(`Unsupported CSS escape while reading ${testSelector}`);
});

test.each([
  ["line feed", "\n"],
  ["carriage return", "\r"],
  ["form feed", "\f"],
])("rejects a string with a raw %s before a later quote", (_, newline) => {
  const css = `${testSelector} { --font: "broken${newline}; --ink-1: #456; --font: "fallback; }`;
  expect(() => parseThemeDeclarations(css, testSelector)).toThrow(`Unterminated CSS string while reading ${testSelector}`);
});

test("rejects a stray quote at the end of a value", () => {
  const css = `${testSelector} { --font: fallback'; }`;
  expect(() => parseThemeDeclarations(css, testSelector)).toThrow(`Unterminated CSS string while reading ${testSelector}`);
});

test.each([
  "url(a/*b*/c)",
  'URL("a")',
])("rejects %s before interpreting its contents", (value) => {
  const css = `${testSelector} { --image: ${value}; --ink-1: #123; }`;
  expect(() => parseThemeDeclarations(css, testSelector)).toThrow(`Unsupported CSS url() while reading ${testSelector}`);
});

test("rejects an escaped important annotation that would bypass a later override", () => {
  const css = `${testSelector} { --ink-1: #456 !\\important; --ink-1: #123; }`;
  expect(() => parseThemeDeclarations(css, testSelector)).toThrow(`Unsupported CSS "!" while reading ${testSelector}`);
});

test("rejects an unsupported declaration", () => {
  const css = `${testSelector} { --ink-1: #123; color: red; }`;
  expect(() => parseThemeDeclarations(css, testSelector)).toThrow(`Unsupported declaration "color: red" while reading ${testSelector}`);
});

test("rejects an unterminated comment", () => {
  const css = `${testSelector} { --ink-1: #123 /* }`;
  expect(() => parseThemeDeclarations(css, testSelector)).toThrow(`Unterminated CSS comment while reading ${testSelector}`);
});

test("keeps escaped quotes, url() text and exclamation marks inside strings", () => {
  const css = `${testSelector} { --font: "a\\"url(b)!"; --ink-1: #123; }`;
  expect(parseThemeDeclarations(css, testSelector).get("--font")).toBe('"a\\"url(b)!"');
});

test("ignores backslashes and exclamation marks inside comments", () => {
  const css = `${testSelector} { /* a\\ b */ /* ! */ --ink-1:#123; }`;
  expect(parseThemeDeclarations(css, testSelector).get("--ink-1")).toBe("#123");
});

test("rejects a theme selector inside a conditional rule", () => {
  const css = `@media (prefers-color-scheme: light) { ${testSelector} { --ink: #123; } }`;
  expect(() => parseThemeDeclarations(css, testSelector)).toThrow(`${testSelector} must be a top-level rule`);
});

test("rejects a nested block in a theme rule", () => {
  const css = `${testSelector} { --ink: #123; @supports (display: grid) { --ink: #456; } }`;
  expect(() => parseThemeDeclarations(css, testSelector)).toThrow(`${testSelector} must not contain nested blocks`);
});

test.each([
  ['"var(--ink)"', '"var(--ink)"'],
  ['"var(--absent)"', '"var(--absent)"'],
  ["'var(--ink)'", "'var(--ink)'"],
  ["'var(--absent)'", "'var(--absent)'"],
  ['"a\\"var(--ink)", var(--ink)', '"a\\"var(--ink)", #123'],
  ['"a\\\r\nvar(--ink)"', '"a\\\r\nvar(--ink)"'],
])("preserves quoted text when resolving %j", (value, expected) => {
  const declarations = parseThemeDeclarations(`:root { --ink: #123; --value: ${value}; }`, ":root");
  expect(resolveThemeDeclarations(declarations).get("--value")).toBe(expected);
});

test("preserves quoted text from a referenced declaration", () => {
  const declarations = parseThemeDeclarations(':root { --value: var(--label); --label: "var(--absent)"; }', ":root");
  expect(resolveThemeDeclarations(declarations).get("--value")).toBe('"var(--absent)"');
});

test.each([
  ["some-var(--ink)", "some-var(--ink)"],
  ["some-var(--absent)", "some-var(--absent)"],
  ["some-var(var(--ink))", "some-var(#123)"],
  ["some-var(--absent),var(--ink)", "some-var(--absent),#123"],
  ["some_var(--ink)", "some_var(--ink)"],
  ["name1var(--ink)", "name1var(--ink)"],
  ["évar(--ink)", "évar(--ink)"],
  ["#var(--ink)", "#var(--ink)"],
  ["@var(--ink)", "@var(--ink)"],
])("resolves only complete var() functions in %j", (value, expected) => {
  const declarations = parseThemeDeclarations(`:root { --ink: #123; --value: ${value}; }`, ":root");
  expect(resolveThemeDeclarations(declarations).get("--value")).toBe(expected);
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
  const missing = REQUIRED_THEME_TOKENS.filter((name) => !own.get(name) && !/^var\(\s*--[\w-]+\s*\)$/.test(base.get(name) ?? ""));
  expect(missing, `${id} declares every required token except shared aliases`).toEqual([]);

  const incompatible = [...own].filter(([, value]) => /\b(?:color-mix|oklch)\(/i.test(value)).map(([name]) => name);
  expect(incompatible, `${id} source uses renderer-compatible color syntax before the build`).toEqual([]);

  const resolved = resolveThemeDeclarations(new Map([...base, ...own]));
  expect(themeTokenProblems((name) => resolved.get(name) ?? "")).toEqual([]);
});
