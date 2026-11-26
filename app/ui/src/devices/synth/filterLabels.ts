const EDGE_INSET = 4;
// Manrope's 11 px numerals and capitals occupy about 8 px above the baseline.
const BASELINE_BELOW_ZERO = 11;
const BOTTOM_INSET = 3;
const FREQUENCY_BAND_HEIGHT = 16;
const TICK_BASELINE_INSET = 4;
const DB_TOP = 24;
const DB_BOTTOM = -36;

export function filterAxis(height: number) {
  const bandTop = Math.max(0, height - FREQUENCY_BAND_HEIGHT);
  const top = Math.min(10, bandTop);
  // Keep the scale's floor at the axis boundary even when the stopband falls farther.
  const yd = (db: number) =>
    top + ((DB_TOP - Math.min(DB_TOP + 3, Math.max(DB_BOTTOM, db))) / (DB_TOP - DB_BOTTOM)) * (bandTop - top);
  return { top, bandTop, tickBaseline: Math.max(0, height - TICK_BASELINE_INSET), yd };
}

export function zeroDbLabel(width: number, plotBottom: number, cutoffX: number, zeroY: number) {
  // The response can still cross the label after the handle itself has cleared it.
  const atRight = cutoffX < width / 2;
  return {
    x: atRight ? width - EDGE_INSET : EDGE_INSET,
    y: Math.min(zeroY + BASELINE_BELOW_ZERO, Math.max(0, plotBottom - BOTTOM_INSET)),
    textAnchor: atRight ? "end" as const : "start" as const,
  };
}
