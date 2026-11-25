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

  test("output_scope_moves_for_a_held_note_and_clears_when_released", async ({ app, mock }, info) => {
    await open(app);
    const scope = editor(app).getByLabel("Synth output waveform");
    const status = editor(app).locator(".synth-output-status");
    const pixels = () => scope.evaluate((c: HTMLCanvasElement) => c.toDataURL());
    await expect(status).toContainText("0 voices");
    const silent = await pixels();
    await app.keyboard.down("z");
    await expect(status).toContainText("1 voice");
    await expect.poll(pixels).not.toBe(silent);
    const first = await pixels();
    await expect.poll(pixels).not.toBe(first);
    await expect(editor(app).locator('[data-synth-meter="amp"]')).toBeVisible();
    await shot(app, info, "live-output");
    await app.keyboard.up("z");
    await expect.poll(pixels).toBe(silent);
    await expect(status).toContainText("0 voices");
    await app.keyboard.down("z");
    await expect(status).toContainText("1 voice");
    await mock.eval("(m) => m.pauseLive(true)");
    await expect.poll(pixels).toBe(silent);
    await app.keyboard.up("z");
    await mock.eval("(m) => m.pauseLive(false)");
    await app.getByRole("button", { name: "Mute Kick", exact: true }).focus();
    await expect(status).toContainText("0 voices");
    const before = (await mock.calls("synth_meter")).length;
    await app.evaluate(() => new Promise((resolve) => setTimeout(resolve, 150)));
    expect((await mock.calls("synth_meter")).length).toBeLessThanOrEqual(before + 1);
  });

  test("measured_stage_and_phase_drive_read_only_motion_and_frozen_capture_clears", async ({ app, mock }, info) => {
    await open(app);
    const before = await mock.eval("m => m.state.project");
    const amp = editor(app).locator('[data-synth-meter="amp"]');
    const flt = editor(app).locator('[data-synth-meter="flt"]');
    const lfo = editor(app).locator('[data-synth-meter="lfo"]');
    const snapshot = async (ampStage: number, phase: number, frames?: number) => {
      await mock.eval(`m => m.setSynthMotion(${JSON.stringify({ ampStage, filterStage: 2, lfoPhase: phase, ampLevel: 0.25, filterLevel: 0.75, lfoValue: -0.3, voices: 1, frames })})`);
    };
    const x = (dot: Locator) => dot.evaluate(e => Number(e.getAttribute("cx")));
    await snapshot(1, 0.1);
    await expect(amp).toBeVisible();
    const geometry = await amp.evaluate(e => ({
      left: Number((e as SVGElement).dataset.meterLeft), attack: Number((e as SVGElement).dataset.meterAttack),
      top: Number((e as SVGElement).dataset.meterTop), bottom: Number((e as SVGElement).dataset.meterBottom),
    }));
    const attackX = await x(amp);
    expect(attackX).toBeCloseTo((geometry.left + geometry.attack) / 2, 4);
    expect(Number(await amp.getAttribute("cy"))).toBeCloseTo(geometry.bottom - 0.25 * (geometry.bottom - geometry.top), 4);
    const phaseX = await x(lfo);
    const filterDecayX = await flt.evaluate(e => (Number((e as SVGElement).dataset.meterAttack) + Number((e as SVGElement).dataset.meterDecay)) / 2);
    await expect.poll(() => x(flt)).toBeCloseTo(filterDecayX, 4);
    await snapshot(3, 0.7);
    await expect.poll(() => x(amp)).toBeGreaterThan(attackX);
    await expect.poll(() => x(lfo)).toBeGreaterThan(phaseX);
    const sustainX = await x(amp);
    await snapshot(3, 0.7);
    await expect.poll(() => x(amp)).toBe(sustainX);
    await snapshot(4, 0.2);
    await expect.poll(() => x(amp)).toBeGreaterThan(sustainX);
    await shot(app, info, "measured-release");
    await app.setViewportSize({ width: 900, height: 800 });
    await expect.poll(async () => {
      const bounds = await editor(app).boundingBox();
      return bounds !== null && bounds.x >= 0 && bounds.x + bounds.width <= 900.5;
    }).toBe(true);
    await shot(app, info, "measured-release-900");
    await app.getByRole("button", { name: "Dismiss", exact: true }).click();
    await app.setViewportSize({ width: 900, height: 560 });
    await expect(amp).toBeVisible();
    await expect(lfo).toBeVisible();
    await expect(control(app, "amp_release")).toBeVisible();
    await expect.poll(async () => {
      const bounds = await amp.boundingBox();
      return bounds !== null && bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= 900 && bounds.y + bounds.height <= 560;
    }).toBe(true);
    await shot(app, info, "measured-release-900x560");
    await app.setViewportSize({ width: 1280, height: 800 });
    await snapshot(1, 0.4, 987654321);
    await expect.poll(() => x(amp)).toBe(attackX);
    await expect(amp).toBeHidden();
    await expect(flt).toBeHidden();
    await expect(lfo).toBeHidden();
    await expect(editor(app).locator('[data-synth-stage="amp"]')).toBeHidden();
    expect(await mock.eval("m => m.state.project")).toEqual(before);
    expect((await mock.calls("channel_param_live")).length).toBe(0);
    await mock.eval("m => m.setSynthMotion(null)");
  });

  test("held_meter_request_stays_bounded_across_selection_and_reopening", async ({ app, mock }) => {
    const settleFrames = () => app.evaluate(() => new Promise<void>(resolve => {
      requestAnimationFrame(() => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    }));
    await mock.eval('m => m.holdCommand("synth_meter")');
    await open(app);
    await expect.poll(async () => (await mock.calls("synth_meter")).length).toBe(1);
    try {
      for (let i = 0; i < 5; i++) {
        await app.getByRole("button", { name: "Mute Kick", exact: true }).focus();
        await settleFrames();
        await editor(app).getByText("Output", { exact: true }).click();
        await settleFrames();
      }
      for (let i = 0; i < 3; i++) {
        await editor(app).getByRole("button", { name: "Close the Bass synth", exact: true }).click();
        await expect(editor(app)).toBeHidden();
        await open(app);
        await settleFrames();
      }
      expect((await mock.calls("synth_meter")).length, "unanswered requests must stay bounded").toBe(1);
    } finally {
      await mock.eval('m => m.releaseCommand("synth_meter")');
    }
    const scope = editor(app).getByLabel("Synth output waveform");
    const status = editor(app).locator(".synth-output-status");
    await expect(status).toContainText("0 voices");
    const pixels = () => scope.evaluate((c: HTMLCanvasElement) => c.toDataURL());
    const silent = await pixels();
    await app.keyboard.down("z");
    await expect(status).toContainText("1 voice");
    await expect.poll(pixels).not.toBe(silent);
    await app.keyboard.up("z");
    await expect(status).toContainText("0 voices");
    await expect.poll(pixels).toBe(silent);
  });

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
    // Keep the gesture pending across host RPCs; advance frames explicitly below.
    await app.clock.install({ time: new Date("2026-01-01T00:00:00Z") });
    await app.clock.pauseAt(new Date("2026-01-01T00:01:00Z"));
    // A wheel gesture is live while it turns and commits once when it settles.
    const level = control(app, "osc1_level");
    await level.hover();
    for (let i = 0; i < 3; i++) {
      await app.mouse.wheel(0, -100);
      await app.clock.runFor(32);
      await expect.poll(() => liveValue(mock, "osc1_level")).toBeCloseTo(0.9 + (i + 1) * 0.025, 6);
    }
    await expect.poll(() => liveValue(mock, "osc1_level")).toBeGreaterThan(0.9);
    expect(await paramEdits(mock)).toHaveLength(2);
    await app.clock.runFor(299);
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

/** Drags a control straight up by `px` and keeps the button down. */
async function dragUp(page: Page, el: Locator, px: number) {
  const at = await centre(el);
  await page.mouse.move(at.x, at.y);
  await page.mouse.down();
  for (let i = 1; i <= 4; i++) await page.mouse.move(at.x, at.y - (i * px) / 4);
}

const twoFrames = (page: Page) => page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));

