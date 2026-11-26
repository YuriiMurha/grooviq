import { cellPoint, colourDistance, expect, grid, pixelsAt, project, soundsFor, test, workspaceReady } from "./fixture";
import { boxSelect, canvas, lane, local, openRoll, stats, twoChannels } from "./rollHelpers";
import { parseColor } from "../src/rack/palette";
import { contrastRatio, oklabDeltaE } from "../src/theme/contrast";
import { REQUIRED_THEME_TOKENS, THEME_COLOR_TOKENS, themeTokenProblems } from "../src/theme/contract";
import { themes, THEME_PREFERENCE_KEY } from "../src/theme/preference";
import { roundedVelocityFills } from "../src/theme/velocityFills";

const drums = project(["Kick", "Snare", "Hat"], { 1: [0, 4, 8, 12], 2: [4, 12], 3: [2, 6, 10, 14] });
const beat = twoChannels([
  { id: 1, channel: 1, tick: 480, length: 720, key: 60, velocity: 1 },
  { id: 2, channel: 1, tick: 1440, length: 480, key: 64, velocity: 0.5 },
  { id: 3, channel: 1, tick: 2400, length: 960, key: 67, velocity: 1, slide: true },
  { id: 4, channel: 2, tick: 480, length: 960, key: 64, velocity: 1 },
]);
beat.channels.push(...drums.channels.map((channel) => ({ ...channel, id: channel.id + 2 })));
beat.pattern.notes.push(...drums.pattern.notes.map((note) => ({ ...note, channel: note.channel + 2 })));
beat.nextId = beat.channels.length + 1;

test.use({ seed: { project: beat, files: soundsFor(["Kick", "Snare", "Hat"]) }, viewport: { width: 1280, height: 800 } });

type Renderer = "webgl2" | "canvas2d";
type RenderProbe = Window & { themeTestRenderers?: WeakMap<HTMLCanvasElement, Renderer> };

const quietNoteStrength = 0.45;

function themeRgb(values: Record<string, string>, name: string) {
  const color = parseColor(values[name] ?? "");
  if (!color || color[3] !== 1) throw new Error(`Expected an opaque color for ${name}: ${values[name]}`);
  const [red, green, blue] = color;
  return [red, green, blue] as const;
}

function nearestVelocityShadeDistance(primary: number[], slide: number[]) {
  // Both renderers scale the primary RGB from 45% to 100%. Project onto that segment.
  const lengthSquared = primary.reduce((sum, channel) => sum + channel * channel, 0);
  const projection = primary.reduce((sum, channel, index) => sum + channel * slide[index], 0);
  const strength = lengthSquared === 0 ? 1 : Math.max(quietNoteStrength, Math.min(1, projection / lengthSquared));
  return colourDistance(primary.map((channel) => channel * strength), slide);
}

