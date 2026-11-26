// Where a device window sits and how large it is drawn. A device is laid out at its design size
// and scaled down (never up) to fit the app window.

/** The synth and sampler editors' design size, CSS px, title bar included. */
export const DESIGN_W = 880;
export const DESIGN_H = 520;
/** Gutters that leave room for the full editor above the hint bar at 900 × 560. */
const MARGIN_X = 10;
const MARGIN_Y = 4;
/** How much of a dragged window must stay on screen to grab it again. */
const KEEP = 80;
/** Each window opened after the first starts this much further down and right. */
const CASCADE = 24;

export interface Placement {
  x: number;
  y: number;
}

/** The scale (0..1] that fits the design size inside the view. */
export function fitScale(viewW: number, viewH: number, reserveBottom: number): number {
  const usableH = viewH - reserveBottom;
  const s = Math.min(1, (viewW - 2 * MARGIN_X) / DESIGN_W, (usableH - 2 * MARGIN_Y) / DESIGN_H);
  return Math.max(0.25, s);
}

/** Where the `index`th open window first appears: centred, then cascading. */
export function initialPlacement(viewW: number, viewH: number, scale: number, index: number, reserveBottom: number): Placement {
  const x = (viewW - DESIGN_W * scale) / 2 + index * CASCADE;
  const usableH = viewH - reserveBottom;
  const y = (usableH - DESIGN_H * scale) / 2 + index * CASCADE;
  // Opening contains the full editor while preserving rounded cascade positions.
  return containPlacement({ x: Math.round(x), y: Math.round(y) }, viewW, viewH, scale, reserveBottom);
}

/** Keeps enough of the window, and all of its title bar's height, on screen. */
export function clampPlacement(p: Placement, viewW: number, viewH: number, scale: number): Placement {
  const w = DESIGN_W * scale;
  const x = Math.min(viewW - KEEP, Math.max(KEEP - w, p.x));
  const y = Math.min(viewH - KEEP * scale, Math.max(0, p.y));
  return { x: Math.round(x), y: Math.round(y) };
}

/** Resize keeps every control visible. Below the minimum scale's physical size, anchor
 * at the top left so the title bar stays reachable even when full containment is impossible. */
export function containPlacement(p: Placement, viewW: number, viewH: number, scale: number, reserveBottom: number): Placement {
  const usableH = viewH - reserveBottom;
  return {
    x: Math.max(0, Math.min(p.x, Math.floor(viewW - DESIGN_W * scale))),
    y: Math.max(0, Math.min(p.y, Math.floor(usableH - DESIGN_H * scale))),
  };
}
