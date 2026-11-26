const EDGE_INSET = 4;
// Manrope's 11 px numerals and capitals occupy about 8 px above the baseline.
const BASELINE_BELOW_ZERO = 11;
const BOTTOM_INSET = 3;

export function zeroDbLabel(width: number, height: number, cutoffX: number, zeroY: number) {
  // The response can still cross the label after the handle itself has cleared it.
  const atRight = cutoffX < width / 2;
  return {
    x: atRight ? width - EDGE_INSET : EDGE_INSET,
    y: Math.min(zeroY + BASELINE_BELOW_ZERO, Math.max(0, height - BOTTOM_INSET)),
    textAnchor: atRight ? "end" as const : "start" as const,
  };
}
