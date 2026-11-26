import type { Locator, Page } from "@playwright/test";
import { expect, project, soundsFor, test, workspaceReady } from "./fixture";
import { pickerButton } from "./listPickerHelpers";
import { at, sessionNotes, settled, stats, twoChannels } from "./rollHelpers";

async function bothPanes(page: Page) {
  await workspaceReady(page);
  await page.keyboard.press("F7");
  await expect(page.getByRole("region", { name: "Channel rack" })).toBeVisible();
  await expect(page.getByRole("region", { name: "Piano roll", exact: true })).toBeVisible();
  await expect(page.getByRole("separator")).toBeVisible();
  // A finite local guard: no polling an empty/hidden canvas into acceptance.
  const frames = await page.locator(".rack-head").evaluateAll(async (headers) => {
    const samples: number[][] = [];
    for (let frame = 0; frame < 3; frame++) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      samples.push(headers.flatMap((el) => {
        const r = el.getBoundingClientRect();
        return [r.x, r.y, r.width, r.height];
      }));
    }
    return samples;
  });
  expect(frames[0]).toHaveLength(8);
  expect(frames[1]).toEqual(frames[0]);
  expect(frames[2]).toEqual(frames[1]);
}

async function contained(child: Locator, parent: Locator) {
  const c = await child.boundingBox();
  const p = await parent.boundingBox();
  expect(c).not.toBeNull();
  expect(p).not.toBeNull();
  if (!c || !p) throw new Error("visible geometry required");
  expect(c.width).toBeGreaterThan(0);
  expect(c.height).toBeGreaterThan(0);
  expect(c.x).toBeGreaterThanOrEqual(p.x);
  expect(c.y).toBeGreaterThanOrEqual(p.y);
  expect(c.x + c.width).toBeLessThanOrEqual(p.x + p.width);
  expect(c.y + c.height).toBeLessThanOrEqual(p.y + p.height);
}

