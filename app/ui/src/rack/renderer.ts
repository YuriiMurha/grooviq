import { createCanvasRenderer } from "./canvasRenderer";
import { createGlRenderer } from "./glRenderer";
import type { GridRenderer } from "./palette";

/** WebGL2 when the machine has it, else Canvas 2D. */
export function defaultRenderer(canvas: HTMLCanvasElement): GridRenderer {
  return createGlRenderer(canvas) ?? createCanvasRenderer(canvas);
}