test.describe("ending a gesture early", () => {
  test.use({ seed: { project: withSynth(), files: soundsFor(KIT) } });

  test("escape_mid_drag_restores_the_value_without_an_edit", async ({ app, mock }) => {
    await open(app);
    await dragUp(app, control(app, "filter_cutoff"), 40);
    await expect.poll(() => liveValue(mock, "filter_cutoff")).toBeGreaterThan(650);
    await app.keyboard.press("Escape");
    await expect(editor(app)).toHaveCount(0);
    await app.mouse.up();
    await twoFrames(app);
    expect(await liveValue(mock, "filter_cutoff")).toBe(650);
    expect(await paramEdits(mock)).toEqual([]);
    expect((await stored(mock)).synth.filter_cutoff).toBe(650);
  });

  test("window_blur_mid_drag_restores_the_value_without_an_edit", async ({ app, mock }) => {
    await open(app);
    const knob = control(app, "filter_cutoff");
    await dragUp(app, knob, 40);
    await expect.poll(() => liveValue(mock, "filter_cutoff")).toBeGreaterThan(650);
    await app.evaluate(() => window.dispatchEvent(new Event("blur")));
    await expect.poll(() => liveValue(mock, "filter_cutoff")).toBe(650);
    await expect(knob).toHaveAttribute("aria-valuenow", "650");
    // The rest of that gesture does nothing.
    await app.mouse.move(0, 0);
    await app.mouse.up();
    await twoFrames(app);
    expect(await liveValue(mock, "filter_cutoff")).toBe(650);
    expect(await paramEdits(mock)).toEqual([]);
  });

  test("pointer_cancel_restores_the_value_without_an_edit", async ({ app, mock }) => {
    await open(app);
    for (const el of [control(app, "filter_cutoff"), editor(app).locator(".osc-display").first()]) {
      await dragUp(app, el, 40);
      await el.dispatchEvent("pointercancel", { pointerId: 1, bubbles: true });
      await app.mouse.up();
    }
    // An envelope point too: the amp sustain point.
    const graph = editor(app).getByRole("img", { name: "Amp envelope" });
    const pt = await graph.evaluate((svg) => {
      const c = svg.querySelectorAll("circle")[1].getBoundingClientRect();
      return { x: c.x + c.width / 2, y: c.y + c.height / 2 };
    });
    await app.mouse.move(pt.x, pt.y);
    await app.mouse.down();
    await app.mouse.move(pt.x, pt.y - 30, { steps: 4 });
    await expect.poll(() => liveValue(mock, "amp_sustain")).toBeGreaterThan(0.12);
    await editor(app).locator(".env-graph").first().dispatchEvent("pointercancel", { pointerId: 1, bubbles: true });
    await app.mouse.up();
    await twoFrames(app);
    expect(await liveValue(mock, "filter_cutoff")).toBe(650);
    expect(await liveValue(mock, "osc1_shape")).toBe(0);
    expect(await liveValue(mock, "amp_sustain")).toBeCloseTo(0.12, 6);
    expect(await paramEdits(mock)).toEqual([]);
    expectShown(await shown(app), presetValues("808 Glide"));
  });

  test("escape_mid_filter_drag_restores_both_values", async ({ app, mock }) => {
    await open(app);
    const fd = editor(app).locator(".filter-display");
    const at = await centre(fd);
    await app.mouse.move(at.x, at.y);
    await app.mouse.down();
    await app.mouse.move(at.x + 40, at.y - 30, { steps: 5 });
    await expect.poll(() => liveValue(mock, "filter_res")).toBeGreaterThan(0.05);
    await app.keyboard.press("Escape");
    await app.mouse.up();
    await twoFrames(app);
    expect(await liveValue(mock, "filter_cutoff")).toBe(650);
    expect(await liveValue(mock, "filter_res")).toBeCloseTo(0.05, 6);
    expect(await paramEdits(mock)).toEqual([]);
  });

  test("closing_during_a_wheel_gesture_sends_no_edit", async ({ app, mock }) => {
    await open(app);
    // Keep the gesture pending across host RPCs; advance frames explicitly below.
    await app.clock.install({ time: new Date("2026-01-01T00:00:00Z") });
    await app.clock.pauseAt(new Date("2026-01-01T00:01:00Z"));
    await control(app, "filter_cutoff").hover();
    await app.mouse.wheel(0, -100);
    await app.clock.runFor(32);
    await expect.poll(() => liveValue(mock, "filter_cutoff")).toBeGreaterThan(650);
    expect(await paramEdits(mock)).toEqual([]);
    await app.keyboard.press("Escape");
    await expect(editor(app)).toHaveCount(0);
    await app.clock.runFor(600);
    expect(await paramEdits(mock)).toEqual([]);
    expect(await liveValue(mock, "filter_cutoff")).toBe(650);
  });

  test("removing_the_channel_mid_gesture_sends_no_edit", async ({ app, mock }) => {
    await open(app);
    await dragUp(app, control(app, "filter_cutoff"), 40);
    await expect.poll(() => liveValue(mock, "filter_cutoff")).toBeGreaterThan(650);
    await app.getByRole("button", { name: /^Bass / }).focus();
    await app.keyboard.press("Delete");
    await app.getByRole("dialog", { name: "Remove Bass?" }).getByRole("button", { name: "Remove" }).press("Enter");
    await expect(editor(app)).toHaveCount(0);
    await app.mouse.up();
    await app.waitForTimeout(400);
    expect((await mock.calls("project_edit")).map((c) => (c.args.edit as { kind: string }).kind)).toEqual(["removeChannel"]);
  });
});

