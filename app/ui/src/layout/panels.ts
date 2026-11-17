// The workspace's panels: the channel rack (F6) and the piano roll (F7), stacked with a
// splitter between them when both are open.

export type Panel = "rack" | "roll";

export interface Panels {
  rack: boolean;
  roll: boolean;
  /** The rack's share of the height when both are open. */
  split: number;
}

export const DEFAULT_PANELS: Panels = { rack: true, roll: false, split: 0.5 };
export const SPLIT_MIN = 0.15;
export const SPLIT_MAX = 0.85;

/** The panel a key toggles: F6 the rack, F7 the piano roll. */
export function panelFor(key: string): Panel | null {
  return key === "F6" ? "rack" : key === "F7" ? "roll" : null;
}

export function togglePanel(p: Panels, panel: Panel): Panels {
  return { ...p, [panel]: !p[panel] };
}

/** The channel the piano roll shows: the selected one, else the first. */
export function rollTarget(selected: number | null, channels: number[]): number | null {
  if (selected !== null && channels.includes(selected)) return selected;
  return channels[0] ?? null;
}

/** The split after dragging the splitter `dy` pixels in a workspace `height` pixels tall. */
export function splitAfterDrag(start: number, dy: number, height: number): number {
  if (!(height > 0)) return start;
  return Math.min(SPLIT_MAX, Math.max(SPLIT_MIN, start + dy / height));
}
