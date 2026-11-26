import { describe, expect, it, vi } from "vitest";
import { decodeMixerMeter, requestMixerMeter } from "./mixerMeter";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const request = { generation: 9, revision: 11, tracks: [5, 2, 999] };
const row = { track: 2, peak: [1.5, 0.125], clipFrame: "9007199254740993" };
const wire = {
  generation: 9,
  revision: 11,
  mixRevision: "18446744073709551615",
  frames: "9007199254741057",
  tracks: [row, { track: 5, peak: [0, 0], clipFrame: null }],
};

describe("mixer meter codec", () => {
  it("keeps exact u64 values and supports a subset when a requested track is absent", () => {
    expect(decodeMixerMeter(wire, request)).toEqual({
      generation: 9, revision: 11, mixRevision: 18446744073709551615n, frames: 9007199254741057n,
      tracks: [
        { track: 2, peak: [1.5, 0.125], clipFrame: 9007199254740993n },
        { track: 5, peak: [0, 0], clipFrame: null },
      ],
    });
    expect(decodeMixerMeter(null, request)).toBeNull();
  });

  it("refuses stale stamps, inexact counters and invalid rows", () => {
    for (const invalid of [
      { generation: 8 }, { revision: 10 }, { revision: 11.5 },
      { frames: "18446744073709551616" }, { frames: "01" }, { frames: "42\n" },
      { frames: Number(wire.frames) }, { frames: "0" }, { mixRevision: "-1" },
      { tracks: [] }, { tracks: [row, row] }, { tracks: [...wire.tracks].reverse() },
      { tracks: [{ ...row, track: 4 }] }, { tracks: [{ ...row, track: 0 }] },
      { tracks: [{ ...row, clipFrame: "9007199254741058" }] },
      { tracks: [{ ...row, clipFrame: "0" }] }, { tracks: [{ ...row, clipFrame: undefined }] },
      { tracks: [{ ...row, peak: [1, NaN] }] }, { tracks: [{ ...row, peak: [-1, 0] }] },
      { tracks: [{ ...row, peak: [1, 0, 0] }] },
    ]) {
      expect(decodeMixerMeter({ ...wire, ...invalid }, request)).toBeNull();
    }
  });

  it("sends only the flat stamp and tracks, including an empty lease cancellation", async () => {
    invoke.mockResolvedValueOnce(wire).mockResolvedValueOnce(null);
    const withExtraFields = { ...request, fullMixer: { huge: true } };
    expect((await requestMixerMeter(withExtraFields))?.frames).toBe(9007199254741057n);
    expect(invoke).toHaveBeenNthCalledWith(1, "mixer_meter", request);
    await expect(requestMixerMeter({ ...request, tracks: [] })).resolves.toBeNull();
    expect(invoke).toHaveBeenNthCalledWith(2, "mixer_meter", { ...request, tracks: [] });

    for (const invalid of [
      { ...request, revision: Number.MAX_SAFE_INTEGER + 1 },
      { ...request, tracks: [0] }, { ...request, tracks: [0x1_0000_0000] },
      { ...request, tracks: Array(65).fill(2) },
    ]) await expect(requestMixerMeter(invalid)).rejects.toThrow();
    expect(invoke).toHaveBeenCalledTimes(2);
  });
});