test.describe("presets during a gesture", () => {
  test.use({ seed: { project: withSynth(), files: soundsFor(KIT) } });

  test("a_preset_during_a_wheel_gesture_wins", async ({ app, mock }) => {
    await open(app);
    // Keep the gesture pending across host RPCs; advance frames explicitly below.
    await app.clock.install({ time: new Date("2026-01-01T00:00:00Z") });
    await app.clock.pauseAt(new Date("2026-01-01T00:01:00Z"));
    await control(app, "filter_cutoff").hover();
    await app.mouse.wheel(0, -100);
    await app.clock.runFor(32);
    await expect.poll(() => liveValue(mock, "filter_cutoff")).toBeGreaterThan(650);
    expect(await paramEdits(mock)).toEqual([]);
    await editor(app).getByRole("button", { name: "Reese", exact: true }).evaluate((b: HTMLElement) => b.click());
    await app.clock.runFor(600);
    expect(await paramEdits(mock)).toEqual([]);
    expect(await stored(mock)).toMatchObject({ preset: "Reese", synth: presetValues("Reese") });
    expectShown(await shown(app), presetValues("Reese"));
  });

  test("a_preset_during_a_drag_wins", async ({ app, mock }) => {
    await open(app);
    await dragUp(app, control(app, "filter_cutoff"), 40);
    await expect.poll(() => liveValue(mock, "filter_cutoff")).toBeGreaterThan(650);
    await editor(app).getByRole("button", { name: "Reese", exact: true }).evaluate((b: HTMLElement) => b.click());
    await app.mouse.move(0, 0);
    await app.mouse.up();
    await app.waitForTimeout(300);
    expect(await paramEdits(mock)).toEqual([]);
    expect(await stored(mock)).toMatchObject({ preset: "Reese", synth: presetValues("Reese") });
    expectShown(await shown(app), presetValues("Reese"));
  });
});

