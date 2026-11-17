import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { SYNTH_PARAMS, SYNTH_SECTIONS } from "./synthLayout";

/** The engine's parameter table, which a Rust test keeps in step with `grooviq_dsp::synth`. */
const FIXTURE = (
  JSON.parse(readFileSync(new URL("../../../../../tests/fixtures/device-params.json", import.meta.url), "utf8")) as {
    synth: { id: string; min: number; max: number; default: number; stepped: boolean }[];
  }
).synth;

describe("synth layout", () => {
  it("the_layout_matches_the_device_params_fixture", () => {
    // Every parameter, in the engine's order, with its range, default and whether the engine
    // rounds it (the fixture's `stepped` comes from `grooviq_dsp::synth::is_stepped`).
    expect(SYNTH_PARAMS.map(({ id, min, max, default: d, stepped }) => ({ id, min, max, default: d, stepped }))).toEqual(FIXTURE);
    expect(FIXTURE.filter((p) => p.stepped).length).toBeGreaterThan(0);

    // Each one is placed on the editor exactly once.
    const placed = SYNTH_SECTIONS.flatMap((s) => s.controls.map((c) => c.id));
    expect([...placed].sort()).toEqual(FIXTURE.map((p) => p.id).sort());

    for (const p of SYNTH_PARAMS) {
      // An enum names every value of its range; only stepped parameters are enums.
      if (p.options) {
        expect(p.stepped, p.id).toBe(true);
        expect(p.options.length, p.id).toBe(p.max - p.min + 1);
      }
      // A stepped range has whole ends; a log curve needs a positive range.
      if (p.stepped) expect(Number.isInteger(p.min) && Number.isInteger(p.max), p.id).toBe(true);
      if (p.curve === "log") expect(p.min, p.id).toBeGreaterThan(0);
    }
  });

  it("enum_parameters_use_segmented_buttons", () => {
    const kinds = Object.fromEntries(SYNTH_SECTIONS.flatMap((s) => s.controls.map((c) => [c.id, c.kind])));
    for (const p of SYNTH_PARAMS) expect(kinds[p.id] === "segmented", p.id).toBe(p.options !== undefined);
  });
});
