// M2 Task 14 (#85): the sampler editor, opened from a sampler channel in the rack as a floating
// window in the synth editor's visual language. Assertions read the mock session's command log
// and state; ranges and defaults come from the engine's parameter fixture.
import type { Locator, Page, TestInfo } from "@playwright/test";
import { readFileSync } from "node:fs";
import { colourDistance, expect, pixelsAt, project, soundsFor, test, type Mock } from "./fixture";
import type { MockProject } from "./mockTauri";

const KIT = ["Kick", "Snare", "Hat"];
const KICK = 1;
const HAT = 3;

/** The Kick holds a looping region; the Hat's sample is missing from the project file. */
function kit(): MockProject {
  const p = project(KIT, { 1: [0, 8] });
  p.channels[0].sampler = { start: 0.1, end: 0.9, loop: 1, loop_start: 0.5, root: 48, tune: -3, filter_type: 1, cutoff: 2000, voice_mode: 1, glide: 120 };
  p.channels[0].choke = 0;
  p.channels[2].embedded = false;
  return p;
}

const PARAMS = (
  JSON.parse(readFileSync(new URL("../../../tests/fixtures/device-params.json", import.meta.url), "utf8")) as { sampler: { id: string; min: number; max: number; default: number; stepped: boolean }[] }
).sampler;
const spec = (id: string) => PARAMS.find((p) => p.id === id)!;

const opener = (page: Page, name: string) => page.getByRole("button", { name: `Edit the ${name} sampler` });
const editor = (page: Page, name = "Kick") => page.getByRole("dialog", { name: `${name} sampler` });
const control = (page: Page, id: string) => editor(page).locator(`[data-param="${id}"]`);
const waveform = (page: Page, name = "Kick") => editor(page, name).getByRole("img", { name: `${name} waveform` });
const choke = (page: Page) => editor(page).getByRole("radiogroup", { name: "Choke group" });

async function open(page: Page, name = "Kick") {
  await opener(page, name).click();
  await expect(editor(page, name)).toBeVisible();
}

/** What every parameter control shows, by id: a slider's value, an enum's checked button, a
 * switch's state (1 on, 0 off). */
async function shown(page: Page): Promise<Record<string, number>> {
  const raw = await editor(page)
    .locator("[data-param]")
    .evaluateAll((els) =>
      els.map((el) => {
        const id = el.getAttribute("data-param")!;
        const role = el.getAttribute("role");
        if (role === "radiogroup") {
          const radios = [...el.querySelectorAll("[role='radio']")];
          return [id, radios.findIndex((r) => r.getAttribute("aria-checked") === "true"), "enum"] as const;
        }
        if (role === "switch") return [id, el.getAttribute("aria-checked") === "true" ? 1 : 0, "switch"] as const;
        return [id, Number(el.getAttribute("aria-valuenow")), "value"] as const;
      }),
    );
  return Object.fromEntries(raw.map(([id, v, kind]) => [id, kind === "enum" ? v + spec(id).min : v]));
}

/** The Kick's stored values and choke group in the mock project. */
const stored = async (mock: Mock, id = KICK) =>
  (await mock.eval<{ channels: { id: number; sampler: Record<string, number>; choke: number }[] }>("(m) => m.view()")).channels.find((c) => c.id === id)!;
const edits = async (mock: Mock) =>
  (await mock.calls("project_edit")).map((c) => c.args.edit as { kind: string; channel: number; param?: string; value?: number; group?: number });
const paramEdits = async (mock: Mock) =>
  (await edits(mock)).filter((e) => e.kind === "setInstrumentParam").map((e) => [e.param, e.value] as const);
const liveValue = (mock: Mock, param: string) => mock.eval<number | undefined>(`(m) => m.state.liveParams.get("${KICK}:${param}")`);
const waveformCalls = async (mock: Mock, channel: number) => (await mock.calls("channel_waveform")).filter((c) => c.args.channel === channel);