/** Another project, with a different synth at the same channel id. */
function replacement(): MockProject {
  const p = withSynth();
  p.name = "Other";
  p.channels[2] = { ...p.channels[2], name: "Replacement", synth: presetValues("Reese"), preset: "Reese" };
  return p;
}

test.describe("replacing the project", () => {
  test.use({
    seed: {
      project: withSynth(),
      files: { ...soundsFor(KIT), "/qa/projects/Other.grooviq": { kind: "project", project: replacement() }, "/qa/projects/Same.grooviq": { kind: "project", project: withSynth() } },
    },
  });

  test("opening_another_project_closes_the_editor", async ({ app, mock }) => {
    await open(app);
    await dragUp(app, control(app, "filter_cutoff"), 40);
    await mock.eval("(m) => m.queueDialog('/qa/projects/Other.grooviq')");
    await app.keyboard.press("ControlOrMeta+o");
    await expect.poll(() => mock.view().then((v) => v.path)).toBe("/qa/projects/Other.grooviq");
    await app.mouse.up();
    await app.waitForTimeout(400);
    await expect(app.getByRole("dialog")).toHaveCount(0);
    expect(await paramEdits(mock)).toEqual([]);
    expect((await stored(mock)).synth).toEqual(presetValues("Reese"));
  });

  test("reopening_the_same_file_closes_the_editor", async ({ app, mock }) => {
    await mock.eval("(m) => m.queueDialog('/qa/projects/Same.grooviq')");
    await app.keyboard.press("ControlOrMeta+o");
    await expect.poll(() => mock.view().then((v) => v.path)).toBe("/qa/projects/Same.grooviq");
    await open(app);
    await control(app, "filter_cutoff").hover();
    await app.mouse.wheel(0, -100);
    await mock.eval("(m) => m.queueDialog('/qa/projects/Same.grooviq')");
    await app.keyboard.press("ControlOrMeta+o");
    await expect(editor(app)).toHaveCount(0);
    await app.waitForTimeout(600);
    expect(await paramEdits(mock)).toEqual([]);
    expect((await stored(mock)).synth).toEqual(presetValues("808 Glide"));
  });
});

