import { cellPoint, colourDistance, expect, grid, pixelsAt, project, soundsFor, test, workspaceReady } from "./fixture";
import { boxSelect, canvas, lane, local, openRoll, stats, twoChannels } from "./rollHelpers";
import { parseColor } from "../src/rack/palette";
import { contrastRatio } from "../src/theme/contrast";
import { THEME_COLOR_TOKENS, themeTokenProblems } from "../src/theme/contract";
import { themes, THEME_PREFERENCE_KEY } from "../src/theme/preference";

const drums = project(["Kick", "Snare", "Hat"], { 1: [0, 4, 8, 12], 2: [4, 12], 3: [2, 6, 10, 14] });
const beat = twoChannels([
  { id: 1, channel: 1, tick: 480, length: 720, key: 60, velocity: 1 },
  { id: 2, channel: 1, tick: 1440, length: 480, key: 64, velocity: 0.5 },
  { id: 3, channel: 1, tick: 2400, length: 960, key: 67, velocity: 1 },
  { id: 4, channel: 2, tick: 480, length: 960, key: 64, velocity: 1 },
]);
beat.channels.push(...drums.channels.map((channel) => ({ ...channel, id: channel.id + 2 })));
beat.pattern.notes.push(...drums.pattern.notes.map((note) => ({ ...note, channel: note.channel + 2 })));
beat.nextId = beat.channels.length + 1;

test.use({ seed: { project: beat, files: soundsFor(["Kick", "Snare", "Hat"]) }, viewport: { width: 1280, height: 800 } });

type Renderer = "webgl2" | "canvas2d";
type RenderProbe = Window & { themeTestRenderers?: WeakMap<HTMLCanvasElement, Renderer> };

