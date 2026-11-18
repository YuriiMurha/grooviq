import { describe, expect, it } from "vitest";
import { decodeSynthMeter } from "./synthMeter";
describe("synth observation identity", () => {
  it("rejects another incarnation and malformed/nonfinite frames", () => {
    const raw = [7, 1024, ...Array(262).fill(0.25)];
    expect(decodeSynthMeter(raw, 7)?.scope).toEqual(Array(256).fill(0.25));
    expect(decodeSynthMeter(raw, 8)).toBeNull();
    expect(decodeSynthMeter(raw.slice(1), 7)).toBeNull();
    raw[25] = NaN;
    expect(decodeSynthMeter(raw, 7)).toBeNull();
  });
  it("clears a pending first observation", () => {
    expect(decodeSynthMeter([9, 1024], null)?.scope).toEqual(Array(256).fill(0));
  });
});