test.describe("compact project identity", () => {
  const longName = "East Warehouse Live Sessions — Percussion and Synth Instrumentals";
  const longPath = `/qa/projects/${longName}.grooviq`;
  const outputName = "Warehouse Studio Monitor Output";
  const kick = project(["Kick"], { 1: [0, 8] });
  test.use({
    viewport: { width: 900, height: 560 },
    seed: {
      outputName,
      project: kick,
      files: { ...soundsFor(["Kick"]), [longPath]: { kind: "project", project: kick } },
    },
  });

  test("toolbar keeps project identity, save state and controls readable", async ({ app, mock }) => {
    const toolbar = app.locator(".toolbar");
    const projectColumn = toolbar.locator(".toolbar-project");
    const shortName = toolbar.getByRole("button", { name: "QA beat", exact: true });
    const mute = app.getByRole("region", { name: "Channel rack" }).getByRole("button", { name: "Mute Kick" });
    const output = toolbar.getByRole("button", { name: outputName });
    const outputLabel = output.getByText(outputName, { exact: true });
    const controls = [
      ...["Play", "Stop", "Record", "Metronome", "Typing keyboard", "Test tone"]
        .map((name) => toolbar.getByRole("button", { name, exact: true })),
      output,
      pickerButton(app, "Playback mode"),
      ...["Beats per bar", "Beat note value"]
        .map((name) => toolbar.getByRole("combobox", { name })),
      toolbar.getByRole("textbox", { name: "BPM" }),
      toolbar.getByRole("timer", { name: "Position" }),
      toolbar.getByRole("img", { name: "Master level meter" }),
    ];

    async function fits(label: Locator) {
      await expect(label).toBeVisible();
      await contained(label, projectColumn);
      const { scrollWidth, clientWidth, fontSize } = await label.evaluate((el) => ({
        scrollWidth: el.scrollWidth,
        clientWidth: el.clientWidth,
        fontSize: parseFloat(getComputedStyle(el).fontSize),
      }));
      expect(scrollWidth, `full text of "${await label.innerText()}"`).toBeLessThanOrEqual(clientWidth);
      expect(fontSize).toBeGreaterThanOrEqual(11);
    }

    async function controlsFit() {
      for (const control of controls) {
        await expect(control).toBeVisible();
        await expect(control).toBeInViewport({ ratio: 1 });
        await contained(control, toolbar);
      }
    }

    await app.evaluate(() => document.fonts.ready.then(() => true));
    for (const [width, height] of [[900, 560], [1280, 800], [1440, 900], [1920, 1080]]) {
      await app.setViewportSize({ width, height });
      await test.step(`${width}×${height} toolbar fits`, async () => {
        await fits(shortName.getByText("QA beat", { exact: true }));
        await fits(projectColumn.getByText("Not saved yet", { exact: true }));
        await controlsFit();
      });
    }

    const wideOutputWidth = await outputLabel.evaluate((el) => el.clientWidth);
    await test.step("900×560 long output leaves room for the project and save state", async () => {
      await app.setViewportSize({ width: 900, height: 560 });
      await expect(output).toHaveAttribute("title", outputName);
      const { scrollWidth, clientWidth, textOverflow } = await outputLabel.evaluate((el) => ({
        scrollWidth: el.scrollWidth,
        clientWidth: el.clientWidth,
        textOverflow: getComputedStyle(el).textOverflow,
      }));
      expect(textOverflow).toBe("ellipsis");
      expect(scrollWidth).toBeGreaterThan(clientWidth);
      expect(clientWidth).toBeLessThan(wideOutputWidth);
      await fits(shortName.getByText("QA beat", { exact: true }));
      await mute.click();
      await fits(projectColumn.getByText("Unsaved changes", { exact: true }));
    });

    await mock.eval(`(m) => m.queueDialog(${JSON.stringify(longPath)})`);
    await shortName.click();
    await app.getByRole("menuitem", { name: "Open…" }).click();
    await app.getByRole("button", { name: "Don't save" }).click();

    const longProject = toolbar.getByRole("button", { name: longName, exact: true });
    await expect(longProject).toBeVisible();
    await expect(longProject).toHaveAttribute("title", longName);
    const longLabel = longProject.getByText(longName, { exact: true });
    expect(await longLabel.evaluate((el) => (
      el.scrollWidth > el.clientWidth && getComputedStyle(el).textOverflow === "ellipsis"
    ))).toBe(true);
    await contained(longProject, toolbar);
    await fits(projectColumn.getByText(/^Saved /));
    await mute.click();
    await fits(projectColumn.getByText("Unsaved changes", { exact: true }));
    await controlsFit();
  });
});

async function geometry(page: Page) {
  const headers = page.locator(".rack-head");
  await expect(headers).toHaveCount(2);
  await expect(page.getByRole("region", { name: "Channel rack" }).locator(".rack-head").locator(":scope > span, :scope > label")).toHaveCount(2);
  await expect(page.getByRole("region", { name: "Piano roll", exact: true }).locator(".rack-head").locator(":scope > span, :scope > label")).toHaveCount(1);
  await expect(page.locator(".rack-head > span, .rack-head > label")).toHaveCount(3);
  for (const header of await headers.all()) {
    expect((await header.boundingBox())?.height).toBe(40);
    expect(await header.locator("h2").evaluate((el) => getComputedStyle(el).fontSize)).toBe("14px");
    // Structural selectors intentionally still find metadata if its class regresses to .chip.
    for (const metadata of await header.locator(":scope > span, :scope > label").all()) {
      expect((await metadata.boundingBox())?.height).toBe(24);
      await contained(metadata, header);
    }
  }
  const output = page.locator("button.chip[aria-haspopup='dialog']");
  expect((await output.boundingBox())?.height).toBe(32);
  expect(await output.evaluate((el) => {
    const s = getComputedStyle(el);
    return [s.gap, s.paddingTop, s.paddingRight, s.paddingBottom, s.paddingLeft, s.borderRadius];
  })).toEqual(["4px", "3px", "8px", "3px", "8px", "4px"]);
  await contained(output, page.locator(".toolbar"));
  await contained(page.getByRole("spinbutton", { name: "Pattern length" }), page.locator(".pattern-length"));
}

