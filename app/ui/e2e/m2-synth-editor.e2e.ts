// M2 Task 13 (#84): the synth editor, opened from a synth channel in the rack as a floating
// window. Assertions read the mock session's command log and state; values come from the
// engine's parameter and preset fixtures.
import type { Locator, Page, TestInfo } from "@playwright/test";
import { SYNTH_PARAMS, expect, presetValues, project, soundsFor, test, type Mock } from "./fixture";
import type { MockProject } from "./mockTauri";

const KIT = ["Kick", "Snare"];
const SYNTH_ID = 3;

/** A kit and a synth channel "Bass" holding the 808 Glide preset. */
function withSynth(): MockProject {
  const p = project(KIT, { 1: [0, 8] });
  p.channels.push({
    id: SYNTH_ID,
    name: "Bass",
    color: 3,
    sample: null,
    volume: 1,
    pan: 0,
    mute: false,
    solo: false,
    sampler: {},
    kind: "synth",
    synth: presetValues("808 Glide"),
    preset: "808 Glide",
  });
  p.nextId = SYNTH_ID + 1;
  return p;
}

const opener = (page: Page) => page.getByRole("button", { name: "Edit the Bass synth" });
const editor = (page: Page) => page.getByRole("dialog", { name: "Bass synth" });
const control = (page: Page, id: string) => editor(page).locator(`[data-param="${id}"]`);
const spec = (id: string) => SYNTH_PARAMS.find((p) => p.id === id)!;

async function open(page: Page) {
  await opener(page).click();
  await expect(editor(page)).toBeVisible();
}

/** What every control in the editor shows, by parameter id: a slider's value, or the value of
 * an enum's checked button. */
async function shown(page: Page): Promise<Record<string, number>> {
  const raw = await editor(page)
    .locator("[data-param]")
    .evaluateAll((els) =>
      els.map((el) => {
        const id = el.getAttribute("data-param")!;
        if (el.getAttribute("role") === "radiogroup") {
          const radios = [...el.querySelectorAll("[role='radio']")];
          return [id, radios.findIndex((r) => r.getAttribute("aria-checked") === "true"), true] as const;
        }
        return [id, Number(el.getAttribute("aria-valuenow")), false] as const;
      }),
    );
  return Object.fromEntries(raw.map(([id, v, isEnum]) => [id, isEnum ? v + spec(id).min : v]));
}

function expectShown(values: Record<string, number>, want: Record<string, number>) {
  expect(Object.keys(values).sort()).toEqual(Object.keys(want).sort());
  for (const [id, v] of Object.entries(want)) expect(values[id], id).toBeCloseTo(v, 3);
}

/** The synth's stored values in the mock project. */
const stored = async (mock: Mock) =>
  (await mock.eval<{ channels: { id: number; synth: Record<string, number>; preset: string | null }[] }>("(m) => m.view()")).channels.find(
    (c) => c.id === SYNTH_ID,
  )!;
/** `setInstrumentParam` edits sent, as [param, value]. */
const paramEdits = async (mock: Mock) =>
  (await mock.calls("project_edit"))
    .map((c) => c.args.edit as { kind: string; param?: string; value?: number })
    .filter((e) => e.kind === "setInstrumentParam")
    .map((e) => [e.param, e.value]);
const liveCalls = async (mock: Mock, param: string) => (await mock.calls("channel_param_live")).filter((c) => c.args.param === param);
const liveValue = (mock: Mock, param: string) => mock.eval<number | undefined>(`(m) => m.state.liveParams.get("${SYNTH_ID}:${param}")`);

const centre = async (el: Locator) => {
  const b = (await el.boundingBox())!;
  return { x: b.x + b.width / 2, y: b.y + b.height / 2 };
};

async function shot(page: Page, info: TestInfo, name: string) {
  await info.attach(`${name}-aria`, { body: await editor(page).ariaSnapshot(), contentType: "text/yaml" });
  await page.screenshot({ path: info.outputPath(`${name}.png`) });
}

