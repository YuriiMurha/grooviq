import type { Locator, Page } from "@playwright/test";
import { FILTER_BP, FILTER_LP24 } from "../src/devices/synth/waveShapes";
import { contrastRatio } from "../src/theme/contrast";
import { expect, presetValues, project, test } from "./fixture";

const sampleName = "Warehouse room microphone — long percussion take 037 — alternate transient edit.wav";
const samplePath = `/qa/sounds/${sampleName}`;
const beat = project(["Percussion", "Missing", "Bass"]);

// These values exercise formats that can be wider away from the endpoints: note names with an
// accidental and a signed octave, fractional semitones, and the Hz/ms unit changes.
const wideTimes = { attack: 99.9, decay: 99.9, release: 99.9 };
beat.channels[0].sample = samplePath;
beat.channels[0].sampler = {
  root: 10, tune: -23.9, cutoff: 9990, glide: 99.9, ...wideTimes,
  flt_attack: 99.9, flt_decay: 99.9, flt_release: 99.9,
};
beat.channels[1].embedded = false;
beat.channels[1].sampler = { ...beat.channels[0].sampler };
Object.assign(beat.channels[2], {
  kind: "synth", sample: null, preset: "808 Glide",
  synth: {
    ...presetValues("808 Glide"), osc1_shape: 0.5, osc2_shape: 2.5, filter_cutoff: 9990, glide: 99.9,
    amp_attack: 99.9, amp_decay: 99.9, amp_release: 99.9,
    flt_attack: 99.9, flt_decay: 99.9, flt_release: 99.9,
  },
});
test.use({ seed: { project: beat, files: { [samplePath]: { kind: "sample" } } } });

async function openEditor(page: Page, name: string): Promise<Locator> {
  await page.getByRole("button", { name: `Edit the ${name}`, exact: true }).click();
  const dialog = page.getByRole("dialog", { name, exact: true });
  await expect(dialog).toBeVisible();
  const fontLoaded = await page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    return document.fonts.check('11px "Manrope Variable"') &&
      [...document.fonts].some((face) => face.family.replaceAll('"', "") === "Manrope Variable" && face.status === "loaded");
  });
  expect(fontLoaded, "bundled Manrope is loaded").toBe(true);
  if (page.viewportSize()?.width === 900) {
    expect(await dialog.evaluate((el) => el.getBoundingClientRect().width / el.clientWidth), "minimum window scales the device").toBeLessThan(1);
  }
  return dialog;
}