const centre = async (el: Locator) => {
  const b = (await el.boundingBox())!;
  return { x: b.x + b.width / 2, y: b.y + b.height / 2 };
};

/** Drags `handle` sideways to `x` (page px) in steps, then releases. */
async function dragTo(page: Page, handle: Locator, x: number) {
  const from = await centre(handle);
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  for (let i = 1; i <= 8; i++) await page.mouse.move(from.x + ((x - from.x) * i) / 8, from.y);
}

/** Lets two frames go by (whatever a key or a render would send has gone out). */
const frames = (page: Page) => page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));

async function shot(page: Page, info: TestInfo, name: string, dialog = editor(page)) {
  await info.attach(`${name}-aria`, { body: await dialog.ariaSnapshot(), contentType: "text/yaml" });
  await page.screenshot({ path: info.outputPath(`${name}.png`) });
}

test.describe("the sampler editor", () => {
  test.use({ seed: { project: kit(), files: soundsFor(KIT) } });

  test("every_sampler_parameter_has_a_control", async ({ app, mock }, info) => {
    await open(app);
    await expect(waveform(app)).toBeVisible();
    await shot(app, info, "sampler-editor");
    expect(PARAMS.length).toBe(29);
    for (const p of PARAMS) {
      const c = control(app, p.id);
      await expect(c, p.id).toHaveCount(1);
      await expect(c, p.id).toBeVisible();
      expect(await c.getAttribute("data-hint"), p.id).toBeTruthy();
    }
    // Each one shows the channel's stored value (a switch is on at 0.5 and up, as in the engine).
    const values = await shown(app);
    const want = (await stored(mock)).sampler;
    expect(Object.keys(values).sort()).toEqual(PARAMS.map((p) => p.id).sort());
    for (const p of PARAMS) {
      const isSwitch = (await control(app, p.id).getAttribute("role")) === "switch";
      expect(values[p.id], p.id).toBeCloseTo(isSwitch ? Number(want[p.id] >= 0.5) : want[p.id], 3);
    }
    expect(values.root).toBe(48);
    expect(values.loop_start).toBeCloseTo(0.5, 3);
    // The choke group is there too, showing "none".
    await expect(choke(app)).toBeVisible();
    await expect(choke(app).getByRole("radio", { checked: true })).toHaveAccessibleName("Off");

    // A knob change goes to the sampler as one parameter edit.
    await control(app, "gain").focus();
    await app.keyboard.press("End");
    await expect.poll(() => paramEdits(mock)).toEqual([["gain", spec("gain").max]]);
    // A switch flips with a click and stores 0 or 1.
    await control(app, "reverse").click();
    await expect.poll(() => paramEdits(mock)).toEqual([
      ["gain", spec("gain").max],
      ["reverse", 1],
    ]);
    await expect(control(app, "reverse")).toHaveAttribute("aria-checked", "true");
    // The window fits the app window (design size 880 x 520).
    const box = (await app.locator(".device-window").boundingBox())!;
    expect(box.width).toBeLessThanOrEqual(880);
    expect(box.y + box.height).toBeLessThanOrEqual(800);
  });

  test("the_choke_group_picker_sends_set_choke", async ({ app, mock }) => {
    await open(app);
    await choke(app).getByRole("radio", { name: "3", exact: true }).click();
    await expect.poll(async () => (await stored(mock)).choke).toBe(3);
    await expect(choke(app).getByRole("radio", { checked: true })).toHaveAccessibleName("3");
    // The arrow keys move along the groups; Home is "none".
    await app.keyboard.press("ArrowRight");
    await expect.poll(async () => (await stored(mock)).choke).toBe(4);
    await app.keyboard.press("Home");
    await expect.poll(async () => (await stored(mock)).choke).toBe(0);
    await choke(app).getByRole("radio", { name: "16", exact: true }).click();
    await expect.poll(async () => (await stored(mock)).choke).toBe(16);

    // Every change is one SetChoke for this channel; no parameter edit.
    const sent = await edits(mock);
    expect(sent.map((e) => [e.kind, e.channel, e.group])).toEqual([
      ["setChoke", KICK, 3],
      ["setChoke", KICK, 4],
      ["setChoke", KICK, 0],
      ["setChoke", KICK, 16],
    ]);
  });

  test("start_end_and_loop_markers_drag_and_stay_ordered", async ({ app, mock }, info) => {
    await open(app);
    const well = (await waveform(app).boundingBox())!;
    const start = control(app, "start");
    const end = control(app, "end");
    const loop = control(app, "loop_start");

    // The start marker dragged far past the end: live while it moves, one edit at release, and
    // it stops short of the end.
    await dragTo(app, start, well.x + well.width * 0.98);
    await expect.poll(() => liveValue(mock, "start")).toBeGreaterThan(0.5);
    expect(await paramEdits(mock)).toEqual([]);
    await app.mouse.up();
    await expect.poll(async () => (await paramEdits(mock)).length).toBe(1);
    let s = (await stored(mock)).sampler;
    expect((await paramEdits(mock))[0][0]).toBe("start");
    expect(s.start).toBeGreaterThan(0.85);
    expect(s.start).toBeLessThan(s.end);
    expect(s.end).toBe(0.9);

    // Home takes the start back to the beginning; then the end marker dragged far before the
    // start stops after it.
    await start.focus();
    await app.keyboard.press("Home");
    await expect.poll(async () => (await stored(mock)).sampler.start).toBe(0);
    await dragTo(app, end, well.x - 40);
    await app.mouse.up();
    await expect.poll(async () => (await paramEdits(mock)).length).toBe(3);
    s = (await stored(mock)).sampler;
    expect(s.end).toBeGreaterThan(s.start);
    expect(s.end).toBeLessThan(0.01);

    // The keys go as far as the other marker allows.
    await end.focus();
    await app.keyboard.press("End");
    await expect.poll(async () => (await stored(mock)).sampler.end).toBe(1);
    await start.focus();
    await app.keyboard.press("End");
    await expect.poll(async () => (await stored(mock)).sampler.start).toBeGreaterThan(0.99);
    s = (await stored(mock)).sampler;
    expect(s.start).toBeLessThan(s.end);
    await app.keyboard.press("Home");
    await expect.poll(async () => (await stored(mock)).sampler.start).toBe(0);

    // The loop marker dragged past either end stays inside the played region.
    await dragTo(app, end, well.x + well.width * 0.6);
    await app.mouse.up();
    await expect.poll(async () => (await stored(mock)).sampler.end).toBeLessThan(0.7);
    for (const x of [well.x + well.width * 0.99, well.x - 40, well.x + well.width * 0.3]) {
      await dragTo(app, loop, x);
      await app.mouse.up();
      // Where each marker's line is, across the waveform (its grip hangs to one side of it).
      const [sx, ex, lx] = await Promise.all([start, end, loop].map((h) => h.evaluate((el) => parseFloat(getComputedStyle(el).left))));
      expect(lx).toBeGreaterThanOrEqual(sx - 0.5);
      expect(lx).toBeLessThanOrEqual(ex + 0.5);
    }
    await expect.poll(async () => (await stored(mock)).sampler.loop_start).toBeCloseTo(0.5, 1);
    s = (await stored(mock)).sampler;
    expect(s.loop_start).toBeGreaterThanOrEqual(0);
    expect(s.loop_start).toBeLessThanOrEqual(1);
    const history = (await paramEdits(mock)).filter(([id]) => id === "loop_start").map(([, v]) => v!);
    expect(history.length).toBe(3);
    expect(history[0]).toBe(1);
    expect(history[1]).toBe(0);
    await shot(app, info, "sampler-markers");

    // A drag cut short (the pointer is cancelled) sends no edit and puts the engine back.
    const before = (await paramEdits(mock)).length;
    await dragTo(app, start, well.x + well.width * 0.3);
    await expect.poll(() => liveValue(mock, "start")).toBeGreaterThan(0.1);
    await start.dispatchEvent("pointercancel", { pointerId: 1 });
    await expect.poll(() => liveValue(mock, "start")).toBe(0);
    await app.mouse.up();
    expect((await paramEdits(mock)).length).toBe(before);
    expect((await stored(mock)).sampler.start).toBe(0);
  });

  test("the_waveform_is_fetched_once_per_sample_and_redrawn_on_resize", async ({ app, mock }) => {
    await open(app);
    const canvas = waveform(app);
    await expect(canvas).toBeVisible();
    await expect.poll(async () => (await waveformCalls(mock, KICK)).length).toBe(1);

    // Drawn: the centre line is inked (the mock's sample decays from full scale), the top of the
    // quiet tail is not.
    const drawn = async () => {
      const b = (await canvas.boundingBox())!;
      const [loud, mid, quiet] = await pixelsAt(app, canvas, [
        { x: b.width * 0.15, y: b.height * 0.3 },
        { x: b.width * 0.42, y: b.height * 0.5 },
        { x: b.width * 0.88, y: b.height * 0.12 },
      ]);
      return colourDistance(loud, quiet) > 60 && colourDistance(mid, quiet) > 60;
    };
    await expect.poll(drawn).toBe(true);
    const backing = () => canvas.evaluate((c: HTMLCanvasElement) => c.width);
    const wide = await backing();

    // A smaller app window scales the editor down: the waveform is drawn again at its new size
    // from the peaks it already has.
    await app.setViewportSize({ width: 760, height: 520 });
    await expect.poll(backing).toBeLessThan(wide);
    await expect.poll(drawn).toBe(true);
    await app.setViewportSize({ width: 1280, height: 800 });
    await expect.poll(backing).toBe(wide);
    await expect.poll(drawn).toBe(true);

    // Parameter changes redraw the markers, not the sample.
    await control(app, "reverse").click();
    await control(app, "end").focus();
    await app.keyboard.press("ArrowLeft");
    await expect.poll(async () => (await paramEdits(mock)).length).toBe(2);
    expect((await waveformCalls(mock, KICK)).length).toBe(1);
    // Each fetch asks for a fixed number of points, whatever the window's size.
    const asked = (await waveformCalls(mock, KICK)).map((c) => c.args.points as number);
    expect(asked[0]).toBeGreaterThanOrEqual(1024);
  });

  test("a_missing_sample_shows_relink_instead_of_a_waveform", async ({ app, mock }, info) => {
    await open(app, "Hat");
    const hat = editor(app, "Hat");
    const relink = hat.getByRole("button", { name: "Relink Hat" });
    await expect(relink).toBeVisible();
    await expect(waveform(app, "Hat")).toHaveCount(0);
    await shot(app, info, "sampler-missing", hat);
    // Nothing is asked of the engine for a sample it does not have.
    expect(await waveformCalls(mock, HAT)).toEqual([]);
    // Every parameter is still there to edit.
    for (const p of PARAMS) await expect(hat.locator(`[data-param="${p.id}"]`), p.id).toHaveCount(1);

    // A cancelled pick changes nothing.
    await mock.eval("(m) => m.queueDialog(null)");
    await relink.click();
    await expect.poll(async () => (await mock.calls("plugin:dialog|open")).length).toBe(1);
    expect(await mock.calls("channel_set_sample")).toEqual([]);
    await expect(relink).toBeVisible();

    // Relinking asks for the file, points the channel at it and draws it, fetched once.
    await mock.eval("(m) => m.queueDialog('/qa/sounds/Hat.wav')");
    await relink.click();
    await expect.poll(async () => (await mock.calls("channel_set_sample")).map((c) => c.args)).toEqual([{ channel: HAT, path: "/qa/sounds/Hat.wav" }]);
    await expect(waveform(app, "Hat")).toBeVisible();
    await expect(relink).toHaveCount(0);
    await expect.poll(async () => (await waveformCalls(mock, HAT)).length).toBe(1);
  });

  test("enums_switch_and_send_their_values", async ({ app, mock }) => {
    await open(app);
    const filter = control(app, "filter_type");
    await filter.getByRole("radio", { name: "HP", exact: true }).click();
    await expect(filter.getByRole("radio", { checked: true })).toHaveAccessibleName("HP");
    await app.keyboard.press("ArrowLeft");
    await expect(filter.getByRole("radio", { checked: true })).toHaveAccessibleName("LP");
    await control(app, "voice_mode").getByRole("radio", { name: "Legato", exact: true }).click();
    await expect.poll(() => paramEdits(mock)).toEqual([
      ["filter_type", 2],
      ["filter_type", 1],
      ["voice_mode", 2],
    ]);
    expect((await stored(mock)).sampler).toMatchObject({ filter_type: 1, voice_mode: 2 });
  });

  /** The centre of a point (a circle) of a graph, in page px. */
  const point = (graph: Locator, i: number) =>
    graph.evaluate((svg, i) => {
      const c = svg.querySelectorAll("circle")[i].getBoundingClientRect();
      return { x: c.x + c.width / 2, y: c.y + c.height / 2 };
    }, i);

  test("envelope_points_preview_live_and_commit_each_changed_parameter_once", async ({ app, mock }) => {
    await open(app);
    // The amp envelope's decay point (decay time and sustain level): right and down.
    const amp = editor(app).getByRole("img", { name: "Amp envelope" });
    let pt = await point(amp, 1);
    await app.mouse.move(pt.x, pt.y);
    await app.mouse.down();
    await app.mouse.move(pt.x + 15, pt.y + 20, { steps: 4 });
    await app.mouse.move(pt.x + 30, pt.y + 40, { steps: 4 });
    await expect.poll(() => liveValue(mock, "sustain")).toBeLessThan(1);
    await expect.poll(() => liveValue(mock, "decay")).toBeGreaterThan(500);
    expect(await paramEdits(mock)).toEqual([]);
    await app.mouse.up();
    await expect.poll(async () => (await paramEdits(mock)).length).toBe(2);
    const edits1 = Object.fromEntries(await paramEdits(mock));
    expect(Object.keys(edits1).sort()).toEqual(["decay", "sustain"]);
    expect(edits1.decay).toBeGreaterThan(500);
    expect(edits1.sustain).toBeLessThan(1);
    expect((await stored(mock)).sampler).toMatchObject({ decay: edits1.decay, sustain: edits1.sustain, attack: 0, release: 40 });

    // The filter envelope's release point, to the right: only its release changes.
    const flt = editor(app).getByRole("img", { name: "Filter envelope" });
    pt = await point(flt, 2);
    await app.mouse.move(pt.x, pt.y);
    await app.mouse.down();
    await app.mouse.move(pt.x + 30, pt.y, { steps: 6 });
    await expect.poll(() => liveValue(mock, "flt_release")).toBeGreaterThan(40);
    await app.mouse.up();
    await expect.poll(async () => (await paramEdits(mock)).length).toBe(3);
    const [param, value] = (await paramEdits(mock))[2];
    expect(param).toBe("flt_release");
    expect(value).toBeGreaterThan(40);

    // A drag the pointer cancels puts the engine back and sends nothing.
    pt = await point(amp, 0);
    await app.mouse.move(pt.x, pt.y);
    await app.mouse.down();
    await app.mouse.move(pt.x + 30, pt.y, { steps: 6 });
    await expect.poll(() => liveValue(mock, "attack")).toBeGreaterThan(0);
    await amp.locator("..").dispatchEvent("pointercancel", { pointerId: 1, bubbles: true });
    await expect.poll(() => liveValue(mock, "attack")).toBe(0);
    await app.mouse.up();
    expect(await paramEdits(mock)).toHaveLength(3);
    expect((await stored(mock)).sampler.attack).toBe(0);
  });

  test("the_filter_graph_drags_cutoff_and_resonance", async ({ app, mock }) => {
    await open(app);
    const graph = editor(app).getByRole("img", { name: "Filter response" });
    const at = await centre(graph);
    await app.mouse.move(at.x, at.y);
    await app.mouse.down();
    await app.mouse.move(at.x + 40, at.y - 30, { steps: 6 });
    await expect.poll(() => liveValue(mock, "cutoff")).toBeGreaterThan(2000);
    await expect.poll(() => liveValue(mock, "res")).toBeGreaterThan(0);
    expect(await paramEdits(mock)).toEqual([]);
    await app.mouse.up();
    await expect.poll(async () => (await paramEdits(mock)).length).toBe(2);
    const sent = Object.fromEntries(await paramEdits(mock));
    expect(Object.keys(sent).sort()).toEqual(["cutoff", "res"]);
    expect(sent.cutoff).toBeGreaterThan(2000);
    expect(sent.res).toBeGreaterThan(0);
    await expect(control(app, "cutoff")).toHaveAttribute("aria-valuenow", String(Number(sent.cutoff!.toFixed(4))));

    // Cancelled mid-drag: both back, no edit.
    await app.mouse.move(at.x, at.y);
    await app.mouse.down();
    await app.mouse.move(at.x - 40, at.y + 20, { steps: 6 });
    await expect.poll(() => liveValue(mock, "cutoff")).toBeLessThan(sent.cutoff!);
    await graph.locator("..").dispatchEvent("pointercancel", { pointerId: 1, bubbles: true });
    await expect.poll(() => liveValue(mock, "cutoff")).toBeCloseTo(sent.cutoff!, 3);
    await app.mouse.up();
    expect(await paramEdits(mock)).toHaveLength(2);
  });
});

