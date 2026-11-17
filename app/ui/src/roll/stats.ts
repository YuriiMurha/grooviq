import type { RollView } from "./view";

/** The piano roll's counters, for the Playwright pass and the bench. */
export interface RollStats {
  /** Instance buffers built (each upload of the notes to the renderer). */
  bufferBuilds: number;
  /** Frames drawn. */
  draws: number;
  /** "webgl2" or "canvas2d". */
  renderer: string;
  view: RollView | null;
  /** The scale the grid shows, as twelve bits from C (0 without a key). */
  scaleMask: number;
}

export const rollStats: RollStats = { bufferBuilds: 0, draws: 0, renderer: "", view: null, scaleMask: 0 };

// Outside production builds (`vite build --mode e2e`, the dev server, the bench) the counters
// are on `window.__grooviqRoll`.
if (import.meta.env.MODE !== "production" && typeof window !== "undefined") {
  (window as unknown as { __grooviqRoll: RollStats }).__grooviqRoll = rollStats;
}