async function inspectTextVisibility(dialog: Locator) {
  return dialog.evaluate((root) => {
    type Rgb = readonly [number, number, number];
    type Rgba = readonly [number, number, number, number];

    const tolerance = 1;
    const issues: string[] = [];
    const colours: { selector: string; foreground: Rgb; background: Rgb }[] = [];
    let textCount = 0;
    let pointCount = 0;
    const styles = new Map<Element, CSSStyleDeclaration>();
    const styleOf = (el: Element) => {
      const cached = styles.get(el);
      if (cached) return cached;
      const style = getComputedStyle(el);
      styles.set(el, style);
      return style;
    };
    // Let the browser resolve computed RGB and color(srgb …) from color-mix alike.
    const canvas = document.createElement("canvas");
    canvas.width = canvas.height = 1;
    const context = canvas.getContext("2d", { willReadFrequently: true });
    if (!context) throw new Error("Device text contrast needs a 2D canvas");
    context.globalCompositeOperation = "copy";
    const parsedColours = new Map<string, Rgba>();
    const rgba = (value: string): Rgba => {
      const cached = parsedColours.get(value);
      if (cached) return cached;
      context.fillStyle = "transparent";
      context.fillStyle = value;
      context.fillRect(0, 0, 1, 1);
      const [red, green, blue, alpha] = context.getImageData(0, 0, 1, 1).data;
      const colour = [red / 255, green / 255, blue / 255, alpha / 255] as const;
      parsedColours.set(value, colour);
      return colour;
    };
    const over = ([red, green, blue, alpha]: Rgba, background: Rgb): Rgb => [
      red * alpha + background[0] * (1 - alpha),
      green * alpha + background[1] * (1 - alpha),
      blue * alpha + background[2] * (1 - alpha),
    ];
    const backgrounds = new Map<Element, Rgb>();
    const backgroundOf = (el: Element | null): Rgb => {
      if (!el) return [1, 1, 1]; // The browser's default white canvas.
      const cached = backgrounds.get(el);
      if (cached) return cached;
      const style = styleOf(el);
      const solid = rgba(style.backgroundColor);
      // The chart and waveform wells use a subtle radial gradient over the well colour.
      // Use its base colour for contrast. Background images and gradient stops are unsampled.
      const layer = solid[3] === 0 && el.matches(".dwell") && style.backgroundImage !== "none"
        ? rgba(style.getPropertyValue("--well").trim())
        : solid;
      const background: Rgb = layer[3] === 1
        ? [layer[0], layer[1], layer[2]]
        : over(layer, backgroundOf(el.parentElement));
      backgrounds.set(el, background);
      return background;
    };
    const describe = (el: Element) => {
      const param = el.closest("[data-param]");
      const classes = [...el.classList].map((name) => `.${name}`).join("");
      return `${param ? `[data-param="${param.getAttribute("data-param")}"] ` : ""}${el.localName}${classes}`;
    };
    const pixels = (width: number) => `${width.toFixed(1)}px`;
    const spans = (rect: DOMRect, box: DOMRect) => {
      const edges = [
        ["left", box.left - rect.left], ["right", rect.right - box.right],
        ["top", box.top - rect.top], ["bottom", rect.bottom - box.bottom],
      ] as const;
      const overhang = edges.filter(([, distance]) => distance > tolerance)
        .map(([edge, distance]) => `${pixels(distance)} past ${edge}`).join(", ");
      return `${pixels(rect.width)} text vs ${pixels(box.width)} box${overhang ? `, ${overhang}` : ""}`;
    };

    const samplePoints = [[0.1, 0.5], [0.5, 0.5], [0.9, 0.5], [0.5, 0.25], [0.5, 0.75]];
    // The collapsed File details panel can retain layout boxes in Chromium.
    const inClosedDetails = (el: Element) => el.closest("details:not([open])") && !el.closest("summary");
    const controls = [...root.querySelectorAll(".dctl, button, [role=radio], [role=slider], summary")]
      .filter((el) => !inClosedDetails(el) && styleOf(el).visibility === "visible")
      .map((el) => ({ el, box: el.getBoundingClientRect() }))
      .filter(({ box }) => box.width > 0 && box.height > 0);
    const checkVisibility = (el: Element, selector: string, rects: DOMRect[]) => {
      textCount++;
      let opacity = 1;
      for (let bound: Element | null = el; bound && root.contains(bound); bound = bound.parentElement) {
        const css = styleOf(bound);
        opacity *= Number(css.opacity);
        const mask = css.maskImage || css.getPropertyValue("-webkit-mask-image");
        if (css.clipPath !== "none") issues.push(`${selector}: clip-path on ${describe(bound)} (${css.clipPath})`);
        if (mask && mask !== "none") issues.push(`${selector}: mask on ${describe(bound)} (${mask})`);
      }
      // A 0.4 floor leaves room for deliberately dimmed off states, which still need 3:1 contrast.
      if (opacity < 0.4) issues.push(`${selector}: effective opacity ${opacity.toFixed(3)} below 0.4`);
      const style = styleOf(el);
      if (style.visibility !== "visible") {
        issues.push(`${selector}: visibility is ${style.visibility}`);
        return;
      }
      const svg = el instanceof SVGTextElement ? el.ownerSVGElement : null;
      const [red, green, blue, alpha] = rgba(svg && style.fill !== "currentcolor" ? style.fill : style.color);
      const ink: Rgba = [red, green, blue, alpha * (svg ? Number(style.fillOpacity) : 1)];
      const background = backgroundOf(el);
      const chartFills = new Set<Element>();
      let sampledPlainBackground = !svg;
      // Shell overlays (e.g. the QA toast) are outside this editor.
      const inEditor = (candidate: Element) => root.contains(candidate);
      const neighbours = new Set<Element>();
      const occluders = new Set<Element | null>();
      for (const rect of rects) {
        if (rect.width <= 0 || rect.height <= 0) continue;
        for (const { el: control, box } of controls) {
          const width = Math.min(rect.right, box.right) - Math.max(rect.left, box.left);
          const height = Math.min(rect.bottom, box.bottom) - Math.max(rect.top, box.top);
          if (width > tolerance && height > tolerance && !control.contains(el)) neighbours.add(control);
        }

        // Sample inside the text, away from glyph-box edges and inter-line gaps.
        for (const [across, down] of samplePoints) {
          const x = rect.left + rect.width * across;
          const y = rect.top + rect.height * down;
          const stack = svg ? document.elementsFromPoint(x, y) : null;
          let hit = stack ? stack[0] ?? null : document.elementFromPoint(x, y);
          pointCount++;
          if (hit && !inEditor(hit)) hit = (stack ?? document.elementsFromPoint(x, y)).find(inEditor) ?? null;
          if (!el.contains(hit)) occluders.add(hit);
          if (svg && stack) {
            // A chart fill can be behind only part of a label. Check both sampled surfaces.
            const fill = stack.find((candidate) => candidate.matches(".dw-fill") &&
              candidate.closest("svg") === svg &&
              (el.compareDocumentPosition(candidate) & Node.DOCUMENT_POSITION_PRECEDING));
            if (fill) chartFills.add(fill);
            else sampledPlainBackground = true;
          }
        }
      }
      if (sampledPlainBackground) colours.push({ selector, foreground: over(ink, background), background });
      for (const fill of chartFills) {
        const css = styleOf(fill);
        const [fillRed, fillGreen, fillBlue, fillAlpha] = rgba(css.fill);
        const painted = over([fillRed, fillGreen, fillBlue, fillAlpha * Number(css.fillOpacity) * Number(css.opacity)], background);
        colours.push({ selector, foreground: over(ink, painted), background: painted });
      }
      for (const control of neighbours) issues.push(`${selector}: overlaps ${describe(control)}`);
      for (const hit of occluders) issues.push(`${selector}: occluded by ${hit ? describe(hit) : "the viewport edge"}`);
    };

    const checkText = (el: Element) => {
      if (el.getClientRects().length === 0 || inClosedDetails(el)) return;
      const text = (el instanceof SVGTextElement ? el.textContent : [...el.childNodes]
        .filter((node) => node.nodeType === Node.TEXT_NODE).map((node) => node.textContent).join(" "))
        ?.replace(/\s+/g, " ").trim();
      if (!text) return;
      const selector = `${describe(el)} ${JSON.stringify(text)}`;

      if (el instanceof SVGTextElement && el.ownerSVGElement) {
        // Both rects use viewport coordinates, so SVG transforms and device scale agree.
        const textBox = el.getBoundingClientRect();
        const viewport = el.ownerSVGElement.getBoundingClientRect();
        if (textBox.left < viewport.left - tolerance || textBox.right > viewport.right + tolerance ||
            textBox.top < viewport.top - tolerance || textBox.bottom > viewport.bottom + tolerance) {
          issues.push(`${selector}: outside SVG (${spans(textBox, viewport)})`);
        }
        checkVisibility(el, selector, [textBox]);
        return;
      }
      if (!(el instanceof HTMLElement)) return;
      const style = styleOf(el);

      const rects = [...el.childNodes].flatMap((node) => {
        if (node.nodeType !== Node.TEXT_NODE || !node.textContent?.trim()) return [];
        const range = document.createRange();
        range.selectNodeContents(node);
        return [...range.getClientRects()].filter((rect) => rect.width > 0 && rect.height > 0);
      });
      if (rects.length === 0) return;

      const overflowsWidth = (rect: DOMRect, box: DOMRect) =>
        rect.left < box.left - tolerance || rect.right > box.right + tolerance;
      const box = el.getBoundingClientRect();
      const scrolls = el.clientWidth > 0 && el.scrollWidth > el.clientWidth + tolerance;
      const ellipsis = style.textOverflow === "ellipsis" && style.overflowX !== "visible" &&
        (scrolls || rects.some((rect) => overflowsWidth(rect, box)));
      if ((scrolls || ellipsis) && !(ellipsis && el.title.replace(/\s+/g, " ").includes(text))) {
        const missingTitle = ellipsis ? "; ellipsis has no full title" : "";
        issues.push(`${selector}: scrollWidth ${el.scrollWidth}px vs clientWidth ${el.clientWidth}px${missingTitle}`);
      }

      // Range includes characters hidden by ellipsis. Restrict neighbour checks to the drawn span.
      const visibleRects = ellipsis ? rects.map((rect) => {
        const left = Math.max(rect.left, box.left);
        return new DOMRect(left, rect.top, Math.min(rect.right, box.right) - left, rect.height);
      }) : rects;
      checkVisibility(el, selector, visibleRects);
      // A nowrap span can grow to fit itself and still spill out of its control or grid cell.
      // Positioned or transformed text can cross its control without affecting layout widths.
      const overflowRects = ellipsis ? [box] : rects;
      const control = el.closest(".dctl");
      for (let bound: Element | null = el; bound && root.contains(bound); bound = bound.parentElement) {
        const css = styleOf(bound);
        const bounds = bound.getBoundingClientRect();
        const clips = (overflow: string) => overflow === "hidden" || overflow === "clip";
        const horizontal = bounds.width > 0 && css.display !== "inline";
        const vertical = bound === control || clips(css.overflowY);
        const overflow = overflowRects.find((rect) => (horizontal && overflowsWidth(rect, bounds)) ||
          (vertical && (rect.top < bounds.top - tolerance || rect.bottom > bounds.bottom + tolerance)));
        if (overflow) {
          const edges = DOMRect.fromRect({
            x: horizontal ? overflow.x : bounds.x,
            y: vertical ? overflow.y : bounds.y,
            width: horizontal ? overflow.width : bounds.width,
            height: vertical ? overflow.height : bounds.height,
          });
          issues.push(`${selector}: outside ${describe(bound)} (${spans(edges, bounds)})`);
          break;
        }
      }
    };

    // Decorative overlays can paint over captions even when pointer events pass through them.
    const hitTestStyle = document.createElement("style");
    hitTestStyle.textContent = "*, *::before, *::after { pointer-events: auto !important; }";
    document.head.append(hitTestStyle);
    try {
      root.querySelectorAll("*").forEach(checkText);
      return { issues, colours, textCount, controlCount: controls.length, pointCount };
    } finally {
      hitTestStyle.remove();
    }
  });
}