test.describe("a narrow region", () => {
  // 9.6 frames of a 48,000-frame sample: valid for the engine, which only widens a region
  // shorter than one frame (and then plays something other than what the editor shows).
  const FRAMES = 48_000;
  const p = project(["Kick"]);
  p.channels[0].sampler = { start: Math.fround(0.1), end: Math.fround(0.1002), loop: 1, loop_start: 0.5 };
  p.channels[0].frames = FRAMES;
  /** Frames between two region ends, counted as the engine counts them. */
  const between = (start: number, end: number) => end * FRAMES - start * FRAMES;
  test.use({ seed: { project: p, files: soundsFor(["Kick"]) } });

  test("a_narrow_region_is_kept_and_nudged_inwards", async ({ app, mock }) => {
    await open(app);
    const start = control(app, "start");
    const end = control(app, "end");
    // Opening it and pressing the markers without moving them changes nothing.
    await start.click();
    await end.click();
    await expect(start).toHaveAttribute("aria-valuenow", "0.1");
    expect(await paramEdits(mock)).toEqual([]);

    // The keys move each marker the way they point, never past the other one. A step (0.1% of
    // the sample, 48 frames) is wider than this region: the start stops a frame before the end.
    await start.focus();
    await app.keyboard.press("ArrowRight");
    await expect.poll(async () => (await paramEdits(mock)).length).toBe(1);
    let s = (await stored(mock)).sampler;
    expect(s.start).toBeGreaterThan(Math.fround(0.1));
    expect(between(s.start, s.end)).toBeGreaterThanOrEqual(1);
    expect(between(s.start, s.end)).toBeLessThan(1.01);
    expect(s.end).toBe(Math.fround(0.1002));
    // Pushed against each other, an inward nudge has nowhere to go and sends nothing.
    await app.keyboard.press("ArrowRight");
    await end.focus();
    await app.keyboard.press("ArrowLeft");
    await frames(app);
    expect(await paramEdits(mock)).toHaveLength(1);
    // Outwards they move freely; then the end comes back towards the start, staying after it.
    await start.focus();
    await app.keyboard.press("Home");
    await expect.poll(async () => (await stored(mock)).sampler.start).toBe(0);
    await end.focus();
    await app.keyboard.press("ArrowLeft");
    await expect.poll(async () => (await paramEdits(mock)).length).toBe(3);
    s = (await stored(mock)).sampler;
    expect(s.end).toBeLessThan(Math.fround(0.1002));
    expect(s.end).toBeGreaterThan(Math.fround(0.099));
    // End pushed down to the start at 0: it stops one frame in.
    await app.keyboard.press("Home");
    await expect.poll(async () => (await paramEdits(mock)).length).toBe(4);
    s = (await stored(mock)).sampler;
    expect(s.start).toBe(0);
    expect(between(s.start, s.end)).toBeGreaterThanOrEqual(1);
    expect(between(s.start, s.end)).toBeLessThan(1.01);
  });
});

