import { readFileSync } from "node:fs";
import { expect, test } from "@playwright/test";
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