async function measure(dialog: Locator, state: string, findings: Set<string>) {
  const { issues, colours, textCount, controlCount, pointCount } = await inspectTextVisibility(dialog);
  expect(textCount, `${state} | text elements measured`).toBeGreaterThan(0);
  expect(controlCount, `${state} | interactive controls measured`).toBeGreaterThan(0);
  expect(pointCount, `${state} | occlusion points sampled`).toBeGreaterThan(0);
  for (const issue of issues) findings.add(`${state} | ${issue}`);
  // A coarse visibility floor. The theme contrast suite checks the full text contrast targets.
  for (const { selector, foreground, background } of colours) {
    const ratio = contrastRatio(foreground, background);
    if (ratio < 3) findings.add(`${state} | ${selector}: text contrast ${ratio.toFixed(2)}:1 below 3:1`);
  }
}

async function measureExtremes(dialog: Locator, state: string, findings: Set<string>) {
  await measure(dialog, state, findings);
  // All segmented labels are already present. Batch independent controls' key events to avoid
  // one Playwright round trip per knob. The DOM values confirm that the handlers took effect.
  const knobs = dialog.getByRole("slider").and(dialog.locator(".dctl"));
  expect(await knobs.count()).toBeGreaterThan(0);
  for (const key of ["Home", "End"]) {
    const target = key === "Home" ? "aria-valuemin" : "aria-valuemax";
    await knobs.evaluateAll((elements, key) => {
      for (const element of elements) element.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
    }, key);
    await expect.poll(() => knobs.evaluateAll((elements, target) => elements.filter((element) => {
      const value = Number(element.getAttribute("aria-valuenow"));
      return Math.abs(value - Number(element.getAttribute(target))) > 0.0001;
    }).map((element) => element.getAttribute("data-param")), target)).toEqual([]);
    await measure(dialog, state, findings);
  }

  // The region markers constrain each other. Restore each bound before moving the next one.
  const start = dialog.locator('.wmark[data-param="start"]');
  const end = dialog.locator('.wmark[data-param="end"]');
  const loop = dialog.locator('.wmark[data-param="loop_start"]');
  if (await start.count()) {
    for (const [marker, first, second] of [[start, "End", "Home"], [end, "Home", "End"], [loop, "End", "Home"]] as const) {
      await marker.press(first);
      await measure(dialog, state, findings);
      await marker.press(second);
      await measure(dialog, state, findings);
    }
  }
}

