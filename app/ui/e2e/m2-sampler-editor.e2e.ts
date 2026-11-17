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
});