test.describe("compact workspace metadata", () => {
  test.use({ seed: { project: twoChannels([]) } });
  for (const [width, height] of [[1280, 800], [1440, 900], [1920, 1080], [900, 560]]) {
    test(`${width}x${height} geometry24/40/output32 and containment`, async ({ app }) => {
      await app.setViewportSize({ width, height });
      expect(await app.evaluate(() => [window.innerWidth, window.innerHeight])).toEqual([width, height]);
      await bothPanes(app);
      await geometry(app);
    });
  }

  test("PatternLength Enter commits actual model steps", async ({ app, mock }) => {
    await bothPanes(app);
    const input = app.getByRole("spinbutton", { name: "Pattern length" });
    await input.fill("32");
    await input.press("Enter");
    await expect.poll(async () => (await mock.view()).pattern.steps).toBe(32);
    await expect(input).toHaveValue("32");
    await expect(input).not.toBeFocused();
  });

  test("PatternLength blur commits actual model steps", async ({ app, mock }) => {
    await bothPanes(app);
    const input = app.getByRole("spinbutton", { name: "Pattern length" });
    await input.fill("24");
    await input.press("Tab");
    await expect.poll(async () => (await mock.view()).pattern.steps).toBe(24);
    await expect(input).toHaveValue("24");
    await expect(input).not.toBeFocused();
  });

  test("Output pointer, Escape and actual-opener focus return including WebKit body fallback", async ({ app }) => {
    await bothPanes(app);
    const output = app.locator("button.chip[aria-haspopup='dialog']");
    const dialog = app.getByRole("dialog", { name: "Settings" });
    await output.focus();
    await output.click();
    await expect(dialog).toBeVisible();
    await expect.poll(() => dialog.evaluate((el) => el.contains(document.activeElement))).toBe(true);
    await app.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await expect(output).toBeFocused();
    // WebKit pointer clicks leave body active; exercise that actual browser path as well.
    await output.evaluate((el) => (el as HTMLElement).blur());
    expect(await app.evaluate(() => document.activeElement === document.body)).toBe(true);
    await output.click();
    await expect(dialog).toBeVisible();
    await app.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await expect(output).toBeFocused();
  });
});

test.describe("long generated metadata", () => {
  const name = "Generated channel with a deliberately long musical label ".repeat(8);
  const seedProject = project([name]);
  seedProject.pattern.name = "Generated pattern with a deliberately long musical label ".repeat(8);
  test.use({ viewport: { width: 900, height: 560 }, seed: { project: seedProject, files: soundsFor([name]) } });

  test("Long generated metadata remains contained", async ({ app }) => {
    expect(await app.evaluate(() => [window.innerWidth, window.innerHeight])).toEqual([900, 560]);
    await bothPanes(app);
    await geometry(app);
    await expect(app.locator(".rack-head > span b")).toHaveCount(2);
    for (const label of await app.locator(".rack-head > span b").all()) {
      expect(await label.evaluate((el) => {
        const s = getComputedStyle(el);
        return el.scrollWidth > el.clientWidth && s.textOverflow === "ellipsis" && s.whiteSpace === "nowrap";
      })).toBe(true);
      await contained(label, label.locator(".."));
    }
    const input = app.getByRole("spinbutton", { name: "Pattern length" });
    await input.click();
    await expect(input).toBeFocused();
  });
});

