/** A volume (linear gain) in decibels. */
export function formatVolume(volume: number): string {
  if (volume <= 0) return "Silent";
  const db = 20 * Math.log10(volume);
  const text = Math.abs(db) < 0.05 ? "0.0" : db.toFixed(1);
  return `${db >= 0.05 ? "+" : ""}${text} dB`;
}

/** A pan position in words. */
export function formatPan(pan: number): string {
  const pct = Math.round(Math.abs(pan) * 100);
  if (pct === 0) return "Centre";
  return `${pct}% ${pan < 0 ? "left" : "right"}`;
}