/** The kit plus two synth channels, "Bass" (3) and "Lead" (4). */
function twoSynths(): MockProject {
  const p = withSynth();
  p.channels.push({ ...p.channels[2], id: 4, name: "Lead", color: 4, synth: presetValues("Trap Lead"), preset: "Trap Lead" });
  p.nextId = 5;
  return p;
}

test.describe("playing from an editor", () => {
  test.use({ seed: { project: twoSynths(), files: soundsFor(KIT) } });
  const noteCalls = async (mock: Mock) => (await mock.calls("channel_note")).map((c) => [c.args.channel, c.args.key, c.args.on]);

  test("the_typing_keyboard_plays_the_focused_editors_channel", async ({ app, mock }) => {
    await app.getByRole("button", { name: "Edit the Bass synth" }).click();
    await app.getByRole("button", { name: "Edit the Lead synth" }).evaluate((b: HTMLElement) => b.click());
    const bass = app.getByRole("dialog", { name: "Bass synth" });
    const lead = app.getByRole("dialog", { name: "Lead synth" });
    await bass.locator('[data-param="filter_cutoff"]').focus();
    await app.keyboard.down("z");
    await expect.poll(() => noteCalls(mock)).toEqual([[3, 48, true]]);
    // Focus moves to the other editor: the held note ends on Bass, the next plays on Lead.
    await lead.locator('[data-param="filter_cutoff"]').focus();
    await expect.poll(() => noteCalls(mock)).toEqual([
      [3, 48, true],
      [3, 48, false],
    ]);
    await app.keyboard.up("z");
    await app.keyboard.press("x");
    await expect.poll(() => noteCalls(mock)).toEqual([
      [3, 48, true],
      [3, 48, false],
      [4, 50, true],
      [4, 50, false],
    ]);
    expect(await mock.eval<string[]>("(m) => m.live()")).toEqual([]);
  });

  test("space_plays_and_stops_from_inside_an_editor", async ({ app, mock }) => {
    await open(app);
    await control(app, "filter_cutoff").focus();
    await app.keyboard.press(" ");
    await expect.poll(async () => (await mock.calls("transport_play")).length).toBe(1);
    await app.keyboard.press(" ");
    await expect.poll(async () => (await mock.calls("transport_stop")).length).toBe(1);
    // On a preset button, Space plays too and does not load the preset.
    await editor(app).getByRole("button", { name: "Reese", exact: true }).focus();
    await app.keyboard.press(" ");
    await expect.poll(async () => (await mock.calls("transport_play")).length).toBe(2);
    expect((await mock.calls("project_edit")).length).toBe(0);
  });

  test("a_text_input_keeps_its_keys_while_an_editor_is_open", async ({ app, mock }) => {
    await open(app);
    await app.getByRole("textbox", { name: "BPM" }).focus();
    await app.keyboard.press("z");
    await app.keyboard.press(" ");
    await app.waitForTimeout(200);
    expect(await noteCalls(mock)).toEqual([]);
    expect(await mock.calls("transport_play")).toEqual([]);
  });
});

