import { readFileSync } from "node:fs";
import { expect, test } from "./fixture";

const replay = JSON.parse(readFileSync(new URL("../../../tests/fixtures/session-replay.json", import.meta.url), "utf8")) as {
  steps: { command: string; args: Record<string, unknown> }[];
  expected: unknown[];
};

test("mock replays the real offline Session transcript", async ({ app }) => {
  const actual = await app.evaluate(async (steps) => {
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