test.describe("the synth editor", () => {
  test.use({ seed: { project: withSynth(), files: soundsFor(KIT) } });

  test("every_one_of_the_40_parameters_has_a_control", async ({ app, mock }, info) => {
    await open(app);
    await shot(app, info, "synth-editor");
    expect(SYNTH_PARAMS.length).toBe(40);
    for (const p of SYNTH_PARAMS) {
      const c = control(app, p.id);
      await expect(c, p.id).toHaveCount(1);
      await expect(c, p.id).toBeVisible();
      expect(await c.getAttribute("data-hint"), p.id).toBeTruthy();
    }
    // Each shows the channel's stored value.
    expectShown(await shown(app), presetValues("808 Glide"));
    expectShown(await shown(app), (await stored(mock)).synth);
    // The window fits the app window (design size 880 x 520).
    const box = (await app.locator(".device-window").boundingBox())!;
    expect(box.width).toBeLessThanOrEqual(880);
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.y + box.height).toBeLessThanOrEqual(800);
  });

  test("a_knob_drag_previews_live_and_commits_once", async ({ app, mock }) => {
    await open(app);
    const knob = control(app, "filter_cutoff");
    const start = await centre(knob);
    await app.mouse.move(start.x, start.y);
    await app.mouse.down();
    for (let i = 1; i <= 8; i++) await app.mouse.move(start.x, start.y - i * 6);
    // Live while it moves: the engine hears it, the project does not change.
    await expect.poll(() => liveValue(mock, "filter_cutoff")).toBeGreaterThan(650);
    expect(await paramEdits(mock)).toEqual([]);
    expect((await stored(mock)).synth.filter_cutoff).toBe(650);
    const during = Number(await knob.getAttribute("aria-valuenow"));
    expect(during).toBeGreaterThan(650);

    // A burst of moves inside one frame sends one live value, the last.
    const before = (await liveCalls(mock, "filter_cutoff")).length;
    await knob.evaluate((el, y) => {
      for (let i = 1; i <= 10; i++) {
        el.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, clientX: 0, clientY: y - 48 - i * 3, pointerId: 1, isPrimary: true }));
      }
    }, start.y);
    await app.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
    const burst = (await liveCalls(mock, "filter_cutoff")).slice(before);
    expect(burst.length).toBe(1);
    expect(burst[0].args.value).toBeCloseTo(Number(await knob.getAttribute("aria-valuenow")), 3);

    // One more move and the release in the same frame: the commit carries that value, and no
    // live value goes out after it.
    const livesBeforeRelease = (await liveCalls(mock, "filter_cutoff")).length;
    await knob.evaluate((el, y) => {
      const at = { bubbles: true, clientX: 0, clientY: y - 90, pointerId: 1, isPrimary: true };
      el.dispatchEvent(new PointerEvent("pointermove", at));
      el.dispatchEvent(new PointerEvent("pointerup", at));
    }, start.y);
    await app.mouse.up();
    const last = Number(await knob.getAttribute("aria-valuenow"));
    expect(last).toBeGreaterThan(burst[0].args.value as number);
    // One commit, at the value the knob shows; the project holds it.
    await expect.poll(() => paramEdits(mock)).toHaveLength(1);
    const [[param, value]] = await paramEdits(mock);
    expect(param).toBe("filter_cutoff");
    expect(value).toBeCloseTo(last, 3);
    await expect.poll(async () => (await stored(mock)).synth.filter_cutoff).toBeCloseTo(last, 3);
    await expect(knob).toHaveAttribute("aria-valuenow", String(Number(last.toFixed(4))));
    // Nothing else was committed or sent late.
    await app.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
    expect((await liveCalls(mock, "filter_cutoff")).length).toBe(livesBeforeRelease);
    expect(await paramEdits(mock)).toHaveLength(1);
  });

  test("an_envelope_point_drag_commits_its_values_once", async ({ app, mock }) => {
    await open(app);
    const graph = editor(app).getByRole("img", { name: "Amp envelope" });
    // The sustain point: end of the decay, at the sustain level (0.12 in 808 Glide).
    const pt = await graph.evaluate((svg) => {
      const c = svg.querySelectorAll("circle")[1].getBoundingClientRect();
      return { x: c.x + c.width / 2, y: c.y + c.height / 2 };
    });
    await app.mouse.move(pt.x, pt.y);
    await app.mouse.down();
    await app.mouse.move(pt.x, pt.y - 20, { steps: 4 });
    await app.mouse.move(pt.x, pt.y - 40, { steps: 4 });
    await expect.poll(() => liveValue(mock, "amp_sustain")).toBeGreaterThan(0.12);
    expect(await paramEdits(mock)).toEqual([]);
    await app.mouse.up();
    await expect.poll(() => paramEdits(mock)).toHaveLength(1);
    const [[param, value]] = await paramEdits(mock);
    expect(param).toBe("amp_sustain");
    expect(value).toBeGreaterThan(0.12);
    await expect(control(app, "amp_sustain")).toHaveAttribute("aria-valuenow", String(Number((value as number).toFixed(4))));
  });

  test("double_click_resets_to_the_default", async ({ app, mock }) => {
    await open(app);
    for (const id of ["filter_cutoff", "osc1_shape", "amp_decay"]) {
      await control(app, id).dblclick();
      await expect(control(app, id)).toHaveAttribute("aria-valuenow", String(spec(id).default));
    }
    expect(await paramEdits(mock)).toEqual([
      ["filter_cutoff", spec("filter_cutoff").default],
      ["osc1_shape", spec("osc1_shape").default],
      ["amp_decay", spec("amp_decay").default],
    ]);
    expect((await stored(mock)).synth).toMatchObject({ filter_cutoff: 4000, osc1_shape: 2, amp_decay: 300 });
  });

  test("keys_and_the_wheel_change_a_control", async ({ app, mock }) => {
    await open(app);
    const semi = control(app, "osc1_semi");
    await semi.focus();
    await app.keyboard.press("ArrowUp");
    await app.keyboard.press("ArrowUp");
    await expect(semi).toHaveAttribute("aria-valuenow", "2");
    expect(await paramEdits(mock)).toEqual([
      ["osc1_semi", 1],
      ["osc1_semi", 2],
    ]);
    // A wheel gesture is live while it turns and commits once when it settles.
    const level = control(app, "osc1_level");
    await level.hover();
    for (let i = 0; i < 3; i++) await app.mouse.wheel(0, -100);
    await expect.poll(() => liveValue(mock, "osc1_level")).toBeGreaterThan(0.9);
    await expect.poll(() => paramEdits(mock)).toHaveLength(3);
    expect((await paramEdits(mock))[2][0]).toBe("osc1_level");
    expect((await stored(mock)).synth.osc1_level).toBeCloseTo(0.9 + 3 * 0.025, 6);
  });

  test("choosing_a_preset_sends_load_synth_preset_and_updates_every_control", async ({ app, mock }) => {
    await open(app);
    const reese = editor(app).getByRole("button", { name: "Reese", exact: true });
    await reese.click();
    const loads = (await mock.calls("project_edit")).map((c) => c.args.edit).filter((e) => (e as { kind: string }).kind === "loadSynthPreset");
    expect(loads).toEqual([{ kind: "loadSynthPreset", channel: SYNTH_ID, preset: "Reese" }]);
    await expect(reese).toHaveAttribute("aria-pressed", "true");
    await expect(editor(app).getByRole("button", { name: "808 Glide", exact: true })).toHaveAttribute("aria-pressed", "false");
    await expect.poll(async () => (await shown(app)).filter_cutoff).toBeCloseTo(presetValues("Reese").filter_cutoff, 3);
    expectShown(await shown(app), presetValues("Reese"));
    expect(await stored(mock)).toMatchObject({ preset: "Reese", synth: presetValues("Reese") });
    // The rack names the preset too.
    await expect(app.getByRole("button", { name: /^Bass Reese/ })).toBeVisible();
  });

  test("enums_use_segmented_buttons", async ({ app, mock }) => {
    await open(app);
    const enums = ["sub_shape", "filter_type", "lfo_shape", "lfo_target", "voice_mode"];
    for (const id of enums) {
      const group = control(app, id);
      await expect(group, id).toHaveAttribute("role", "radiogroup");
      await expect(group.getByRole("radio"), id).toHaveCount(spec(id).max - spec(id).min + 1);
    }
    // The oscillator shapes morph continuously: knobs, not buttons.
    await expect(control(app, "osc1_shape")).toHaveAttribute("role", "slider");

    // A click chooses; the arrow keys move along the row.
    await control(app, "filter_type").getByRole("radio").nth(2).click();
    await expect(control(app, "filter_type").getByRole("radio").nth(2)).toHaveAttribute("aria-checked", "true");
    await app.keyboard.press("ArrowRight");
    await expect(control(app, "filter_type").getByRole("radio").nth(3)).toBeFocused();
    // 808 Glide plays legato; choose mono.
    await expect(control(app, "voice_mode").getByRole("radio", { name: "Legato" })).toHaveAttribute("aria-checked", "true");
    await control(app, "voice_mode").getByRole("radio", { name: "Mono" }).click();
    expect(await paramEdits(mock)).toEqual([
      ["filter_type", 2],
      ["filter_type", 3],
      ["voice_mode", 1],
    ]);
    expect((await stored(mock)).synth).toMatchObject({ filter_type: 3, voice_mode: 1 });
  });

  test("the_window_closes_with_escape_and_returns_focus", async ({ app }) => {
    await opener(app).focus();
    await app.keyboard.press("Enter");
    await expect(editor(app)).toBeVisible();
    // Focus moves in and Tab stays inside.
    const inside = () => editor(app).evaluate((el) => el.contains(document.activeElement));
    expect(await inside()).toBe(true);
    for (let i = 0; i < 60; i++) await app.keyboard.press("Tab");
    expect(await inside()).toBe(true);
    for (let i = 0; i < 5; i++) await app.keyboard.press("Shift+Tab");
    expect(await inside()).toBe(true);

    await app.keyboard.press("Escape");
    await expect(editor(app)).toHaveCount(0);
    await expect(opener(app)).toBeFocused();

    // Opening it again gives one window (the window covers the rack: click the button itself);
    // × closes it too.
    await opener(app).evaluate((b: HTMLElement) => b.click());
    await opener(app).evaluate((b: HTMLElement) => b.click());
    await expect(editor(app)).toHaveCount(1);
    await editor(app).getByRole("button", { name: "Close the Bass synth" }).click();
    await expect(editor(app)).toHaveCount(0);
  });

  test("the_title_bar_drags_the_window", async ({ app }) => {
    await open(app);
    const win = app.locator(".device-window");
    const before = (await win.boundingBox())!;
    const bar = editor(app).locator(".device-title");
    const at = await centre(bar);
    await app.mouse.move(at.x, at.y);
    await app.mouse.down();
    await app.mouse.move(at.x - 60, at.y + 40, { steps: 5 });
    await app.mouse.up();
    const after = (await win.boundingBox())!;
    expect(after.x).toBeCloseTo(before.x - 60, 0);
    expect(after.y).toBeCloseTo(before.y + 40, 0);
  });

  test("removing_the_channel_closes_its_window", async ({ app, mock }) => {
    await open(app);
    await app.getByRole("button", { name: /^Bass / }).focus();
    await app.keyboard.press("Delete");
    await app.getByRole("dialog", { name: "Remove Bass?" }).getByRole("button", { name: "Remove" }).click();
    await expect(editor(app)).toHaveCount(0);
    expect((await mock.view()).channels.map((c) => c.id)).toEqual([1, 2]);
  });
});