test.describe("stepped parameters", () => {
  test.use({ seed: { project: withSynth(), files: soundsFor(KIT) } });

  test("stepped_drags_preview_and_commit_whole_values", async ({ app, mock }) => {
    await open(app);
    const oct = control(app, "osc1_octave");
    const at = await centre(oct);
    await app.mouse.move(at.x, at.y);
    await app.mouse.down();
    // Down through the negative half steps, a few pixels at a time. The preview goes out at most
    // once per animation frame and a release drops an unsent one, so the drag has to span a frame:
    // wait for the first live value (as a real drag does) before it carries on and lets go.
    for (let i = 1; i <= 15; i++) await app.mouse.move(at.x, at.y + i * 2);
    await expect.poll(async () => (await liveCalls(mock, "osc1_octave")).length).toBeGreaterThan(0);
    for (let i = 16; i <= 30; i++) await app.mouse.move(at.x, at.y + i * 2);
    await app.mouse.up();
    await expect.poll(() => paramEdits(mock)).toHaveLength(1);
    const lives = (await liveCalls(mock, "osc1_octave")).map((c) => c.args.value as number);
    expect(lives.length).toBeGreaterThan(0);
    for (const v of lives) expect(Number.isInteger(v), `live ${v}`).toBe(true);
    const [[, value]] = await paramEdits(mock);
    expect(Number.isInteger(value)).toBe(true);
    expect(value).toBeLessThan(-1);
    await expect(oct).toHaveAttribute("aria-valuenow", String(value));
  });
});

/** Live values sent for `param` since the call log had `from` entries. */
const livesSince = async (mock: Mock, param: string, from: number) =>
  (await mock.calls()).slice(from).filter((c) => c.cmd === "channel_param_live" && c.args.param === param).map((c) => c.args.value as number);
const blur = (page: Page) => page.evaluate(() => window.dispatchEvent(new Event("blur")));
const pressReese = (page: Page) => editor(page).getByRole("button", { name: "Reese", exact: true }).evaluate((b: HTMLElement) => b.click());