async function noteAreaGeometry(page: Page) {
  const body = page.getByRole("region", { name: "Piano roll", exact: true }).locator(".roll-body");
  await expect(body).toHaveCount(1);
  return body.evaluate(async (element) => {
    // Read real DOM geometry even at zero height: a hidden note surface must fail the
    // minimum-height assertion, rather than time out waiting to become visible.
    const notes = element.querySelector<HTMLCanvasElement>("canvas[aria-label='Piano roll notes']");
    if (!notes) throw new Error("The actual piano-roll note canvas must be mounted");
    const samples = [];
    for (let frame = 0; frame < 3; frame++) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      const bodyBox = element.getBoundingClientRect();
      const canvasBox = notes.getBoundingClientRect();
      samples.push({ bodyWidth: bodyBox.width, bodyHeight: bodyBox.height,
        canvasWidth: canvasBox.width, canvasHeight: canvasBox.height });
    }
    return samples;
  });
}

async function minimumNoteArea(page: Page) {
  const samples = await noteAreaGeometry(page);
  expect(samples[1]).toEqual(samples[0]);
  expect(samples[2]).toEqual(samples[1]);
  expect(samples[2].bodyWidth).toBeGreaterThan(0);
  expect(samples[2].canvasWidth).toBeGreaterThan(0);
  expect(samples[2].bodyHeight).toBeGreaterThanOrEqual(112);
  expect(samples[2].canvasHeight).toBeGreaterThanOrEqual(112);
}

const displayedSplit = async (page: Page) => Number(await page.getByRole("separator").getAttribute("aria-valuenow"));
const rackPaneHeight = (page: Page) => page.locator(".panel-rack").evaluate((element) => element.getBoundingClientRect().height);

async function requestLargeSplit(page: Page) {
  await page.setViewportSize({ width: 1920, height: 1080 });
  await bothPanes(page);
  const separator = page.getByRole("separator");
  await separator.focus();
  for (let step = 0; step < 3; step++) await page.keyboard.press("ArrowDown");
  await expect.poll(() => displayedSplit(page)).toBe(65);
  await minimumNoteArea(page);
}

async function measuredPanelChrome(page: Page) {
  return page.evaluate(() => {
    const measure = (element: Element | null) => {
      if (!element) return null;
      const box = element.getBoundingClientRect();
      const style = getComputedStyle(element);
      return {
        x: box.x, y: box.y, width: box.width, height: box.height,
        paddingTop: Number.parseFloat(style.paddingTop), paddingBottom: Number.parseFloat(style.paddingBottom),
        borderTop: Number.parseFloat(style.borderTopWidth), borderBottom: Number.parseFloat(style.borderBottomWidth),
        minHeight: style.minHeight, flexShrink: style.flexShrink, boxSizing: style.boxSizing,
      };
    };
    const workspace = document.querySelector<HTMLElement>(".workspace");
    const rackBody = measure(document.querySelector(".rack-body"));
    const rackHeader = measure(document.querySelector(".panel-rack .rack-head"));
    const firstRackRow = measure(document.querySelector(".rack-rows .channel-row"));
    const rollHeader = measure(document.querySelector(".panel-roll > .rack-head"));
    const tools = measure(document.querySelector(".roll-tools"));
    const velocity = measure(document.querySelector(".roll-lane"));
    const splitter = document.querySelector<HTMLElement>(".splitter");
    const splitterBox = measure(splitter);
    return {
      observation: "after-case DOM measurements; absent elements remain null",
      viewport: { width: innerWidth, height: innerHeight, deviceScaleFactor: devicePixelRatio },
      fonts: { status: document.fonts.status, family: getComputedStyle(document.body).fontFamily },
      remainingViewport: measure(workspace?.parentElement ?? null),
      innerWorkspace: measure(workspace),
      rackPane: measure(document.querySelector(".panel-rack")),
      rackContent: measure(document.querySelector(".rack")),
      rollPane: measure(document.querySelector(".panel-roll")),
      noteBody: measure(document.querySelector(".roll-body")),
      noteCanvas: measure(document.querySelector("canvas[aria-label='Piano roll notes']")),
      intrinsicChrome: {
        rackHeader, firstRackRow, rackBody,
        rackPadding: rackBody ? rackBody.paddingTop + rackBody.paddingBottom : null,
        rollHeader, tools, velocity, splitter: splitterBox,
      },
      splitPropertyObservation: workspace ? getComputedStyle(workspace).getPropertyValue("--split").trim() : null,
      aria: splitter ? {
        current: splitter.getAttribute("aria-valuenow"), minimum: splitter.getAttribute("aria-valuemin"),
        maximum: splitter.getAttribute("aria-valuemax"), text: splitter.getAttribute("aria-valuetext"),
      } : null,
    };
  });
}

