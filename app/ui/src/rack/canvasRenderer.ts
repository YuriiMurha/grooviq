import { RADIUS, RING_GAP, RING_WIDTH, PLAYHEAD_MIX } from "./glRenderer";
import {
  EDGE,
  FLAG_BAR,
  FLAG_BEAT_B,
  FLAG_FOCUS,
  FLAG_PLAYHEAD,
  STRIDE,
  fillHeight,
  type GridLayout,
} from "./gridModel";
import type { GridRenderer, Palette, Rgba } from "./palette";

// The same picture as the WebGL2 renderer, with the Canvas 2D API, for a machine without WebGL2.
// It walks the cell records twice: the offset second ink first, then the cells.

const css = ([r, g, b, a]: Rgba) =>
  `rgba(${Math.round(r * 255)}, ${Math.round(g * 255)}, ${Math.round(b * 255)}, ${a})`;
const mix = (a: Rgba, b: Rgba, t: number): Rgba => [
  a[0] + (b[0] - a[0]) * t,
  a[1] + (b[1] - a[1]) * t,
  a[2] + (b[2] - a[2]) * t,
  a[3],
];

function roundRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
  ctx.beginPath();
  if (typeof ctx.roundRect === "function") ctx.roundRect(x, y, w, h, r);
  else ctx.rect(x, y, w, h);
}

/** A Canvas 2D renderer. Without even a 2D context it draws nothing (and says so once). */
export function createCanvasRenderer(canvas: HTMLCanvasElement): GridRenderer {
  const ctx = canvas.getContext("2d");
  if (!ctx) {
    console.error("The step grid has no canvas to draw on.");
    return { draw() {}, dispose() {} };
  }
  return {
    draw(instances: Float32Array, layout: GridLayout, palette: Palette) {
      const dpr = layout.dpr;
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      ctx.setTransform(dpr, 0, 0, dpr, EDGE * dpr, EDGE * dpr);
      const [ox, oy] = palette.offset;

      // A coincident shadow would show through the antialiased corners of the fill.
      if (ox !== 0 || oy !== 0) {
        for (let o = 0; o < instances.length; o += STRIDE) {
          const velocity = instances[o + 5];
          if (velocity < 0 || instances[o + 6] & FLAG_BAR) continue;
          ctx.fillStyle = css(palette.inksB[instances[o + 4]] ?? palette.inksB[0]);
          roundRect(ctx, instances[o] + ox, instances[o + 1] + oy, instances[o + 2], instances[o + 3], RADIUS);
          ctx.fill();
        }
      }

      for (let o = 0; o < instances.length; o += STRIDE) {
        const x = instances[o];
        const y = instances[o + 1];
        const w = instances[o + 2];
        const h = instances[o + 3];
        const ink = palette.inks[instances[o + 4]] ?? palette.inks[0];
        const velocity = instances[o + 5];
        const flags = instances[o + 6];
        const lit = velocity >= 0;
        const playhead = (flags & FLAG_PLAYHEAD) !== 0;
        if (flags & FLAG_BAR) {
          // A roll preview's note bar: solid ink.
          ctx.fillStyle = css(ink);
          roundRect(ctx, x, y, w, h, Math.min(RADIUS, w / 2, h / 2));
          ctx.fill();
          continue;
        }

        let base = lit ? palette.stepLit : flags & FLAG_BEAT_B ? palette.stepB : palette.stepA;
        if (!lit && playhead) base = mix(base, palette.playhead, PLAYHEAD_MIX);
        ctx.fillStyle = css(base);
        roundRect(ctx, x, y, w, h, RADIUS);
        ctx.fill();
        if (lit) {
          const fill = fillHeight(h, velocity);
          ctx.save();
          ctx.clip();
          ctx.fillStyle = css(ink);
          ctx.fillRect(x, y + h - fill, w, fill);
          ctx.restore();
        }

        const focus = (flags & FLAG_FOCUS) !== 0;
        if (focus || (playhead && lit)) {
          const out = RING_GAP + RING_WIDTH / 2;
          ctx.strokeStyle = css(focus ? palette.focus : palette.playhead);
          ctx.lineWidth = RING_WIDTH;
          roundRect(ctx, x - out, y - out, w + 2 * out, h + 2 * out, RADIUS + out);
          ctx.stroke();
        }
      }
    },
    dispose() {},
  };
}
