import { readFileSync } from "node:fs";
import { cellPoint, clickCell, colourDistance, expect, grid, pixelsAt, project, soundsFor, test, workspaceReady } from "./fixture";
import { REQUIRED_THEME_TOKENS, THEME_COLOR_TOKENS, themeTokenProblems } from "../src/theme/contract";
import { THEME_PREFERENCE_KEY } from "../src/theme/preference";
import { parseThemeDeclarations, resolveThemeDeclarations } from "../src/theme/themeSource";
import { parseColor } from "../src/rack/palette";

test.use({ seed: { project: project(["Kick"]), files: soundsFor(["Kick"]) }, viewport: { width: 900, height: 560 } });

const riso = parseThemeDeclarations(readFileSync(new URL("../src/theme/riso.css", import.meta.url), "utf8"), ":root");
const taste = parseThemeDeclarations(readFileSync(new URL("../src/theme/taste.css", import.meta.url), "utf8"), ':root[data-theme="taste"]');
const risoTokens = resolveThemeDeclarations(riso);
const tasteTokens = resolveThemeDeclarations(new Map([...riso, ...taste]));
// The CSS build can serialize source rgba() as 8-bit hex, quantizing the alpha channel.
const rgbaBytes = (value: string | undefined) => parseColor(value ?? "")?.map((channel) => Math.round(channel * 255));

for (const [id, name, tokens] of [["riso", "Riso", risoTokens], ["taste", "Taste", tasteTokens]] as const) {
  test(`the grid uses ${name} ink when the startup stylesheet is slow`, async ({ app, mock }) => {
    if (id === "taste") {
      await app.addInitScript(({ key, id }) => localStorage.setItem(key, id), { key: THEME_PREFERENCE_KEY, id });
    }

    let delayedStylesheet = false;
    // Every built sheet: the tokens sit in a chunk shared with the other entries.
    await app.route("**/assets/*.css", async (route) => {
      delayedStylesheet = true;
      await new Promise((resolve) => setTimeout(resolve, 1500));
      await route.continue();
    });

    await app.reload();
    await workspaceReady(app);
    expect(delayedStylesheet, "the built stylesheets were delayed").toBe(true);
    await expect(app.locator("html")).toHaveAttribute("data-theme", id);
    const rootStyle = await app.evaluate((names) => {
      const style = getComputedStyle(document.documentElement);
      return Object.fromEntries(names.map((name) => [name, style.getPropertyValue(name)]));
    }, REQUIRED_THEME_TOKENS);
    expect(themeTokenProblems((name) => rootStyle[name] ?? "")).toEqual([]);
    for (const color of THEME_COLOR_TOKENS) {
      expect(rgbaBytes(rootStyle[color]), `${name} supplies ${color}`).toEqual(rgbaBytes(tokens.get(color)));
    }
    if (id === "taste") {
      expect(THEME_COLOR_TOKENS.map((color) => rgbaBytes(rootStyle[color])), "the Taste palette differs from Riso")
        .not.toEqual(THEME_COLOR_TOKENS.map((color) => rgbaBytes(risoTokens.get(color))));
    }

    await clickCell(app, 0, 1);
    await expect.poll(async () => (await mock.view()).pattern.cells["1"][1]).toBeGreaterThan(0);
    const ink = rgbaBytes(tokens.get("--ink-1"))!.slice(0, 3);
    await expect.poll(async () => {
      const [pixel] = await pixelsAt(app, grid(app), [cellPoint(0, 1)]);
      return colourDistance(pixel, ink);
    }, { message: `the lit step uses ${name} ink` }).toBeLessThan(4);
  });
}

for (const preference of ["absent", "invalid", "unavailable", "denied"] as const) {
  test(`Riso startup and editing survive ${preference} theme storage`, async ({ app, mock }, info) => {
    await app.addInitScript((kind) => {
      if (kind === "denied") {
        Object.defineProperty(window, "localStorage", { get: () => { throw new DOMException("Blocked", "SecurityError"); } });
      } else if (kind === "absent") {
        localStorage.removeItem("grooviq.theme");
      } else {
        localStorage.setItem("grooviq.theme", kind === "invalid" ? '{"theme":"riso"}' : "future-theme");
      }
    }, preference);
    await app.reload();
    await workspaceReady(app);
    await app.evaluate(() => document.fonts.ready);
    await expect(app.locator("html")).toHaveAttribute("data-theme", "riso");
    const colors = await app.evaluate((names) => {
      const style = getComputedStyle(document.documentElement);
      return Object.fromEntries(names.map((name) => [name, style.getPropertyValue(name).trim()]));
    }, THEME_COLOR_TOKENS);
    expect(Object.entries(colors).filter(([, value]) => parseColor(value) === null), "all renderer role colors resolve").toEqual([]);
    expect(colors["--bg"]).toBe("#0e1127");
    expect(colors["--roll-select"]).toBe("#f3eee4");
    expect(await app.evaluate(() => document.fonts.check("13px 'Manrope Variable'"))).toBe(true);
    if (preference !== "denied") {
      expect(await app.evaluate(() => localStorage.getItem("grooviq.theme")))
        .toBe(preference === "absent" ? null : preference === "invalid" ? '{"theme":"riso"}' : "future-theme");
    }
    const notesBefore = (await mock.view()).pattern.cells;
    await clickCell(app, 0, 1);
    await expect.poll(async () => (await mock.view()).pattern.cells["1"][1]).toBeGreaterThan(0);
    await app.keyboard.press("ControlOrMeta+z");
    await expect.poll(async () => (await mock.view()).pattern.cells).toEqual(notesBefore);
    await info.attach("riso-after-theme-fallback", { body: await app.screenshot({ scale: "css", animations: "disabled" }), contentType: "image/png" });
  });
}
