import { cellPoint, clickCell, colourDistance, expect, grid, pixelsAt, project, soundsFor, test, workspaceReady } from "./fixture";
import { THEME_COLOR_TOKENS, themeTokenProblems } from "../src/theme/contract";
import { parseColor } from "../src/rack/palette";
import { token } from "./rollHelpers";

test.use({ seed: { project: project(["Kick"]), files: soundsFor(["Kick"]) }, viewport: { width: 900, height: 560 } });

test("the grid uses Riso ink when the startup stylesheet is slow", async ({ app, mock }) => {
  let delayedStylesheet = false;
  await app.route("**/assets/main-*.css", async (route) => {
    delayedStylesheet = true;
    await new Promise((resolve) => setTimeout(resolve, 1500));
    await route.continue();
  });

  await app.reload();
  await workspaceReady(app);
  expect(delayedStylesheet, "the built stylesheet was delayed").toBe(true);
  await expect(app.locator("html")).toHaveAttribute("data-theme", "riso");
  const rootStyle = await app.evaluate((names) => {
    const style = getComputedStyle(document.documentElement);
    return Object.fromEntries(names.map((name) => [name, style.getPropertyValue(name)]));
  }, [...THEME_COLOR_TOKENS, "--reg-x", "--reg-y", "--radius-s", "--radius-m", "--radius-l"]);
  expect(themeTokenProblems((name) => rootStyle[name] ?? "")).toEqual([]);

  await clickCell(app, 0, 1);
  await expect.poll(async () => (await mock.view()).pattern.cells["1"][1]).toBeGreaterThan(0);
  const ink = await token(app, "--ink-1");
  await expect.poll(async () => {
    const [pixel] = await pixelsAt(app, grid(app), [cellPoint(0, 1)]);
    return colourDistance(pixel, ink);
  }, { message: "the lit step uses the loaded ink" }).toBeLessThan(4);
});

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
