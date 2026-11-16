import { describe, expect, it } from "vitest";
import { exportMessage, formatPeak } from "./exportText";

const base = { frames: 1, seconds: 1 };

describe("exportMessage", () => {
  it("reports the peak of a clean export", () => {
    expect(exportMessage({ ...base, peakDb: -0.3, clipped: 0 })).toEqual({
      text: "Exported. Peak -0.3 dBFS.",
      warn: false,
    });
  });

  it("warns about clipping with a thousands separator", () => {
    expect(exportMessage({ ...base, peakDb: 2.1, clipped: 1204 })).toEqual({
      text: "Exported, but 1,204 samples clipped (peak +2.1 dBFS). Lower the channel volumes and export again.",
      warn: true,
    });
  });

  it("says a file of silence is silent", () => {
    expect(exportMessage({ ...base, peakDb: -120, clipped: 0 }).text).toBe(
      "Exported. The file is silent.",
    );
  });

  it("does not print a negative zero", () => {
    expect(formatPeak(-0.04)).toBe("0.0");
    expect(formatPeak(0.04)).toBe("0.0");
    expect(formatPeak(-12.34)).toBe("-12.3");
  });
});