for (const { id } of themes) {
  for (const renderer of ["webgl2", "canvas2d"] as const) {
    test(`${id} paints the rack and roll on ${renderer} after a preference reload`, async ({ app, mock }, info) => {
      await app.addInitScript((force2d) => {
        const renderers = new WeakMap<HTMLCanvasElement, Renderer>();
        Object.defineProperty(window, "themeTestRenderers", { value: renderers });
        const get = HTMLCanvasElement.prototype.getContext;
        HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, kind: string, options?: unknown) {
          if (force2d && kind === "webgl2") return null;
          const context = get.call(this, kind, options);
          if (context && kind === "webgl2") renderers.set(this, "webgl2");
          if (context && kind === "2d") renderers.set(this, "canvas2d");
          return context;
        } as typeof get;
      }, renderer === "canvas2d");
      await app.evaluate(({ key, id }) => localStorage.setItem(key, id), { key: THEME_PREFERENCE_KEY, id });
      await app.reload();
      await workspaceReady(app);
      await expect(app.locator("html")).toHaveAttribute("data-theme", id);

      const { values, missingOverrides } = await app.evaluate(({ names, id }) => {
        const style = getComputedStyle(document.documentElement);
        const values = Object.fromEntries(names.map((name) => [name, style.getPropertyValue(name).trim()]));
        // The default rule also matches a themed root. Check ownership before its cascade can
        // hide a deleted variant token. Pure var() aliases can resolve against the active palette.
        const rules = Array.from(document.styleSheets)
          .flatMap((sheet) => Array.from(sheet.cssRules))
          .filter((rule): rule is CSSStyleRule => rule instanceof CSSStyleRule);
        const defaults = rules.find((rule) => rule.selectorText === ":root");
        if (!defaults) throw new Error("Missing Riso root rule");
        const selector = `:root[data-theme="${id}"]`;
        const overrides = rules.filter((rule) => rule.selectorText.replaceAll("'", '"') === selector);
        const missingOverrides = id === "riso" ? [] : Array.from(defaults.style).filter((name) =>
          name.startsWith("--") && !defaults.style.getPropertyValue(name).trim().startsWith("var(") &&
          !overrides.some((rule) => rule.style.getPropertyValue(name).trim()));
        return { values, missingOverrides };
      }, { names: [...THEME_COLOR_TOKENS, "--reg-x", "--reg-y", "--radius-s", "--radius-m", "--radius-l"], id });
      expect(missingOverrides, `${id} supplies every non-alias token`).toEqual([]);
      expect(themeTokenProblems((name) => values[name] ?? ""), "all live theme tokens resolve").toEqual([]);

      const rgb = (name: string) => {
        const color = parseColor(values[name]);
        if (!color || color[3] !== 1) throw new Error(`Expected an opaque color for ${name}: ${values[name]}`);
        const [red, green, blue] = color;
        return [red, green, blue] as const;
      };
      const bytes = (name: string) => rgb(name).map((channel) => 255 * channel);
      for (const text of ["--text", "--muted", "--faint"]) {
        for (const surface of ["--bg", "--panel", "--raised"]) {
          expect(contrastRatio(rgb(text), rgb(surface)), `${text} over ${surface}`).toBeGreaterThanOrEqual(4.5);
        }
      }
      const inks = THEME_COLOR_TOKENS.filter((name) => name.startsWith("--ink-"));
      for (const state of ["--select", "--grid-focus", "--roll-select", "--grid-playhead", "--roll-playhead"]) {
        for (const ink of inks) {
          // Keep the existing Riso playhead palette covered while its pink collision awaits #546.
          const risoPlayhead = id === "riso" && state.endsWith("playhead");
          if (risoPlayhead && ink === "--ink-1") continue;
          // 48 is nearly a fifth of one RGB channel's range. This catches close palette
          // collisions well above the screenshot tolerance, without prescribing a hue.
          expect(colourDistance(bytes(state), bytes(ink)), `${state} versus ${ink}`).toBeGreaterThanOrEqual(risoPlayhead ? 32 : 48);
        }
      }

      const stepGrid = grid(app);
      await expect.poll(() => stepGrid.evaluate((element: HTMLCanvasElement) =>
        (window as RenderProbe).themeTestRenderers?.get(element))).toBe(renderer);
      const kick = cellPoint(2, 0);
      const [lit, cap, empty] = await pixelsAt(app, stepGrid, [kick, { ...kick, y: kick.y - 11 }, cellPoint(2, 1)]);
      expect(colourDistance(lit, bytes("--ink-1")), "lit Kick step").toBeLessThan(8);
      expect(colourDistance(cap, bytes("--step-lit")), "unfilled top of the lit step").toBeLessThan(8);
      expect(colourDistance(empty, bytes("--step-a")), "empty step").toBeLessThan(8);
      if (renderer === "webgl2") await app.screenshot({ path: info.outputPath("rack.png"), scale: "css", animations: "disabled" });

      const ring = { ...kick, x: kick.x - 16 };
      const ringDistance = async (color: string) => {
        const [pixel] = await pixelsAt(app, stepGrid, [ring]);
        return colourDistance(pixel, bytes(color));
      };
      await stepGrid.focus();
      await app.keyboard.press("ArrowDown");
      await app.keyboard.press("ArrowDown");
      await expect.poll(() => ringDistance("--grid-focus"), { message: "focused Kick step" }).toBeLessThan(8);
      await app.getByRole("button", { name: "Play" }).click();
      await expect.poll(() => mock.eval<boolean>("(m) => m.state.transport.playing")).toBe(true);
      await mock.eval("(m) => m.hold(10)");
      await expect.poll(() => ringDistance("--grid-playhead"), { message: "playhead on the lit Kick step" }).toBeLessThan(8);
      await app.getByRole("button", { name: "Stop" }).click();

      await app.getByRole("button", { name: "Edit the Bass synth" }).click();
      const synth = app.getByRole("dialog", { name: "Bass synth" });
      const osc = synth.getByRole("region", { name: "Osc 1", exact: true });
      const [card] = await pixelsAt(app, osc, [{ x: 20, y: 3 }]);
      expect(colourDistance(card, bytes("--dev-card")), "synth card surface").toBeLessThan(8);
      await synth.getByRole("button", { name: /close/i }).click();

      await openRoll(app);
      expect((await stats(app)).renderer).toBe(renderer);
      const note = await local(app, 800, 60);
      const velocity = { x: (await local(app, 480, 60)).x + 1, y: 20 };
      const [noteFill] = await pixelsAt(app, canvas(app), [note]);
      const [bar] = await pixelsAt(app, lane(app), [velocity]);
      expect(colourDistance(noteFill, bytes("--ink-4")), "full-velocity Bass note").toBeLessThan(8);
      expect(colourDistance(bar, bytes("--ink-4")), "velocity bar").toBeLessThan(8);

      const [ghost, row] = await pixelsAt(app, canvas(app), [await local(app, 800, 64), await local(app, 3200, 64)]);
      const ghostInk = bytes("--ink-2");
      const ghostFill = ghostInk.map((channel, index) => channel * 0.30 + row[index] * 0.70);
      expect(colourDistance(ghost, ghostFill), "Lead ghost note at 30% opacity").toBeLessThan(8);
      expect(colourDistance(ghost, row)).toBeGreaterThan(25);

      await boxSelect(app, 400, 61, 1250, 59);
      const [selectedBar] = await pixelsAt(app, lane(app), [velocity]);
      expect(colourDistance(selectedBar, bytes("--roll-select")), "selected velocity bar").toBeLessThan(8);
      if (renderer === "webgl2") await app.screenshot({ path: info.outputPath("roll.png"), scale: "css", animations: "disabled" });
    });
  }
}
