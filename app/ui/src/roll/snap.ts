import type { TimeSignature } from "../api/project";

/** Ticks in a whole note (960 a quarter). */
const TICKS_PER_WHOLE = 3840;

/** A snap grid: a fixed number of ticks, or one bar of the time signature. */
export interface Grid {
  name: string;
  ticks: number | "bar";
}

/** The snap grids, finest first (Global Constraints). */
export const GRIDS: readonly Grid[] = [
  { name: "Line", ticks: 1 },
  { name: "1/6 step", ticks: 40 },
  { name: "1/4 step", ticks: 60 },
  { name: "1/3 step", ticks: 80 },
  { name: "1/2 step", ticks: 120 },
  { name: "Step", ticks: 240 },
  { name: "1/3 beat", ticks: 320 },
  { name: "1/2 beat", ticks: 480 },
  { name: "Beat", ticks: 960 },
  { name: "Bar", ticks: "bar" },
];

export const DEFAULT_GRID = GRIDS[5];

/** One bar of the signature, in ticks. */
export const barTicks = (sig: TimeSignature) => Math.max(1, Math.round((sig.beats * TICKS_PER_WHOLE) / sig.unit));

export const gridTicks = (grid: Grid, sig: TimeSignature) => (grid.ticks === "bar" ? barTicks(sig) : grid.ticks);

/** `tick` on a `grid`-tick grid: the line at or before it, or the nearest line (half way rounds up). */
export function snapTick(tick: number, grid: number, mode: "floor" | "round"): number {
  const g = Math.max(1, grid);
  return (mode === "floor" ? Math.floor(tick / g) : Math.round(tick / g)) * g;
}
