import type { ExportResult } from "../api/project";

/** -0.3, +2.1, 0.0: one decimal, with an explicit plus above full scale. */
export function formatPeak(db: number): string {
  const r = Math.round(db * 10) / 10;
  if (r === 0) return "0.0";
  return `${r > 0 ? "+" : ""}${r.toFixed(1)}`;
}

/** What the export dialog says when the file is written. Clipping is a warning. */
export function exportMessage(r: ExportResult): { text: string; warn: boolean } {
  if (r.clipped > 0) {
    const n = r.clipped.toLocaleString("en-US");
    const samples = r.clipped === 1 ? "1 sample" : `${n} samples`;
    return {
      text: `Exported, but ${samples} clipped (peak ${formatPeak(r.peakDb)} dBFS). Lower the channel volumes and export again.`,
      warn: true,
    };
  }
  // The engine floors silence at -120.
  if (r.peakDb <= -120) return { text: "Exported. The file is silent.", warn: false };
  return { text: `Exported. Peak ${formatPeak(r.peakDb)} dBFS.`, warn: false };
}
