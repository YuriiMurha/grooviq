import { beforeEach, describe, expect, it, vi } from "vitest";
import { decodeSynthMeter } from "./synthMeter";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

describe("synth observation identity", () => {
  it("decodes motion without extending scope and accepts legacy/pending observations", () => {
    const legacy = [7, 1024, ...Array(262).fill(0.25)];
    expect(decodeSynthMeter(legacy, 7)?.motion).toBeNull();
    expect(decodeSynthMeter([7, 2048], 7)?.motion).toBeNull();
    for (const [code, stage] of ["idle", "attack", "decay", "sustain", "release"].entries()) {
      const result = decodeSynthMeter([...legacy, code, 4 - code, 1], 7);
      expect(result?.motion).toEqual({ ampStage: stage, filterStage: ["release", "sustain", "decay", "attack", "idle"][code], lfoPhase: 1 });
      expect(result?.scope).toEqual(Array(256).fill(0.25));
    }
    for (const motion of [[-1, 0, 0], [5, 0, 0], [1.5, 0, 0], [0, 5, 0], [0, 1.2, 0], [0, 0, -0.1], [0, 0, 1.01], [0, 0, NaN]]) {
      expect(decodeSynthMeter([...legacy, ...motion], 7)).toBeNull();
    }
    for (const frame of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1, Infinity]) {
      expect(decodeSynthMeter([7, frame], 7)).toBeNull();
    }
  });
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

describe("synth observation request ownership", () => {
  beforeEach(() => { vi.resetAllMocks(); vi.resetModules(); });
  it("retains one native request until it settles, then allows the current selection", async () => {
    const { readSynthMeter } = await import("./synthMeter");
    let release!: (value: unknown) => void;
    invoke.mockReturnValueOnce(new Promise(resolve => { release = resolve; }));
    invoke.mockImplementation(() => Promise.resolve([4, 2048]));
    const first = readSynthMeter(3, 0, null);
    expect(first).not.toBeNull();
    for (let i = 0; i < 10; i++) expect(readSynthMeter(4, 1, null)).toBeNull();
    expect(invoke).toHaveBeenCalledTimes(1);
    release([3, 1024]);
    await expect(first).resolves.toEqual([3, 1024]);
    invoke.mockResolvedValueOnce([4, 2048]);
    const next = readSynthMeter(4, 1, null);
    expect(next).not.toBeNull();
    await expect(next).resolves.toEqual([4, 2048]);
  });
  it("releases ownership after rejection so a later selection can recover", async () => {
    const { readSynthMeter } = await import("./synthMeter");
    invoke.mockRejectedValueOnce("unavailable");
    const first = readSynthMeter(3, 0, null);
    expect(first).not.toBeNull();
    await expect(first).rejects.toBe("unavailable");
    invoke.mockResolvedValueOnce([5, 3072]);
    const next = readSynthMeter(5, 2, null);
    expect(next).not.toBeNull();
    await expect(next).resolves.toEqual([5, 3072]);
  });
});