for (const viewport of [{ width: 1280, height: 800 }, { width: 900, height: 560 }]) {
  test.describe(`${viewport.width}×${viewport.height}`, () => {
    test.use({ viewport });

    test("sampler and synth editor text fits", async ({ app }) => {
      const findings = new Set<string>();
      const loaded = await openEditor(app, "Percussion sampler");
      await expect(loaded.getByRole("img", { name: "Percussion waveform" })).toBeVisible();
      await expect(loaded.locator(".sample-name")).toHaveText(sampleName);
      await loaded.getByText("File details", { exact: true }).click();
      await expect(loaded.locator("dd").first()).toBeVisible();
      await measure(loaded, "sampler / file details", findings);
      await loaded.getByText("File details", { exact: true }).click();
      await measureExtremes(loaded, "sampler / loaded", findings);
      await loaded.getByRole("button", { name: "Close the Percussion sampler" }).click();

      const missing = await openEditor(app, "Missing sampler");
      await expect(missing.getByRole("button", { name: "Relink Missing" })).toBeVisible();
      await measureExtremes(missing, "sampler / missing file", findings);
      await missing.getByRole("button", { name: "Close the Missing sampler" }).click();

      const synth = await openEditor(app, "Bass synth");
      const status = synth.locator(".synth-output-status");
      await app.keyboard.down("z");
      try {
        await expect(status).toContainText("1 voice");
        await measureExtremes(synth, "synth / live Output", findings);
      } finally {
        await app.keyboard.up("z");
      }
      await expect(status).toContainText("0 voices");
      await measure(synth, "synth / idle Output", findings);

      expect(findings.size, `Device text visibility at ${viewport.width}×${viewport.height}:\n${[...findings].join("\n")}`).toBe(0);
    });

    for (const { mode, type, cutoff } of [
      { mode: "LP 24", type: FILTER_LP24, cutoff: 400 },
      { mode: "BP", type: FILTER_BP, cutoff: 80 },
    ]) {
      const name = `${mode} at ${cutoff} Hz`;
      test.describe(name, () => {
        const filterBeat = structuredClone(beat);
        filterBeat.channels[2].synth = {
          ...filterBeat.channels[2].synth,
          filter_type: type, filter_cutoff: cutoff, filter_res: 0.05, filter_drive: 0, filter_env_amt: -0.6,
        };
        test.use({ seed: { project: filterBeat, files: { [samplePath]: { kind: "sample" } } } });

        test("synth filter text fits", async ({ app }) => {
          const synth = await openEditor(app, "Bass synth");
          await expect(synth.getByRole("radio", { name: mode, exact: true })).toBeChecked();
          await expect(synth.locator('.dctl[data-param="filter_cutoff"]')).toHaveAttribute("aria-valuenow", String(cutoff));
          const response = synth.getByRole("img", { name: "Filter response" });
          await expect(response.getByText("1k", { exact: true })).toBeVisible();
          const findings = new Set<string>();
          await measure(synth, `synth / ${name}`, findings);
          expect([...findings], `Synth filter text visibility at ${viewport.width}×${viewport.height}`).toEqual([]);
        });
      });
    }
  });
}
