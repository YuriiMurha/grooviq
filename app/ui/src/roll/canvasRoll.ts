import { FALLBACK, type Rgba } from "../rack/palette";
import { GHOST_ALPHA, type RollPalette, type RollRenderer, type RollScene } from "./glRoll";
import { FLAG_GHOST, FLAG_SELECTED, FLAG_SLIDE, INK_SHIFT, INSTANCE_STRIDE } from "./noteStore";

// The piano roll on a 2D canvas, for when WebGL2 is not there: the same picture, drawing only
// the rows, lines and notes in view.

const BLACK = new Set([1, 3, 6, 8, 10]);
const STEP = 240;
const QUIET = 0.45;
const RING = 1.5;

const css = ([r, g, b, a]: Rgba, scale = 1) =>
  `rgba(${Math.round(r * scale * 255)}, ${Math.round(g * scale * 255)}, ${Math.round(b * scale * 255)}, ${a})`;

/** The outline of a note as a path: a box, or for a slide note one whose right edge leans from
 * the top corner in towards the bottom. `inset` shrinks it all round. */
function outline(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, slide: boolean, inset: number) {
  const lean = slide ? Math.min(h, w / 2) : 0;
  ctx.beginPath();
  ctx.moveTo(x + inset, y + inset);
  ctx.lineTo(x + w - inset, y + inset);
  ctx.lineTo(x + w - lean - inset, y + h - inset);
  ctx.lineTo(x + inset, y + h - inset);
  ctx.closePath();
}

export function createCanvasRoll(canvas: HTMLCanvasElement): RollRenderer | null {
  const ctx = canvas.getContext("2d", { alpha: false });
  if (!ctx) return null;
  let data: Float32Array = new Float32Array(0);
  let count = 0;

  return {
    kind: "canvas2d",
    setNotes(d, n) {
      data = d;
      count = n;
    },
    draw(s: RollScene, p: RollPalette) {
      const v = s.view;
      ctx.setTransform(s.dpr, 0, 0, s.dpr, 0, 0);
      const hair = 1 / s.dpr;
      const lastTick = v.scrollTick + v.width / v.tickPx;
      const bottomKey = v.topKey - v.height / v.keyPx;
      const y = (key: number) => (v.topKey - key) * v.keyPx;

      // Rows, then their lines.
      ctx.fillStyle = css(p.outside);
      ctx.fillRect(0, 0, v.width, v.height);
      for (let k = Math.max(0, Math.floor(bottomKey)); k <= Math.min(127, Math.floor(v.topKey)); k++) {
        ctx.fillStyle = css(BLACK.has(k % 12) ? p.rowBlack : p.row);
        ctx.fillRect(0, y(k + 1), v.width, v.keyPx);
        if (s.scaleMask && ((s.scaleMask >> k % 12) & 1) === 0) {
          ctx.fillStyle = css(p.dim);
          ctx.fillRect(0, y(k + 1), v.width, v.keyPx);
        }
        ctx.fillStyle = css(k % 12 === 0 ? p.beat : p.line);
        ctx.fillRect(0, y(k) - hair, v.width, hair);
      }
      const lines = (period: number, colour: Rgba) => {
        if (period * v.tickPx < 6) return;
        ctx.fillStyle = css(colour);
        for (let t = Math.ceil(v.scrollTick / period) * period; t <= lastTick; t += period) {
          ctx.fillRect((t - v.scrollTick) * v.tickPx, 0, hair, v.height);
        }
      };
      lines(STEP, p.line);
      lines(s.beatTicks, p.beat);
      lines(s.barTicks, p.bar);
      if (s.endTick < lastTick) {
        const x = Math.max(0, (s.endTick - v.scrollTick) * v.tickPx);
        ctx.fillStyle = css([...p.outside.slice(0, 3), 0.6] as Rgba);
        ctx.fillRect(x, 0, v.width - x, v.height);
      }

      // The notes in view, the selected ones moved by the drag.
      const own = Math.min(5, Math.max(0, s.ink));
      for (let i = 0; i < count; i++) {
        const o = i * INSTANCE_STRIDE;
        const flags = data[o + 4] | 0;
        const selected = (flags & FLAG_SELECTED) !== 0;
        const slide = (flags & FLAG_SLIDE) !== 0;
        let tick = data[o];
        let len = data[o + 1];
        let key = data[o + 2];
        if (selected) {
          tick += s.drag[0];
          key += s.drag[1];
          if (s.resize !== 0) len = Math.max(s.minLength, len + s.resize);
        }
        if (tick > lastTick || tick + len < v.scrollTick || key < bottomKey - 1 || key > v.topKey) continue;
        const x = (tick - v.scrollTick) * v.tickPx;
        const w = Math.max(len * v.tickPx, 2);
        const top = y(key + 1);
        const h = v.keyPx - 1;
        const index = flags >> INK_SHIFT === 0 ? own : (flags >> INK_SHIFT) - 1;
        const ink = (slide ? p.inksB : p.inks)[index] ?? FALLBACK;
        ctx.globalAlpha = (flags & FLAG_GHOST) !== 0 ? GHOST_ALPHA : 1;
        if (selected) {
          ctx.fillStyle = css(p.select);
          outline(ctx, x, top, w, h, slide, 0);
          ctx.fill();
        }
        ctx.fillStyle = css(ink, QUIET + (1 - QUIET) * Math.min(1, Math.max(0, data[o + 3])));
        outline(ctx, x, top, w, h, slide, selected ? RING : 0);
        ctx.fill();
      }
      ctx.globalAlpha = 1;

      if (s.playhead !== null) {
        ctx.fillStyle = css(p.playhead);
        ctx.fillRect((s.playhead - v.scrollTick) * v.tickPx - 1, 0, 2, v.height);
      }
      if (s.box) {
        const x = Math.min(s.box.x0, s.box.x1);
        const top = Math.min(s.box.y0, s.box.y1);
        const w = Math.abs(s.box.x1 - s.box.x0);
        const h = Math.abs(s.box.y1 - s.box.y0);
        ctx.fillStyle = css(p.box);
        ctx.fillRect(x, top, w, h);
        ctx.strokeStyle = css(p.select);
        ctx.lineWidth = 1;
        ctx.strokeRect(x + 0.5, top + 0.5, w - 1, h - 1);
      }
    },
    dispose() {},
  };
}