test.describe("a region stored out of order", () => {
  // An older or hand-edited project: loading clamps start and end one by one, and the engine
  // plays either order.
  const p = project(["Kick"]);
  p.channels[0].sampler = { start: Math.fround(0.8), end: Math.fround(0.2) };
  test.use({ seed: { project: p, files: soundsFor(["Kick"]) } });

  test("keys_never_move_a_crossed_marker_the_other_way", async ({ app, mock }) => {
    await open(app);
    // Inwards (towards the other marker's side) there is nowhere to go: nothing is sent.
    await control(app, "start").focus();
    await app.keyboard.press("ArrowRight");
    await app.keyboard.press("End");
    await control(app, "end").focus();
    await app.keyboard.press("ArrowLeft");
    await app.keyboard.press("Home");
    await frames(app);
    expect(await paramEdits(mock)).toEqual([]);
    expect((await stored(mock)).sampler).toMatchObject({ start: Math.fround(0.8), end: Math.fround(0.2) });
    // The way it points, a key puts it back in order.
    await control(app, "start").focus();
    await app.keyboard.press("ArrowLeft");
    await expect.poll(async () => (await paramEdits(mock)).length).toBe(1);
    const s = (await stored(mock)).sampler;
    expect(s.start).toBeLessThan(s.end);
  });
});

