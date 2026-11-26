import type { MixerSnapshot } from "../src/api/mixer";
import { expect, project, soundsFor, test } from "./fixture";

test.use({ seed: { project: project(["Kick"]), files: soundsFor(["Kick"]) } });

test("Workspace mixer disclosure and guarded F9 preserve focus and compact roll space", async ({ app, mock }) => {
  const snapshot: MixerSnapshot = {
    generation: (await mock.view()).generation, revision: 0, routeInputs: [{ track: 0, from: [] }],
    mixer: {
      mode: "legacyDirect",
      tracks: [{ id: 0, name: "Master", output: null, level: { db: 0 }, pan: 0, mute: false, solo: false, inserts: [] }],
      sends: [], sidechains: [], sources: [], nextTrackId: 1, nextInsertId: 1, nextSendId: 1,
    },
    sources: [{ source: { channel: 1 }, name: "Kick", instrument: "sampler" }],
    effects: [],
    unsupported: { mute: "Mute unavailable.", solo: "Solo unavailable.", bypass: "Bypass unavailable.", detector: "Detector unavailable." },
  };
  const toggle = app.getByRole("button", { name: "Mixer", exact: true });
  const mixer = app.getByRole("region", { name: "Mixer", exact: true });
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(mixer).toHaveCount(0);
  expect(await mock.calls("mixer_view")).toHaveLength(0);

  // Extend this generated App test's IPC stand-in only for the mixer read.
  await app.evaluate((state) => {
    const windowWithFixture = window as unknown as {
      __TAURI_INTERNALS__: { invoke: (command: string, args?: Record<string, unknown>, options?: unknown) => Promise<unknown> };
      __grooviqMock: { calls: { cmd: string; args: Record<string, unknown>; at: number }[] };
    };
    const shell = windowWithFixture.__TAURI_INTERNALS__;
    const invoke = shell.invoke.bind(shell);
    shell.invoke = (command, args, options) => {
      if (command !== "mixer_view") return invoke(command, args, options);
      windowWithFixture.__grooviqMock.calls.push({ cmd: command, args: args ?? {}, at: performance.now() });
      return Promise.resolve(structuredClone(state));
    };
  }, snapshot);

  const bpm = app.getByRole("textbox", { name: "BPM" });
  await bpm.focus();
  await app.keyboard.press("F9");
  await expect(bpm).toBeFocused();
  await expect(toggle).toHaveAttribute("aria-expanded", "false");

  const settings = app.locator("button.chip[aria-haspopup='dialog']");
  await settings.click();
  const dialog = app.getByRole("dialog", { name: "Settings" });
  await expect(dialog).toBeVisible();
  await dialog.focus();
  await app.keyboard.press("F9");
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await app.keyboard.press("Escape");
  await expect(dialog).toBeHidden();

  await toggle.focus();
  await app.keyboard.press("Shift+F9");
  await toggle.dispatchEvent("keydown", { key: "F9", repeat: true, bubbles: true, cancelable: true });
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  expect(await mock.calls("mixer_view")).toHaveLength(0);

  await app.keyboard.press("F7");
  const splitter = app.getByRole("separator", { name: "Resize the channel rack and the piano roll" });
  await expect(splitter).toHaveAttribute("aria-valuetext", /Channel rack/);
  const startingSplit = Number(await splitter.getAttribute("aria-valuenow"));
  await splitter.press("ArrowUp");
  await expect.poll(async () => Number(await splitter.getAttribute("aria-valuenow"))).toBeLessThan(startingSplit);
  const requestedSplit = await splitter.getAttribute("aria-valuenow");

  await app.setViewportSize({ width: 900, height: 560 });
  await splitter.focus();
  await app.keyboard.press("F9");
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(mixer).toBeFocused();
  await expect(mixer.getByRole("button", { name: "Add track" })).toBeVisible();
  const notes = app.getByRole("region", { name: "Piano roll", exact: true }).locator("canvas[aria-label='Piano roll notes']");
  await expect.poll(() => notes.evaluate((el) => el.getBoundingClientRect().height)).toBeGreaterThanOrEqual(112);
  await expect(notes).toBeInViewport();

  await app.keyboard.press("F9");
  await expect(mixer).toHaveCount(0);
  await expect(splitter).toBeFocused();
  await app.setViewportSize({ width: 1280, height: 800 });
  await expect(splitter).toHaveAttribute("aria-valuenow", requestedSplit ?? "");

  await toggle.click();
  await expect(mixer).toBeFocused();
  await expect(mixer.getByRole("button", { name: "Add track" })).toBeVisible();
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(toggle).toBeFocused();
  await expect(mixer).toHaveCount(0);
  const readsAfterClose = (await mock.calls("mixer_view")).length;
  await app.getByRole("button", { name: "Mute Kick", exact: true }).click();
  await expect.poll(async () => (await mock.view()).dirty).toBe(true);
  expect(await mock.calls("mixer_view")).toHaveLength(readsAfterClose);
});
