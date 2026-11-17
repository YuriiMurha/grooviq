import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { SAMPLER_PARAMS, SAMPLER_SECTIONS } from "./samplerLayout";

/** The engine's parameter table, which a Rust test keeps in step with `grooviq_dsp::sampler`. */
const FIXTURE = (
  JSON.parse(readFileSync(new URL("../../../../../tests/fixtures/device-params.json", import.meta.url), "utf8")) as {
    sampler: { id: string; min: number; max: number; default: number; stepped: boolean }[];
  }
).sampler;

describe("sampler layout", () => {
  it("the_layout_matches_the_device_params_fixture", () => {
    // Every parameter, in the engine's order, with its range, default and whether the engine
    // rounds it (`grooviq_dsp::sampler::is_stepped`).
    expect(SAMPLER_PARAMS.map(({ id, min, max, default: d, stepped }) => ({ id, min, max, default: d, stepped }))).toEqual(FIXTURE);
    expect(FIXTURE.length).toBe(29);

    // Each one is placed on the editor exactly once.
    const placed = SAMPLER_SECTIONS.flatMap((s) => s.controls.map((c) => c.id));
    expect([...placed].sort()).toEqual(FIXTURE.map((p) => p.id).sort());

    const kinds = Object.fromEntries(SAMPLER_SECTIONS.flatMap((s) => s.controls.map((c) => [c.id, c.kind])));
    for (const p of SAMPLER_PARAMS) {
      // An enum names every value of its range and is a row of buttons; only stepped
      // parameters are enums.
      expect(kinds[p.id] === "segmented", p.id).toBe(p.options !== undefined);
      if (p.options) {
        expect(p.stepped, p.id).toBe(true);
        expect(p.options.length, p.id).toBe(p.max - p.min + 1);
      }
      // A switch (on at >= 0.5 in the engine) is a 0..1 range that is not rounded.
      if (kinds[p.id] === "toggle") expect([p.min, p.max, p.stepped], p.id).toEqual([0, 1, false]);
      if (p.stepped) expect(Number.isInteger(p.min) && Number.isInteger(p.max), p.id).toBe(true);
      if (p.curve === "log") expect(p.min, p.id).toBeGreaterThan(0);
    }
    // The region's three markers are the waveform's handles.
    expect(Object.keys(kinds).filter((id) => kinds[id] === "marker").sort()).toEqual(["end", "loop_start", "start"]);
  });
});