test.describe("overlapping relinks", () => {
  const p = project(["Hat"]);
  p.channels[0].embedded = false;
  test.use({ seed: { project: p, files: { "/a/Hat.wav": { kind: "sample" }, "/b/Hat.wav": { kind: "sample" } } } });

  test("each_relinked_sample_is_drawn_from_its_own_peaks", async ({ app, mock }) => {
    await open(app, "Hat");
    const hat = editor(app, "Hat");
    const relink = hat.getByRole("button", { name: "Relink Hat" });
    // Pick /a/Hat.wav and hold its load; while Relink is still there, pick /b/Hat.wav and hold
    // the pick. A loads and is drawn, then B's pick comes back: same file name, another sample.
    await mock.eval("(m) => { m.queueDialog('/a/Hat.wav'); m.holdCommand('channel_set_sample'); }");
    await relink.click();
    await expect.poll(async () => (await mock.calls("channel_set_sample")).length).toBe(1);
    await mock.eval("(m) => { m.queueDialog('/b/Hat.wav'); m.holdCommand('plugin:dialog|open'); }");
    await relink.click();
    await expect.poll(async () => (await mock.calls("plugin:dialog|open")).length).toBe(2);
    await mock.eval("(m) => m.releaseCommand('channel_set_sample')");
    await expect(waveform(app, "Hat")).toBeVisible();
    await expect.poll(async () => (await mock.calls("channel_waveform")).length).toBe(1);
    await mock.eval("(m) => m.releaseCommand('plugin:dialog|open')");
    await expect.poll(() => mock.eval<string>("(m) => m.state.project.channels[0].sample")).toBe("/b/Hat.wav");
    await expect.poll(async () => (await mock.calls("channel_waveform")).length).toBe(2);
    await expect(waveform(app, "Hat")).toBeVisible();
    // Each relink fetched once: nothing more after it settles.
    await frames(app);
    expect(await mock.calls("channel_waveform")).toHaveLength(2);
  });
});