test.describe("graph drags after the editor ends them", () => {
  test.use({ seed: { project: withSynth(), files: soundsFor(KIT) } });

  test("a_filter_drag_cancelled_by_blur_does_not_start_its_other_parameter", async ({ app, mock }) => {
    await open(app);
    const at = await centre(editor(app).locator(".filter-display"));
    await app.mouse.move(at.x, at.y);
    await app.mouse.down();
    await app.mouse.move(at.x + 40, at.y, { steps: 4 });
    await expect.poll(() => liveValue(mock, "filter_cutoff")).toBeGreaterThan(650);
    await blur(app);
    await expect.poll(() => liveValue(mock, "filter_cutoff")).toBe(650);
    const from = (await mock.calls()).length;
    // Now only up and down: resonance was never part of this drag before it ended.
    await app.mouse.move(at.x + 40, at.y - 40, { steps: 4 });
    await app.mouse.up();
    await twoFrames(app);
    expect(await livesSince(mock, "filter_res", from)).toEqual([]);
    expect(await livesSince(mock, "filter_cutoff", from)).toEqual([]);
    expect(await paramEdits(mock)).toEqual([]);
    expect((await stored(mock)).synth).toMatchObject({ filter_cutoff: 650, filter_res: 0.05 });
  });

  test("a_filter_drag_cut_short_by_a_preset_leaves_the_preset", async ({ app, mock }) => {
    await open(app);
    const at = await centre(editor(app).locator(".filter-display"));
    await app.mouse.move(at.x, at.y);
    await app.mouse.down();
    await app.mouse.move(at.x + 40, at.y, { steps: 4 });
    await expect.poll(() => liveValue(mock, "filter_cutoff")).toBeGreaterThan(650);
    await pressReese(app);
    await expect.poll(async () => (await stored(mock)).preset).toBe("Reese");
    await app.mouse.move(at.x + 40, at.y - 40, { steps: 4 });
    await app.mouse.up();
    await app.waitForTimeout(200);
    expect(await paramEdits(mock)).toEqual([]);
    expect((await stored(mock)).synth).toEqual(presetValues("Reese"));
    expectShown(await shown(app), presetValues("Reese"));
  });

  test("an_envelope_point_cancelled_by_blur_does_not_start_its_other_parameter", async ({ app, mock }) => {
    await open(app);
    const graph = editor(app).getByRole("img", { name: "Amp envelope" });
    const pt = await graph.evaluate((svg) => {
      const c = svg.querySelectorAll("circle")[1].getBoundingClientRect();
      return { x: c.x + c.width / 2, y: c.y + c.height / 2 };
    });
    await app.mouse.move(pt.x, pt.y);
    await app.mouse.down();
    await app.mouse.move(pt.x, pt.y - 30, { steps: 4 });
    await expect.poll(() => liveValue(mock, "amp_sustain")).toBeGreaterThan(0.12);
    await blur(app);
    await expect.poll(() => liveValue(mock, "amp_sustain")).toBeCloseTo(0.12, 6);
    const from = (await mock.calls()).length;
    // Now sideways only: the decay time.
    await app.mouse.move(pt.x - 40, pt.y - 30, { steps: 4 });
    await app.mouse.up();
    await twoFrames(app);
    expect(await livesSince(mock, "amp_decay", from)).toEqual([]);
    expect(await livesSince(mock, "amp_sustain", from)).toEqual([]);
    expect(await paramEdits(mock)).toEqual([]);
    expect((await stored(mock)).synth).toMatchObject({ amp_decay: 4500, amp_sustain: 0.12 });
  });

  test("a_graph_drag_cancelled_before_it_moves_stays_dead", async ({ app, mock }) => {
    await open(app);
    const from = (await mock.calls()).length;
    const at = await centre(editor(app).locator(".filter-display"));
    await app.mouse.move(at.x, at.y);
    await app.mouse.down();
    await blur(app);
    await app.mouse.move(at.x + 40, at.y - 40, { steps: 4 });
    await app.mouse.up();
    // The same for an envelope point.
    const pt = await editor(app)
      .getByRole("img", { name: "Amp envelope" })
      .evaluate((svg) => {
        const c = svg.querySelectorAll("circle")[1].getBoundingClientRect();
        return { x: c.x + c.width / 2, y: c.y + c.height / 2 };
      });
    await app.mouse.move(pt.x, pt.y);
    await app.mouse.down();
    await blur(app);
    await app.mouse.move(pt.x - 30, pt.y - 30, { steps: 4 });
    await app.mouse.up();
    await twoFrames(app);
    const sent = (await mock.calls()).slice(from).filter((c) => c.cmd === "channel_param_live" || c.cmd === "project_edit");
    expect(sent).toEqual([]);
  });
});

