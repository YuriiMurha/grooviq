import type { GridLayout } from "./gridModel";

/** Red, green, blue and alpha, each 0..1. */
export type Rgba = [number, number, number, number];

/** The step grid's colours, read from the theme's CSS custom properties (one token source). */
export interface Palette {
  /** The six riso inks (`--ink-1` .. `--ink-6`): a lit step's fill. */
  inks: Rgba[];
  /** Their second inks (`--ink-1b` .. `--ink-6b`): the offset shadow under a lit step. */
  inksB: Rgba[];
  /** An empty step in beats 1 and 3, and in beats 2 and 4. */
  stepA: Rgba;
  stepB: Rgba;
  /** The part of a lit step above its velocity fill. */
  stepLit: Rgba;
  /** The keyboard focus ring. */
  focus: Rgba;
  /** The playhead column's tint. */
  playhead: Rgba;
  /** The misregistration of the second ink, in CSS pixels (`--reg-x`, `--reg-y`). */
  offset: [number, number];
  /** Tokens that were missing or unreadable (a fallback stands in for each). */
  missing: string[];
}

/** Draws the grid's cell records. One canvas has one renderer; `dispose` frees what it holds. */
export interface GridRenderer {
  draw(instances: Float32Array, layout: GridLayout, palette: Palette): void;
  dispose(): void;
}

const INKS = [1, 2, 3, 4, 5, 6].map((n) => `--ink-${n}`);
const INKS_B = INKS.map((t) => `${t}b`);
const COLOURS = ["--step-a", "--step-b", "--step-lit", "--grid-focus", "--grid-playhead"] as const;
const OFFSETS = ["--reg-x", "--reg-y"] as const;

/** Every custom property the palette reads, in the order a missing one is reported. */
export const PALETTE_TOKENS: readonly string[] = [...INKS, ...INKS_B, ...COLOURS, ...OFFSETS];

/** What stands in for a colour token that is missing: a plain mid grey, visibly not a theme ink. */
export const FALLBACK: Rgba = [0.5, 0.5, 0.5, 1];
/** The misregistration when `--reg-x` or `--reg-y` is missing. */
const FALLBACK_OFFSET = 2;

const HEX = /^#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i;
const RGB = /^rgba?\((.*)\)$/i;

/** A channel (0..255, or a percentage) as 0..1. */
function channel(text: string): number | null {
  const t = text.trim();
  if (t === "") return null;
  const n = t.endsWith("%") ? (Number(t.slice(0, -1)) / 100) * 255 : Number(t);
  return Number.isFinite(n) ? Math.min(255, Math.max(0, n)) / 255 : null;
}

/** An alpha (0..1, or a percentage). */
function alpha(text: string): number | null {
  const t = text.trim();
  if (t === "") return null;
  const n = t.endsWith("%") ? Number(t.slice(0, -1)) / 100 : Number(t);
  return Number.isFinite(n) ? Math.min(1, Math.max(0, n)) : null;
}

/** `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb(...)` or `rgba(...)` (comma or space syntax) as
 * RGBA floats; `null` for anything else. */
export function parseColor(text: string): Rgba | null {
  const t = text.trim();
  const hex = HEX.exec(t);
  if (hex) {
    let h = hex[1];
    if (h.length <= 4) h = [...h].map((c) => c + c).join("");
    const byte = (i: number) => parseInt(h.slice(i * 2, i * 2 + 2), 16) / 255;
    return [byte(0), byte(1), byte(2), h.length === 8 ? byte(3) : 1];
  }
  const rgb = RGB.exec(t);
  if (!rgb) return null;
  const [colour, slash] = rgb[1].split("/");
  const parts = colour.includes(",") ? colour.split(",") : colour.trim().split(/\s+/);
  let a = 1;
  if (slash !== undefined) {
    const s = alpha(slash);
    if (s === null) return null;
    a = s;
  } else if (parts.length === 4) {
    const s = alpha(parts.pop()!);
    if (s === null) return null;
    a = s;
  }
  if (parts.length !== 3) return null;
  const [r, g, b] = parts.map(channel);
  if (r === null || g === null || b === null) return null;
  return [r, g, b, a];
}

/** A length in pixels ("2px" or "2"). */
function parsePx(text: string): number | null {
  const m = /^(-?\d+(?:\.\d+)?)(px)?$/.exec(text.trim());
  return m ? Number(m[1]) : null;
}

/** Reads the grid's colours from the custom properties in effect on `root` (the same tokens the
 * CSS uses), falling back for any that is missing and naming it in `missing`. */
export function readPalette(root: Element): Palette {
  const style = getComputedStyle(root);
  const missing: string[] = [];
  const colour = (token: string): Rgba => {
    const c = parseColor(style.getPropertyValue(token));
    if (c) return c;
    missing.push(token);
    return [...FALLBACK];
  };
  const inks = INKS.map(colour);
  const inksB = INKS_B.map(colour);
  const [stepA, stepB, stepLit, focus, playhead] = COLOURS.map(colour);
  const [regX, regY] = OFFSETS.map((token) => {
    const px = parsePx(style.getPropertyValue(token));
    if (px !== null) return px;
    missing.push(token);
    return FALLBACK_OFFSET;
  });
  return { inks, inksB, stepA, stepB, stepLit, focus, playhead, offset: [regX, regY], missing };
}