for (const [side, reverse] of [["left", 0], ["right", 0], ["left", 1], ["right", 1]] as const) {
  test.describe(`a region hugging the ${side} edge${reverse ? ", reversed" : ""}`, () => {
    // The other marker is 0.48 of a frame from this edge of a 48,000-frame sample: no place for
    // the edge marker leaves a whole frame, so it cannot move until the other one does.
    const FRAMES = 48_000;
    const p = project(["Kick"]);
    p.channels[0].frames = FRAMES;
    p.channels[0].sampler =
      side === "left"
        ? { start: Math.fround(0.000005), end: Math.fround(0.00001), reverse }
        : { start: Math.fround(0.99999), end: Math.fround(0.999995), reverse };
    test.use({ seed: { project: p, files: soundsFor(["Kick"]) } });

    test("edits_that_leave_less_than_a_frame_are_refused", async ({ app, mock }) => {
      await open(app);
      const before = (await stored(mock)).sampler;
      const edge = control(app, side === "left" ? "start" : "end");
      const other = control(app, side === "left" ? "end" : "start");
      // Every key, and a drag past the sample's edge and back over the other marker.
      await edge.focus();
      for (const key of ["Home", "End", "ArrowLeft", "ArrowRight", "PageUp", "PageDown"]) await app.keyboard.press(key);
      const well = (await waveform(app).boundingBox())!;
      await dragTo(app, edge, side === "left" ? well.x - 30 : well.x + well.width + 30);
      await app.mouse.move(well.x + well.width / 2, well.y + 20, { steps: 4 });
      await app.mouse.up();
      await frames(app);
      expect(await paramEdits(mock)).toEqual([]);
      expect(await liveValue(mock, side === "left" ? "start" : "end")).toBeUndefined();
      expect((await stored(mock)).sampler).toMatchObject({ start: before.start, end: before.end });

      // Once the other marker moves away, the edge marker moves again, and every committed
      // region holds at least a frame.
      await other.focus();
      await app.keyboard.press(side === "left" ? "End" : "Home");
      await expect.poll(async () => (await paramEdits(mock)).length).toBe(1);
      await edge.focus();
      await app.keyboard.press(side === "left" ? "Home" : "End");
      await expect.poll(async () => (await paramEdits(mock)).length).toBe(2);
      const s = (await stored(mock)).sampler;
      expect(side === "left" ? s.start : s.end).toBe(side === "left" ? 0 : 1);
      expect(s.end * FRAMES - s.start * FRAMES).toBeGreaterThanOrEqual(1);
    });
  });
}