for (const { id } of themes) {
  test(`${id} paints Solo consistently in the rack and mixer`, async ({ app }) => {
    await app.evaluate(({ key, id }) => localStorage.setItem(key, id), { key: THEME_PREFERENCE_KEY, id });
    await app.reload();
    await workspaceReady(app);
    await expect(app.locator("html")).toHaveAttribute("data-theme", id);

    const values = await app.locator("html").evaluate((root) => {
      const style = getComputedStyle(root);
      return Object.fromEntries(["--solo", "--mute", "--ok", "--warn"].map((name) => [name, style.getPropertyValue(name)]));
    });
    const bytes = (name: string) => themeRgb(values, name).map((channel) => 255 * channel);
    const otherRoles = id === "slate-system" ? ["--mute", "--ok", "--warn"] : ["--mute", "--ok"];
    for (const other of otherRoles) {
      expect(colourDistance(bytes("--solo"), bytes(other)), `--solo versus ${other}`).toBeGreaterThanOrEqual(48);
    }
    const soloColor = `rgb(${bytes("--solo").map(Math.round).join(", ")})`;

    const rackSolo = app.getByRole("button", { name: "Solo Kick", exact: true });
    await rackSolo.click();
    await expect(rackSolo).toHaveAttribute("aria-pressed", "true");
    await expect(rackSolo).toHaveCSS("border-top-color", soloColor);
    await expect(rackSolo).toHaveCSS("background-color", soloColor);

    await app.goto("/mixer-fixture.html");
    await expect(app.locator("html")).toHaveAttribute("data-theme", id);
    const mixerSolo = app.getByRole("button", { name: "Solo Kick", exact: true });
    await mixerSolo.click();
    await expect(mixerSolo).toHaveAttribute("aria-pressed", "true");
    await expect(mixerSolo).toHaveCSS("border-top-color", soloColor);
    // Let each browser serialize the sRGB mix used for the mixer button's tinted fill.
    const tintedFill = await app.evaluate((solo) => {
      const probe = document.createElement("span");
      probe.style.backgroundColor = `color-mix(in srgb, ${solo} 17%, var(--raised))`;
      document.body.append(probe);
      const fill = getComputedStyle(probe).backgroundColor;
      probe.remove();
      return fill;
    }, soloColor);
    await expect(mixerSolo).toHaveCSS("background-color", tintedFill);

    const mixerMute = app.getByRole("button", { name: "Mute Kick", exact: true });
    const muteColor = `rgb(${bytes("--mute").map(Math.round).join(", ")})`;
    await mixerMute.hover();
    await expect(mixerMute).not.toHaveCSS("border-top-color", muteColor);
    await mixerMute.click();
    await expect(mixerMute).toHaveAttribute("aria-pressed", "true");
    await expect(mixerMute).toHaveCSS("border-top-color", muteColor);
  });

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
      }, { names: REQUIRED_THEME_TOKENS, id });
      expect(missingOverrides, `${id} supplies every non-alias token`).toEqual([]);
      expect(themeTokenProblems((name) => values[name] ?? ""), "all live theme tokens resolve").toEqual([]);

      const rgb = (name: string) => themeRgb(values, name);
      const bytes = (name: string) => rgb(name).map((channel) => 255 * channel);
      const inks = THEME_COLOR_TOKENS.filter((name) => name.startsWith("--ink-"));
      for (const ink of inks.filter((name) => name.endsWith("b"))) {
        for (const row of ["--roll-row", "--roll-row-black"]) {
          expect(contrastRatio(rgb(ink), rgb(row)), `${ink} over ${row}`).toBeGreaterThanOrEqual(3);
        }
        const primary = ink.slice(0, -1);
        if (id === "taste") {
          // Taste's shade-only slide inks still collide with normal velocities. See #642.
          expect(colourDistance(bytes(ink), bytes(primary)), `${ink} versus ${primary}`).toBeGreaterThanOrEqual(48);
        } else {
          expect(nearestVelocityShadeDistance(bytes(primary), bytes(ink)),
            `${ink} versus the closest velocity shade of ${primary}`).toBeGreaterThanOrEqual(48);
        }
      }
      expect(contrastRatio(rgb("--on-accent"), rgb("--accent")), "accent foreground").toBeGreaterThanOrEqual(4.5);
      expect(contrastRatio(rgb("--on-play"), rgb("--play")), "Play foreground").toBeGreaterThanOrEqual(4.5);
      expect(colourDistance(bytes("--play"), bytes("--rec")), "Play versus Record").toBeGreaterThanOrEqual(48);
      if (id === "console") {
        expect(colourDistance(bytes("--play"), bytes("--accent")), "Console Play versus primary accent").toBeGreaterThanOrEqual(48);
      }
      const sectionInks = ["--dev-osc1", "--dev-osc2", "--dev-filter", "--dev-amp", "--dev-lfo", "--dev-mix", "--dev-voice"];
      const cardColor = rgb("--dev-card");
      for (const [index, ink] of sectionInks.entries()) {
        expect(contrastRatio(rgb(ink), cardColor), `${ink} caption over --dev-card`).toBeGreaterThanOrEqual(4.5);
        for (const other of sectionInks.slice(index + 1)) {
          const distance = colourDistance(bytes(ink), bytes(other));
          const deltaE = oklabDeltaE(rgb(ink), rgb(other));
          expect(distance >= 48 || deltaE >= 0.10,
            `${ink} versus ${other}: ${distance.toFixed(2)} RGB, ${deltaE.toFixed(4)} OKLab`).toBe(true);
        }
      }
      const play = app.getByRole("button", { name: "Play", exact: true });
      const [playFill] = await pixelsAt(app, play, [{ x: 8, y: 18 }]);
      expect(colourDistance(playFill, bytes("--play")), "Play button fill").toBeLessThan(8);
      for (const text of ["--text", "--muted", "--faint", "--error", "--warn"]) {
        for (const surface of ["--bg", "--panel", "--raised"]) {
          // Riso retains its existing Bright Red at 4.40:1 on raised controls. See #634.
          if (id === "riso" && text === "--error" && surface === "--raised") continue;
          expect(contrastRatio(rgb(text), rgb(surface)), `${text} over ${surface}`).toBeGreaterThanOrEqual(4.5);
        }
      }
      for (const surface of ["--well", "--line-soft"]) {
        expect(contrastRatio(rgb("--error"), rgb(surface)), `meter clip indicator over ${surface}`).toBeGreaterThanOrEqual(3);
      }
      expect(colourDistance(bytes("--mute"), bytes("--ok")), "mute versus on").toBeGreaterThanOrEqual(48);
      for (const status of ["--ok", "--error", "--select"]) {
        expect(colourDistance(bytes("--warn"), bytes(status)), `--warn versus ${status}`).toBeGreaterThanOrEqual(48);
      }
      if (id === "luminous") {
        for (const warning of ["--ink-3", "--ink-3b"]) {
          expect(colourDistance(bytes("--error"), bytes(warning)), `error versus ${warning}`).toBeGreaterThanOrEqual(48);
        }
      }
      for (const marker of ["--roll-key-held", "--roll-key-scale"]) {
        for (const key of ["--roll-key-white", "--roll-key-black"]) {
          if (id === "luminous") {
            expect(contrastRatio(rgb(marker), rgb(key)), `${marker} over ${key}`).toBeGreaterThanOrEqual(3);
          } else {
            // Riso pink and Taste butter remain distinct from the neutral keys by chroma.
            expect(colourDistance(bytes(marker), bytes(key)), `${marker} versus ${key}`).toBeGreaterThanOrEqual(90);
          }
        }
      }
      for (const state of ["--select", "--grid-focus", "--roll-select"]) {
        const outline = rgb(state);
        // Slide notes (second inks) are velocity-scaled the same way as normal notes.
        for (const ink of inks) {
          const nearest = Math.min(...roundedVelocityFills(rgb(ink), quietNoteStrength, renderer).map(([r, g, b]) =>
            oklabDeltaE(outline, [r / 255, g / 255, b / 255])));
          expect(nearest, `${state} versus ${ink} velocity shades in OKLab`).toBeGreaterThanOrEqual(0.10);
        }
      }
      for (const state of ["--grid-playhead", "--roll-playhead"]) {
        for (const ink of inks) {
          // Keep the existing Riso playhead palette covered while its pink collision awaits #546.
          if (id === "riso" && ink === "--ink-1") continue;
          expect(colourDistance(bytes(state), bytes(ink)), `${state} versus ${ink}`).toBeGreaterThanOrEqual(id === "riso" ? 32 : 48);
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
      if (id !== "riso") {
        expect(values["--reg-x"]).toBe("0px");
        expect(values["--reg-y"]).toBe("0px");
        const [right, below, clearRight, clearBelow] = await pixelsAt(app, stepGrid, [
          { ...kick, x: kick.x + 15 },
          { ...kick, y: kick.y + 15 },
          { ...cellPoint(2, 1), x: cellPoint(2, 1).x + 15 },
          { ...cellPoint(2, 1), y: cellPoint(2, 1).y + 15 },
        ]);
        expect(colourDistance(right, clearRight), "no second ink in the gap to the right").toBeLessThan(8);
        expect(colourDistance(below, clearBelow), "no second ink in the gap below").toBeLessThan(8);
      }
      await app.screenshot({ path: info.outputPath("rack.png"), scale: "css", animations: "disabled" });

      const ring = { ...kick, x: kick.x - 16 };
      const ringDistance = async (color: string) => {
        const [pixel] = await pixelsAt(app, stepGrid, [ring]);
        return colourDistance(pixel, bytes(color));
      };
      await stepGrid.focus();
      await app.keyboard.press("ArrowDown");
      await app.keyboard.press("ArrowDown");
      await expect.poll(() => ringDistance("--grid-focus"), { message: "focused Kick step" }).toBeLessThan(8);
      await play.click();
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
      const [noteFill, slideFill, halfVelocityFill] = await pixelsAt(app, canvas(app), [
        note, await local(app, 2800, 67), await local(app, 1650, 64),
      ]);
      const [bar] = await pixelsAt(app, lane(app), [velocity]);
      expect(colourDistance(noteFill, bytes("--ink-4")), "full-velocity Bass note").toBeLessThan(8);
      expect(colourDistance(slideFill, bytes("--ink-4b")), "full-velocity Bass slide note").toBeLessThan(8);
      const halfVelocityInk = bytes("--ink-4").map((channel) => channel * (1 + quietNoteStrength) / 2);
      expect(colourDistance(halfVelocityFill, halfVelocityInk), "half-velocity Bass note").toBeLessThan(8);
      // Taste's shade-only slide ink can read as a quieter normal note. See #642.
      if (id !== "taste") {
        expect(colourDistance(slideFill, halfVelocityFill), "slide versus half-velocity Bass note").toBeGreaterThanOrEqual(48);
      }
      expect(colourDistance(bar, bytes("--ink-4")), "velocity bar").toBeLessThan(8);

      const [ghost, row] = await pixelsAt(app, canvas(app), [await local(app, 800, 64), await local(app, 3200, 64)]);
      const ghostInk = bytes("--ink-2");
      const ghostFill = ghostInk.map((channel, index) => channel * 0.30 + row[index] * 0.70);
      expect(colourDistance(ghost, ghostFill), "Lead ghost note at 30% opacity").toBeLessThan(8);
      expect(colourDistance(ghost, row)).toBeGreaterThan(25);

      await boxSelect(app, 400, 61, 1250, 59);
      const [selectedBar] = await pixelsAt(app, lane(app), [velocity]);
      expect(colourDistance(selectedBar, bytes("--roll-select")), "selected velocity bar").toBeLessThan(8);
      if (renderer === "webgl2") {
        await app.screenshot({ path: info.outputPath("roll.png"), scale: "css", animations: "disabled" });
        await app.getByRole("combobox", { name: "Key", exact: true }).selectOption({ label: "C" });
        await expect(app.getByRole("combobox", { name: "Scale", exact: true })).toHaveValue("major");
        const warning = app.getByText("Unsaved changes", { exact: true });
        await expect(warning).toHaveCSS("color", `rgb(${bytes("--warn").map(Math.round).join(", ")})`);
        await expect.poll(async () => (await stats(app)).scaleMask).toBe(0b101010110101);
        const keys = app.getByRole("group", { name: "Piano keys" });
        for (const [key, name] of [[60, "root C4"], [62, "in-scale D4"]] as const) {
          const [marker] = await pixelsAt(app, keys.locator(`[data-key="${key}"]`), [{ x: 1, y: 5 }]);
          expect(colourDistance(marker, bytes("--roll-key-scale")), `${name} scale stripe`).toBeLessThan(8);
        }
        await app.screenshot({ path: info.outputPath("roll-c-major.png"), scale: "css", animations: "disabled" });
      }
    });
  }
}
