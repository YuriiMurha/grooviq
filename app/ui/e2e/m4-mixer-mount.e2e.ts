import type { Page } from "@playwright/test";
import type { MixerSnapshot } from "../src/api/mixer";
import { expect, project, soundsFor, test } from "./fixture";

test.use({ seed: { project: project(["Kick"]), files: soundsFor(["Kick"]) } });

async function installMixerFixture(app: Page, snapshot: MixerSnapshot, meterTrack?: number) {
  // Extend this generated App test's IPC stand-in without changing the shared framework.
  await app.evaluate(({ state, meterTrack }) => {
    const windowWithFixture = window as unknown as {
      __TAURI_INTERNALS__: { invoke: (command: string, args?: Record<string, unknown>, options?: unknown) => Promise<unknown> };
      __grooviqMock: { calls: { cmd: string; args: Record<string, unknown>; at: number }[] };
    };
    const shell = windowWithFixture.__TAURI_INTERNALS__;
    const invoke = shell.invoke.bind(shell);
    let frames = 0;
    shell.invoke = (command, args, options) => {
      if (command !== "mixer_view" && (command !== "mixer_meter" || meterTrack === undefined)) return invoke(command, args, options);
      windowWithFixture.__grooviqMock.calls.push({ cmd: command, args: args ?? {}, at: performance.now() });
      if (command === "mixer_view") return Promise.resolve(structuredClone(state));
      const tracks = args?.tracks;
      if (args?.generation !== state.generation || args.revision !== state.revision
        || !Array.isArray(tracks) || tracks.length !== 1 || tracks[0] !== meterTrack) return Promise.resolve(null);
      frames += 64;
      return Promise.resolve({
        generation: state.generation, revision: state.revision, mixRevision: "1", frames: String(frames),
        tracks: [{ track: meterTrack, peak: [1.25, 0.25], clipFrame: "64" }],
      });
    };
  }, { state: snapshot, meterTrack });
}

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

  await installMixerFixture(app, snapshot);

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

test("the mounted mixer reads stereo, clears a clipped strip and releases its lease", async ({ app, mock }) => {
  const generation = (await mock.view()).generation;
  const master = { id: 0, name: "Master", output: null, level: { db: 0 }, pan: 0, mute: false, solo: false, inserts: [] };
  await installMixerFixture(app, {
    generation, revision: 1, routeInputs: [{ track: 0, from: [7] }, { track: 7, from: [] }],
    mixer: {
      mode: "routed", tracks: [master, { ...master, id: 7, name: "Drum bus", output: 0 }],
      sends: [], sidechains: [], sources: [], nextTrackId: 8, nextInsertId: 1, nextSendId: 1,
    },
    sources: [], effects: [],
    unsupported: { mute: "Mute unavailable.", solo: "Solo unavailable.", bypass: "Bypass unavailable.", detector: "Detector unavailable." },
  }, 7);

  const toggle = app.getByRole("button", { name: "Mixer", exact: true });
  expect(await mock.calls("mixer_meter")).toHaveLength(0);
  await toggle.click();
  const mixer = app.getByRole("region", { name: "Mixer", exact: true });
  const strip = mixer.getByRole("region", { name: "Drum bus", exact: true });
  const bars = strip.getByRole("img", { name: "Drum bus meter" }).locator(".mx-meter-bar");
  const clear = strip.getByRole("button", { name: "Clear Drum bus clip" });
  await expect(clear).toBeEnabled();
  await expect.poll(() => bars.evaluateAll((elements) => elements.map((el) =>
    Number((el as HTMLElement).style.transform.match(/^scaleY\((.+)\)$/)?.[1] ?? 0),
  ))).toEqual([1, expect.closeTo(1 + 20 * Math.log10(0.25) / 60, 4)]);
  await clear.click();
  const observed = (await mock.calls("mixer_meter")).length;
  await expect.poll(async () => (await mock.calls("mixer_meter")).length).toBeGreaterThan(observed);
  await expect(strip.getByRole("button", { name: "Drum bus has not clipped" })).toBeDisabled();

  await toggle.click();
  await expect(mixer).toHaveCount(0);
  await expect.poll(async () => (await mock.calls("mixer_meter")).at(-1)?.args)
    .toEqual({ generation, revision: 1, tracks: [] });
  const requests = await mock.calls("mixer_meter");
  expect(requests.slice(0, -1).every(({ args }) => args.generation === generation && args.revision === 1
    && Array.isArray(args.tracks) && args.tracks.length === 1 && args.tracks[0] === 7)).toBe(true);
});
