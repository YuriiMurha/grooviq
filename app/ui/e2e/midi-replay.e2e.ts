import { readFileSync } from "node:fs";
import { expect, test } from "@playwright/test";
import { installMockTauri, type MockSeed } from "./mockTauri";
import { SYNTH_PARAMS, SYNTH_PRESETS } from "./fixture";
import { twoChannels } from "./rollHelpers";

type Step =
  | { op: "plug" | "unplug"; port: string }
  | { op: "enable"; port: string; enabled: boolean }
  | { op: "message"; port: string; bytes: [number, number, number] }
  | { op: "select" | "remove"; channel: number }
  | { op: "stall"; stalled: boolean }
  | { op: "retry" | "add" | "open" | "new" };

type Press = [id: number, channel: number, key: number];

const params = JSON.parse(readFileSync(new URL("../../../tests/fixtures/device-params.json", import.meta.url), "utf8")) as {
  sampler: MockSeed["samplerParams"];
};

const replay = JSON.parse(readFileSync(new URL("../../../tests/fixtures/midi-replay.json", import.meta.url), "utf8")) as {
  steps: Step[];
  expected: { presses: Press[]; delivered: [...Press, boolean][]; owed: number; inputs: [string, boolean][] }[];
};

test("mock replays the real offline MIDI hub transcript", async ({ page }) => {
  const replacement = twoChannels([]);
  await page.evaluate(installMockTauri, {
    project: replacement,
    samplerParams: params.sampler,
    synthParams: SYNTH_PARAMS,
    synthPresets: SYNTH_PRESETS,
    files: { "replacement.grooviq": { kind: "project", project: replacement } },
  } satisfies MockSeed);
  const actual = await page.evaluate(async (steps) => {
    const { __TAURI_INTERNALS__: { invoke }, __grooviqMock: mock } = window as unknown as {
      __TAURI_INTERNALS__: { invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T> };
      __grooviqMock: {
        state: { generation: number; presses: Map<number, string> };
        midiPlug: (port: string) => void;
        midiUnplug: (port: string) => void;
        midiMessage: (port: string, bytes: number[]) => void;
        midiStall: (stalled: boolean) => void;
        midiRetry: () => void;
        midiOwed: () => number;
      };
    };
    const pressesNow = (): Press[] => [...mock.state.presses].map(([id, location]) => {
      const [channel, key] = location.split(":").map(Number);
      return [id, channel, key];
    });
    const results = [];
    for (const step of steps) {
      const before = pressesNow();
      const generation = mock.state.generation;
      switch (step.op) {
        case "plug": mock.midiPlug(step.port); break;
        case "unplug": mock.midiUnplug(step.port); break;
        case "enable": await invoke("midi_set_enabled", { id: step.port, enabled: step.enabled }); break;
        case "message": mock.midiMessage(step.port, step.bytes); break;
        case "select": await invoke("channel_select", { channel: step.channel }); break;
        case "stall": mock.midiStall(step.stalled); break;
        case "retry": mock.midiRetry(); break;
        case "remove": await invoke("project_edit", { generation, edit: { kind: "removeChannel", channel: step.channel } }); break;
        case "add": await invoke("channel_add_synth", { generation, preset: "Init" }); break;
        case "open": await invoke("project_open", { path: "replacement.grooviq" }); break;
        case "new": await invoke("project_new"); break;
      }
      const presses = pressesNow();
      const delivered: [...Press, boolean][] = presses
        .filter(([id]) => !before.some(([previous]) => previous === id))
        .map((press) => [...press, true]);
      // Graph cuts remove presses without sending MIDI offs.
      if (step.op !== "remove" && step.op !== "open" && step.op !== "new") {
        for (const press of before) {
          if (!presses.some(([id]) => id === press[0])) delivered.push([...press, false]);
        }
      }
      delivered.sort((a, b) => a[0] - b[0]);
      const inputs = await invoke<{ id: string; enabled: boolean }[]>("midi_inputs");
      results.push({ presses, delivered, owed: mock.midiOwed(), inputs: inputs.map(({ id, enabled }) => [id, enabled]) });
    }
    return results;
  }, replay.steps);

  expect(replay.expected).toHaveLength(replay.steps.length);
  for (const [index, step] of replay.steps.entries()) {
    expect(actual[index], `step ${index}: ${JSON.stringify(step)}`).toEqual(replay.expected[index]);
  }
});