test.describe("actual note-area minimum with both panes", () => {
  test.use({ seed: { project: twoChannels([]) } });

  test.afterEach(async ({ app }, info) => {
    await info.attach("actual-panel-chrome", {
      body: JSON.stringify(await measuredPanelChrome(app)), contentType: "application/json",
    });
    const image = info.outputPath("actual-note-area.png");
    await app.screenshot({ path: image, animations: "disabled", scale: "css" });
    await info.attach("actual-note-area", { path: image, contentType: "image/png" });
  });

  test("fresh 900x560 keeps actual note body and canvas at least 112px", async ({ app }) => {
    await app.setViewportSize({ width: 900, height: 560 });
    expect(await app.evaluate(() => [innerWidth, innerHeight])).toEqual([900, 560]);
    await bothPanes(app);
    await minimumNoteArea(app);
  });

  for (const [width, height] of [[1280, 800], [1440, 900], [1920, 1080]]) {
    test(`${width}x${height} to 900x560 retains actual 112px note body and canvas`, async ({ app }, info) => {
      await app.setViewportSize({ width, height });
      await bothPanes(app);
      await minimumNoteArea(app);
      await info.attach("initial-large-panel-chrome", {
        body: JSON.stringify(await measuredPanelChrome(app)), contentType: "application/json",
      });
      const initialImage = info.outputPath(`initial-note-area-${width}x${height}.png`);
      await app.screenshot({ path: initialImage, animations: "disabled", scale: "css" });
      await info.attach("initial-large-note-area", { path: initialImage, contentType: "image/png" });
      await app.setViewportSize({ width: 900, height: 560 });
      expect(await app.evaluate(() => [innerWidth, innerHeight])).toEqual([900, 560]);
      await expect(app.getByRole("region", { name: "Channel rack" })).toBeVisible();
      await expect(app.getByRole("separator")).toBeVisible();
      await minimumNoteArea(app);
    });
  }

  test("grow-back restores the requested split after compact fitting", async ({ app }) => {
    await requestLargeSplit(app);
    await app.setViewportSize({ width: 900, height: 560 });
    await minimumNoteArea(app);
    expect(await displayedSplit(app)).toBeLessThan(65);
    await app.setViewportSize({ width: 1920, height: 1080 });
    await minimumNoteArea(app);
    await expect.poll(() => displayedSplit(app)).toBe(65);
  });

  test("blocked compact keys do not accumulate a hidden split request", async ({ app }) => {
    await app.setViewportSize({ width: 900, height: 560 });
    await bothPanes(app);
    await minimumNoteArea(app);
    const separator = app.getByRole("separator");
    const compactValue = await displayedSplit(app);
    const compactHeight = await rackPaneHeight(app);
    expect(compactValue).toBeLessThan(50);
    await separator.focus();
    for (let step = 0; step < 8; step++) await app.keyboard.press("ArrowDown");
    expect(await displayedSplit(app)).toBe(compactValue);
    expect(await rackPaneHeight(app)).toBe(compactHeight);
    await minimumNoteArea(app);
    await app.setViewportSize({ width: 1920, height: 1080 });
    await minimumNoteArea(app);
    await expect.poll(() => displayedSplit(app)).toBe(50);
  });

  test("compact dragging starts from the displayed split", async ({ app }) => {
    await requestLargeSplit(app);
    await app.setViewportSize({ width: 900, height: 560 });
    await minimumNoteArea(app);
    const separator = app.getByRole("separator");
    const box = await separator.boundingBox();
    if (!box) throw new Error("Visible separator required for actual pointer input");
    const before = await rackPaneHeight(app);
    const start = { x: box.x + box.width / 2, y: box.y + box.height / 2 };
    await app.mouse.move(start.x, start.y);
    await app.mouse.down();
    try {
      await app.mouse.move(start.x, start.y - 12, { steps: 8 });
    } finally {
      await app.mouse.up();
    }
    expect(before - await rackPaneHeight(app)).toBeCloseTo(12, 1);
    await minimumNoteArea(app);
  });

  test("resize during a split drag retires its stale origin", async ({ app }) => {
    await app.setViewportSize({ width: 1280, height: 800 });
    await bothPanes(app);
    await minimumNoteArea(app);
    const separator = app.getByRole("separator");
    const initial = await separator.boundingBox();
    if (!initial) throw new Error("Visible separator required before the drag");
    await app.mouse.move(initial.x + initial.width / 2, initial.y + initial.height / 2);
    await app.mouse.down();
    try {
      await app.setViewportSize({ width: 900, height: 560 });
      await minimumNoteArea(app);
      const resized = await separator.boundingBox();
      if (!resized) throw new Error("The same separator must remain mounted after resize");
      const value = await displayedSplit(app);
      const height = await rackPaneHeight(app);
      await app.mouse.move(resized.x + resized.width / 2, resized.y - 12, { steps: 4 });
      expect(await displayedSplit(app)).toBe(value);
      expect(await rackPaneHeight(app)).toBe(height);
    } finally {
      await app.mouse.up();
    }
    await app.setViewportSize({ width: 1280, height: 800 });
    await minimumNoteArea(app);
    await expect.poll(() => displayedSplit(app)).toBe(50);
  });

  test("a zero-movement compact drag preserves the large requested split", async ({ app }) => {
    await requestLargeSplit(app);
    await app.setViewportSize({ width: 900, height: 560 });
    await minimumNoteArea(app);
    const separator = app.getByRole("separator");
    const box = await separator.boundingBox();
    if (!box) throw new Error("Visible separator required for the zero-movement gesture");
    const value = await displayedSplit(app);
    const height = await rackPaneHeight(app);
    const x = box.x + box.width / 2;
    const y = box.y + box.height / 2;
    await app.mouse.move(x, y);
    await app.mouse.down();
    try {
      await app.mouse.move(x, y);
    } finally {
      await app.mouse.up();
    }
    expect(await displayedSplit(app)).toBe(value);
    expect(await rackPaneHeight(app)).toBe(height);
    await app.setViewportSize({ width: 1920, height: 1080 });
    await minimumNoteArea(app);
    await expect.poll(() => displayedSplit(app)).toBe(65);
  });

  test("velocity-lane fitting preserves its size and the requested split", async ({ app }) => {
    await requestLargeSplit(app);
    await app.setViewportSize({ width: 900, height: 560 });
    await minimumNoteArea(app);
    const roll = app.getByRole("region", { name: "Piano roll", exact: true });
    const velocity = roll.getByRole("button", { name: "Velocity", exact: true });
    const before = await displayedSplit(app);
    expect(await roll.locator(".roll-lane").evaluate((element) => element.getBoundingClientRect().height)).toBe(72);
    await velocity.click();
    await expect(roll.locator(".roll-lane")).toHaveCount(0);
    await minimumNoteArea(app);
    expect(await displayedSplit(app)).toBeGreaterThan(before);
    await velocity.click();
    await expect(roll.locator(".roll-lane")).toHaveCount(1);
    await minimumNoteArea(app);
    expect(await displayedSplit(app)).toBe(before);
    expect(await roll.locator(".roll-lane").evaluate((element) => element.getBoundingClientRect().height)).toBe(72);
    await app.setViewportSize({ width: 1920, height: 1080 });
    await minimumNoteArea(app);
    await expect.poll(() => displayedSplit(app)).toBe(65);
  });

  test("short-height scrolling stays inside the workspace viewport", async ({ app }) => {
    await app.setViewportSize({ width: 900, height: 360 });
    await bothPanes(app);
    await minimumNoteArea(app);
    const viewport = app.locator(".workspace-viewport");
    await expect(viewport).toHaveCount(1);
    const pageScroll = await app.evaluate(() => document.scrollingElement?.scrollTop ?? 0);
    expect(await viewport.evaluate((element) => element.scrollHeight > element.clientHeight)).toBe(true);
    await viewport.hover();
    await app.mouse.wheel(0, 800);
    await expect.poll(() => viewport.evaluate((element) => element.scrollTop)).toBeGreaterThan(0);
    expect(await app.evaluate(() => document.scrollingElement?.scrollTop ?? 0)).toBe(pageScroll);
    await minimumNoteArea(app);
    await expect(app.getByLabel("Piano roll notes", { exact: true })).toBeInViewport({ ratio: 0.99 });
  });

  test("single-pane toggles preserve note minimum and both-pane operation", async ({ app }) => {
    await app.setViewportSize({ width: 900, height: 560 });
    await bothPanes(app);
    await minimumNoteArea(app);
    // F6 is exercised as behavior here, after proving BOTH panes' geometry; it is
    // never a setup shortcut that gives the roll space by hiding the rack.
    await app.keyboard.press("F6");
    await expect(app.getByRole("region", { name: "Channel rack" })).toBeHidden();
    await expect(app.getByRole("separator")).toHaveCount(0);
    await minimumNoteArea(app);
    await app.keyboard.press("F6");
    await expect(app.getByRole("region", { name: "Channel rack" })).toBeVisible();
    await minimumNoteArea(app);
    await app.keyboard.press("F7");
    await expect(app.getByRole("region", { name: "Piano roll", exact: true })).toBeHidden();
    await expect(app.getByRole("region", { name: "Channel rack" })).toBeVisible();
    await expect(app.getByRole("separator")).toHaveCount(0);
    await app.keyboard.press("F7");
    await minimumNoteArea(app);
    await expect(app.getByRole("separator")).toBeVisible();
  });

  test("the compact note canvas supports draw, select, move and exact Undo", async ({ app, mock }) => {
    await app.setViewportSize({ width: 900, height: 560 });
    await bothPanes(app);
    await minimumNoteArea(app);
    await settled(app);
    const key = Math.max(0, Math.min(127, Math.floor((await stats(app)).view.topKey) - 3));
    const draw = await at(app, 240, key);
    await app.mouse.click(draw.x, draw.y);
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 240, 240, key]]);
    await settled(app);
    await app.getByRole("region", { name: "Piano roll", exact: true }).getByRole("button", { name: "Select", exact: true }).click();
    await settled(app);
    const from = await at(app, 360, key);
    const to = await at(app, 600, key);
    await app.mouse.move(from.x, from.y);
    await app.mouse.down();
    try {
      await app.mouse.move(to.x, to.y, { steps: 8 });
    } finally {
      await app.mouse.up();
    }
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 480, 240, key]]);
    await app.keyboard.press("ControlOrMeta+z");
    await expect.poll(() => sessionNotes(mock)).toEqual([[1, 240, 240, key]]);
    await app.keyboard.press("ControlOrMeta+z");
    await expect.poll(() => sessionNotes(mock)).toEqual([]);
    await minimumNoteArea(app);
  });
});
