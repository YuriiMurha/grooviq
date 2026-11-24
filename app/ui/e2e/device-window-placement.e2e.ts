import { expect, presetValues, project, soundsFor, test, workspaceReady } from "./fixture";

const demo = project(["Kick"]);
demo.channels.push({
  id: 2, name: "Bass", color: 1, sample: null, volume: 1, pan: 0,
  mute: false, solo: false, sampler: {}, kind: "synth",
  synth: presetValues("808 Glide"), preset: "808 Glide",
});
demo.nextId = 3;
test.use({ seed: { project: demo, files: soundsFor(["Kick"]) }, viewport: { width: 900, height: 560 }, reducedMotion: "reduce" });

test("a_second_device_opens_fully_inside_the_compact_viewport", async ({ app }, info) => {
  await workspaceReady(app);
  await app.getByRole("button", { name: "Edit the Bass synth", exact: true }).click();
  const synth = app.getByRole("dialog", { name: "Bass synth", exact: true });
  await expect(synth).toBeVisible();
  const title = synth.locator(".device-title");
  const titleBounds = await title.boundingBox();
  if (!titleBounds) throw new Error("The real synth title is required for window dragging");
  await app.mouse.move(titleBounds.x + titleBounds.width / 2, titleBounds.y + titleBounds.height / 2);
  await app.mouse.down();
  try {
    await app.mouse.move(876, titleBounds.y + titleBounds.height / 2, { steps: 8 });
  } finally {
    await app.mouse.up();
  }
  // Manual dragging may park a window offscreen while its title stays reachable.
  const parkedTitle = await title.boundingBox();
  if (!parkedTitle) throw new Error("The parked synth must remain reachable");
  expect(parkedTitle.x).toBeGreaterThanOrEqual(0);
  expect(parkedTitle.x + parkedTitle.width).toBeLessThanOrEqual(900);

  await app.getByRole("button", { name: "Edit the Kick sampler", exact: true }).click();
  const sampler = app.getByRole("dialog", { name: "Kick sampler", exact: true });
  await expect(sampler).toBeVisible();
  await expect(app.locator(".device-window")).toHaveCount(2);
  const bounds = await sampler.boundingBox();
  if (!bounds) throw new Error("The newly opened sampler must have actual visible bounds");
  await info.attach("new-device-before-bounds", { body: await app.screenshot({ animations: "disabled", scale: "css" }), contentType: "image/png" });
  expect(bounds.width).toBeGreaterThan(0);
  expect(bounds.height).toBeGreaterThan(0);
  expect(bounds.x).toBeGreaterThanOrEqual(0);
  expect(bounds.y).toBeGreaterThanOrEqual(0);
  expect(bounds.x + bounds.width).toBeLessThanOrEqual(900);
  expect(bounds.y + bounds.height).toBeLessThanOrEqual(560);
});