test.describe("edits while a replacement is on its way", () => {
  test.use({
    seed: {
      project: withSynth(),
      files: { ...soundsFor(KIT), "/qa/projects/Other.grooviq": { kind: "project", project: replacement() } },
      recovery: { savedAt: 1_794_860_000, original: "/qa/projects/Other.grooviq", project: replacement() },
    },
  });

  /** Opens the editor past the recovery prompt (it stays up behind) and presses keys on cutoff. */
  async function openUnderPrompt(page: Page) {
    await opener(page).evaluate((b: HTMLElement) => b.click());
    await expect(editor(page)).toBeVisible();
  }
  const sentAfter = async (mock: Mock, cmd: string) => {
    const calls = await mock.calls();
    const at = calls.findIndex((c) => c.cmd === cmd);
    return calls.slice(at + 1).filter((c) => c.cmd === "channel_param_live" || (c.cmd === "project_edit" && (c.args.edit as { kind: string }).kind === "setInstrumentParam"));
  };

  /** Every project edit and live value sent after `cmd`. */
  const anythingAfter = async (mock: Mock, cmd: string) => {
    const calls = await mock.calls();
    const at = calls.findIndex((c) => c.cmd === cmd);
    return calls.slice(at + 1).filter((c) => c.cmd === "channel_param_live" || c.cmd === "project_edit");
  };
  /** Opens the editor, then starts `cmd` and holds it pending. */
  async function startPending(page: Page, mock: Mock, cmd: string, start: string) {
    await openUnderPrompt(page);
    await mock.eval(`(m) => m.holdCommand("${cmd}")`);
    if (start) {
      await page.getByRole("button", { name: "Discard" }).click();
      await openUnderPrompt(page);
      await mock.eval("(m) => m.queueDialog('/qa/projects/Other.grooviq')");
      await page.keyboard.press(start);
    } else {
      await page.getByRole("dialog", { name: "Restore your project?" }).getByRole("button", { name: "Restore" }).click();
    }
    await expect.poll(async () => (await mock.calls(cmd)).length).toBe(1);
  }

  for (const [name, cmd, start] of [
    ["open", "project_open", "ControlOrMeta+o"],
    ["new", "project_new", "ControlOrMeta+n"],
    ["restore", "recovery_restore", ""],
  ] as const) {
    test(`a_preset_chosen_while_${name}_is_pending_never_reaches_the_new_project`, async ({ app, mock }) => {
      await startPending(app, mock, cmd, start);
      // A's editor is still up while B is on its way: choosing a preset there does nothing.
      await editor(app).getByRole("button", { name: "Trap Lead", exact: true }).evaluate((b: HTMLElement) => b.click());
      await app.waitForTimeout(200);
      await mock.eval(`(m) => m.releaseCommand("${cmd}")`);
      await expect(editor(app)).toHaveCount(0);
      await app.waitForTimeout(400);
      expect(await anythingAfter(mock, cmd)).toEqual([]);
      const b = await mock.view();
      if (name === "new") expect(b.channels).toEqual([]);
      else expect(await stored(mock)).toMatchObject({ preset: "Reese", synth: presetValues("Reese") });
    });

    test(`a_key_pressed_while_${name}_is_pending_never_reaches_the_new_project`, async ({ app, mock }) => {
      await startPending(app, mock, cmd, start);
      // A's editor is still up while B is on its way: its keys and drags change nothing.
      const knob = control(app, "filter_cutoff");
      await knob.focus();
      await app.keyboard.press("ArrowUp");
      await app.keyboard.press("ArrowUp");
      await dragUp(app, knob, 30);
      // It takes no input at all: the knob does not move.
      await expect(knob).toHaveAttribute("aria-valuenow", "650");
      await app.mouse.up();
      await app.waitForTimeout(200);
      await expect(knob).toHaveAttribute("aria-valuenow", "650");
      await mock.eval(`(m) => m.releaseCommand("${cmd}")`);
      await expect(editor(app)).toHaveCount(0);
      await app.waitForTimeout(400);
      expect(await sentAfter(mock, cmd)).toEqual([]);
      const b = await mock.view();
      if (name === "new") expect(b.channels).toEqual([]);
      else expect((await stored(mock)).synth).toEqual(presetValues("Reese"));
    });
  }

  test("a_failed_open_leaves_the_editor_usable", async ({ app, mock }) => {
    await app.getByRole("button", { name: "Discard" }).click();
    await open(app);
    await mock.eval("(m) => m.queueDialog('/qa/projects/Missing.grooviq')");
    await app.keyboard.press("ControlOrMeta+o");
    await expect.poll(async () => (await mock.calls("project_open")).length).toBe(1);
    await app.waitForTimeout(200);
    await expect(editor(app)).toBeVisible();
    await control(app, "filter_cutoff").focus();
    await app.keyboard.press("ArrowUp");
    await expect.poll(() => paramEdits(mock)).toHaveLength(1);
    expect((await stored(mock)).synth.filter_cutoff).toBeGreaterThan(650);
    // Presets too.
    await editor(app).getByRole("button", { name: "Trap Lead", exact: true }).click();
    await expect.poll(async () => (await stored(mock)).preset).toBe("Trap Lead");
  });
});
