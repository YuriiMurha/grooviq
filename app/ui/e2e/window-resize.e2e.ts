import type { Locator } from "@playwright/test";
import { expect, presetValues, project, soundsFor, test } from "./fixture";

const seed = project(["Kick"]);
seed.channels.push({ id: 2, name: "Bass", color: 3, sample: null, volume: 1, pan: 0, mute: false, solo: false, sampler: {}, kind: "synth", synth: presetValues("808 Glide"), preset: "808 Glide" });
seed.nextId = 3;

// Wait for actual rendered geometry across consecutive frames, rather than sampling the
// pre-resize React tree or treating placement-function output as browser evidence.
async function stable(dialog: Locator) {
  return dialog.evaluate(async (el) => {
    let previous = "";
    let same = 0;
    for (let frame = 0; frame < 120; frame++) {
      await new Promise(requestAnimationFrame);
      const r = el.getBoundingClientRect();
      const current = JSON.stringify([r.x, r.y, r.width, r.height]);
      same = current === previous ? same + 1 : 0;
      previous = current;
      if (same >= 3) return { x: r.x, y: r.y, right: r.right, bottom: r.bottom };
    }
    throw new Error("Device geometry did not settle");
  });
}
async function contained(dialog: Locator) {
  const r = await stable(dialog);
  expect(r.x).toBeGreaterThanOrEqual(0);
  expect(r.y).toBeGreaterThanOrEqual(0);
  expect(r.right).toBeLessThanOrEqual(900);
  expect(r.bottom).toBeLessThanOrEqual(560);
  const blocked = await dialog.locator("button, [role=slider], input, select").evaluateAll((els) => els.filter((el) => {
    const r = el.getBoundingClientRect();
    if (!r.width || !r.height) return false;
    const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
    return r.x < 0 || r.y < 0 || r.right > innerWidth || r.bottom > innerHeight || !hit || !el.contains(hit);
  }).map((el) => el.outerHTML));
  expect(blocked).toEqual([]);
}

test.use({ seed: { project: seed, files: soundsFor(["Kick"]) } });
for (const device of ["Kick sampler", "Bass synth"]) {
  for (const [width, height] of [[1280, 800], [1440, 900], [1920, 1080]]) {
    test(`${device} remains reachable after ${width} resize`, async ({ app, mock }, info) => {
      await app.setViewportSize({ width, height });
      const opener = app.getByRole("button", { name: `Edit the ${device}` });
      await opener.click();
      const dialog = app.getByRole("dialog", { name: device });
      await stable(dialog);
      const param = device.includes("synth") ? "osc1_semi" : "tune";
      const knob = dialog.locator(`[data-param="${param}"]`);
      await knob.focus();
      await app.setViewportSize({ width: 900, height: 560 });
      await stable(dialog);
      await app.screenshot({ path: info.outputPath("before-assert.png") });
      await contained(dialog);
      await expect(knob).toBeFocused();
      await app.screenshot({ path: info.outputPath("compact.png") });
      const before = Number(await knob.getAttribute("aria-valuenow"));
      const box = (await knob.boundingBox())!;
      await app.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      await app.mouse.down();
      await app.mouse.move(box.x + box.width / 2, box.y + box.height / 2 - 24, { steps: 4 });
      await app.mouse.up();
      await expect.poll(async () => Number(await knob.getAttribute("aria-valuenow"))).toBeGreaterThan(before);
      await expect.poll(async () => (await mock.calls("project_edit")).filter(c => (c.args.edit as { param?: string }).param === param).length).toBeGreaterThan(0);
      const channelId = device.includes("synth") ? 2 : 1;
      const modelField = device.includes("synth") ? "synth" : "sampler";
      const value = Number(await knob.getAttribute("aria-valuenow"));
      await expect.poll(() => mock.eval<number>(`(m) => m.view().channels.find(c => c.id === ${channelId}).${modelField}.${param}`)).toBeCloseTo(value, 3);
      if (device.includes("sampler")) {
        const marker = dialog.locator('[data-param="start"]');
        const m = (await marker.boundingBox())!;
        await app.mouse.move(m.x + m.width / 2, m.y + m.height / 2);
        await app.mouse.down();
        await app.mouse.move(m.x + m.width / 2 + 60, m.y + m.height / 2, { steps: 4 });
        await app.mouse.up();
        await expect.poll(() => mock.eval<number>("(m) => m.view().channels[0].sampler.start")).toBeGreaterThan(0);
        await expect.poll(() => mock.eval<number>("(m) => m.view().channels[0].sampler.end ?? 1")).toBe(1);
      }
      for (let i = 0; i < 2; i++) {
        await app.setViewportSize({ width, height });
        await stable(dialog);
        await app.setViewportSize({ width: 900, height: 560 });
        await contained(dialog);
      }
      await app.keyboard.press("Tab");
      expect(await dialog.evaluate(el => el.contains(document.activeElement))).toBe(true);
      await app.keyboard.press("Escape");
      await expect(dialog).toHaveCount(0);
      await expect(opener).toBeFocused();
    });
  }
}

test("deliberate dragging remains partly offscreen; resize cancels stale drag", async ({ app }) => {
  await app.getByRole("button", { name: "Edit the Kick sampler" }).click();
  const dialog = app.getByRole("dialog", { name: "Kick sampler" });
  const bar = dialog.locator(".device-bar");
  let b = (await bar.boundingBox())!;
  await app.mouse.move(b.x + 40, b.y + 12);
  await app.mouse.down();
  await app.mouse.move(20, 40, { steps: 4 });
  expect((await stable(dialog)).x).toBeLessThan(0);
  await app.setViewportSize({ width: 900, height: 560 });
  await contained(dialog);
  const at = await stable(dialog);
  await app.mouse.move(400, 200);
  expect(await stable(dialog)).toEqual(at);
  await app.mouse.up();
  b = (await bar.boundingBox())!;
  await app.mouse.move(b.x + 40, b.y + 12);
  await app.mouse.down();
  await app.mouse.move(10, 40);
  await app.mouse.up();
  expect((await stable(dialog)).x).toBeLessThan(0);
});

test("fresh compact open and cascaded windows fit", async ({ app }) => {
  await app.setViewportSize({ width: 900, height: 560 });
  await app.getByRole("button", { name: "Edit the Kick sampler" }).click();
  await contained(app.getByRole("dialog", { name: "Kick sampler" }));
  await app.keyboard.press("Escape");
  await app.setViewportSize({ width: 1920, height: 1080 });
  await app.getByRole("button", { name: "Edit the Kick sampler" }).click();
  await app.getByRole("button", { name: "Edit the Bass synth" }).click();
  await app.setViewportSize({ width: 900, height: 560 });
  for (const name of ["Kick sampler", "Bass synth"]) {
    const dialog = app.getByRole("dialog", { name });
    const r = await stable(dialog);
    expect(r.right).toBeLessThanOrEqual(900);
    expect(r.bottom).toBeLessThanOrEqual(560);
  }
});