test.describe("a relink's length", () => {
  // The seeded length belongs to the missing file; the new one is two seconds long.
  const p = project(["Kick"]);
  p.channels[0].frames = 48_000;
  p.channels[0].embedded = false;
  test.use({ seed: { project: p, files: { "/new/Kick.wav": { kind: "sample", seconds: 2 } } } });

  test("relink_replaces_the_previous_frame_count", async ({ app, mock }) => {
    await open(app);
    expect((await mock.view()).channels[0]).toMatchObject({ missing: true });
    expect(await mock.eval("(m) => m.view().channels[0].sampleFrames")).toBeNull();
    await mock.eval("(m) => m.queueDialog('/new/Kick.wav')");
    await editor(app).getByRole("button", { name: "Relink Kick", exact: true }).click();
    await expect.poll(() => mock.eval("(m) => m.view().channels[0].sampleFrames")).toBe(96_000);
    // The editor now limits the region by the new length: End on Start leaves one frame of it.
    await control(app, "start").focus();
    await app.keyboard.press("End");
    await expect.poll(async () => (await paramEdits(mock)).length).toBe(1);
    const s = (await stored(mock)).sampler;
    expect(s.end * 96_000 - s.start * 96_000).toBeGreaterThanOrEqual(1);
    expect(s.end * 96_000 - s.start * 96_000).toBeLessThan(1.01);
  });
});
