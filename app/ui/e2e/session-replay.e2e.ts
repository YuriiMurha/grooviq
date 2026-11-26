import { readFileSync } from "node:fs";
import { expect, test } from "@playwright/test";
import type { ArrangementReply, AutomationContent } from "../src/api/arrangement";
import { installMockTauri, type MockSeed } from "./mockTauri";

const params = JSON.parse(readFileSync(new URL("../../../tests/fixtures/device-params.json", import.meta.url), "utf8")) as {
  sampler: MockSeed["samplerParams"];
  synth: MockSeed["synthParams"];
};
const presets = JSON.parse(readFileSync(new URL("../../../tests/fixtures/synth-presets.json", import.meta.url), "utf8")) as MockSeed["synthPresets"];

const replay = JSON.parse(readFileSync(new URL("../../../tests/fixtures/session-replay.json", import.meta.url), "utf8")) as {
  steps: { command: string; args: Record<string, unknown> }[];
  expected: unknown[];
};

test("mock replays the real offline Session transcript", async ({ page }) => {
  // The command engine needs a browser, but no React tree, canvas, build or HTTP server.
  await page.evaluate(installMockTauri, { samplerParams: params.sampler, synthParams: params.synth, synthPresets: presets });
  const actual = await page.evaluate(async (steps) => {
    const invoke = (window as unknown as {
      __TAURI_INTERNALS__: { invoke: (command: string, args?: Record<string, unknown>) => Promise<unknown> };
    }).__TAURI_INTERNALS__.invoke;
    const results = [];
    for (const step of steps) {
      const reply = await invoke(step.command, step.args).then(
        (ok) => ({ ok }),
        (error: unknown) => ({ error: String(error) }),
      );
      const bytes = await invoke("pattern_notes") as ArrayBuffer;
      results.push({ reply, view: await invoke("project_view"), notes: Array.from(new Uint8Array(bytes)) });
    }
    return results;
  }, replay.steps);
  expect(actual).toEqual(replay.expected);
});

const mediaReplay = JSON.parse(readFileSync(new URL("../../../tests/fixtures/arrangement-media-replay.json", import.meta.url), "utf8")) as {
  files: Record<string, number[] | null>;
  steps: typeof replay.steps;
  expected: unknown[];
};

test("mock replays offline arrangement media, WFP1 and automation replies", async ({ page }) => {
  const files: MockSeed["files"] = Object.fromEntries(
    Object.entries(mediaReplay.files).map(([path, wav]) => [path, { kind: "sample", wav }]),
  );
  await page.evaluate(installMockTauri, { samplerParams: params.sampler, synthParams: params.synth, synthPresets: presets, files });
  const actual = await page.evaluate(async (steps) => {
    const { invoke } = (window as unknown as {
      __TAURI_INTERNALS__: { invoke: (command: string, args: Record<string, unknown>) => Promise<unknown> };
    }).__TAURI_INTERNALS__;
    const results = [];
    for (const { command, args } of steps) {
      try {
        let ok = await invoke(command, args);
        if (ok instanceof ArrayBuffer) ok = Array.from(new Uint8Array(ok));
        if (command === "project_save_as" || command === "project_new" || command === "channel_add_synth") ok = null;
        if (command === "project_open") ok = (ok as { warnings: string[] }).warnings;
        results.push({ ok });
      } catch (error) {
        results.push({ error: String(error) });
      }
    }
    return results;
  }, mediaReplay.steps);
  expect(actual).toHaveLength(mediaReplay.expected.length);
  for (const [index, step] of mediaReplay.steps.entries()) {
    expect(actual[index], `Replay step ${index + 1}: ${step.command} ${JSON.stringify(step.args)}`).toEqual(mediaReplay.expected[index]);
  }
});

test("a seeded automation clip can resize with its decoded curve", async ({ page }) => {
  const content: AutomationContent = {
    type: "automation", target: { type: "channelPan", channel: 1 },
    priority: 4, offsetTick: 0, lengthTicks: 960,
    points: [{ tick: 0, value: 0.1, mode: "hold" }],
  };
  await page.evaluate(installMockTauri, {
    samplerParams: params.sampler, synthParams: params.synth, synthPresets: presets,
    project: {
      name: "Automation", bpm: 140, timeSignature: { beats: 4, unit: 4 }, nextId: 5,
      pattern: { id: 0, name: "Pattern 1", steps: 16, swing: 0, notes: [] },
      channels: [{ id: 1, name: "Sampler", color: 0, sample: null, volume: 1, pan: 0, mute: false, solo: false, sampler: {} }],
      arrangement: {
        tracks: [{ id: 2, name: "Pan", color: 0, mute: false }],
        clips: [{ id: 3, track: 2, name: "Pan", startTick: 0, muted: false, content }],
        audioAssets: [], loopRegion: null,
      },
    },
  });
  const { curve, edit, oversized } = await page.evaluate(async (content) => {
    const { invoke } = (window as unknown as {
      __TAURI_INTERNALS__: { invoke: (command: string, args: Record<string, unknown>) => Promise<unknown> };
    }).__TAURI_INTERNALS__;
    const stamp = { generation: 0, revision: 0 };
    const curve = await invoke("arrangement_curve", { ...stamp, clip: 3 });
    const edit = await invoke("arrangement_edit", {
      ...stamp, editsJson: JSON.stringify([{
        kind: "resize", clip: 3, startTick: 240, content: { ...content, offsetTick: 240, lengthTicks: 720 },
      }]),
    });
    // Native refuses during decoding before the stamp check or any operation, unlike the mock.
    const oversized = await invoke("arrangement_edit", {
      ...stamp, revision: 1, editsJson: JSON.stringify([
        { kind: "createClip", track: 2, startTick: 960, name: "Oversized", muted: false,
          content: { ...content, points: Array(16_385).fill(content.points[0]) } },
        // The input limit still applies when the oversized curve would leave no persisted clip.
        { kind: "deleteClip", clip: 5 },
      ]),
    }) as ArrangementReply;
    return { curve, edit, oversized };
  }, content);
  expect(curve).toEqual({ generation: 0, revision: 0, clip: 3, points: [{ tick: 0, value: Math.fround(0.1), mode: "hold" }] });
  expect(edit).toMatchObject({ status: "applied", generation: 0, revision: 1, changed: true, createdIds: [] });
  expect(oversized).toMatchObject({ status: "refused", revision: 1, changed: false });
  expect(oversized.message).toMatch(/^arrangement list limit exceeded/);
});
