import { registerGestureCancellation } from "../project/pending";
import { isProjectReplacing } from "../project/replacement";
import type { PatternStamp } from "../api/patterns";
import { useEffect, useRef, type PointerEvent, type RefObject } from "react";
import type { CommitQueue } from "./commitQueue";
import { FALLBACK } from "../rack/palette";
import { readRollPalette } from "./glRoll";
import type { NoteStore } from "./noteStore";
import { laneEdit, laneSet, velocityAt } from "./velocity";
import type { RollView } from "./view";

/** The lane's height in CSS pixels. */
export const LANE_HEIGHT = 72;
/** How far from a bar's top the pointer still grabs it, in CSS pixels. */
const REACH_PX = 4;
const BAR_PX = 3;

interface Props {
  store: NoteStore;
  queue: CommitQueue;
  channel: number | null;
  /** The roll's view: the lane scrolls and zooms with it. */
  view: RefObject<RollView>;
  /** The channel's ink, 0..5. */
  ink: number;
}

/** The velocity lane under the roll: one bar per note, as tall as its velocity. Dragging sets
 * the selected notes' velocity to the pointer's height (or paints the bars passed over when
 * nothing is selected) and commits once, when the pointer lets go. */
export function VelocityLane({ store, queue, channel, view, ink }: Props) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const wrap = useRef<HTMLDivElement>(null);
  const live = useRef({ channel, ink });
  /** A drag under way: the velocities it shows, and the project it started in (its edit is that
   * project's, and is dropped if another replaced it). */
  const drag = useRef<{ values: Map<number, number>; rev: number; origin: PatternStamp } | null>(null);
  const redraw = useRef(true);
  useEffect(() => registerGestureCancellation(() => { drag.current = null; redraw.current = true; }), []);
  useEffect(() => {
    live.current = { channel, ink };
    redraw.current = true;
  });

  useEffect(() => {
    const el = canvas.current;
    const box = wrap.current;
    if (!el || !box) return;
    const ctx = el.getContext("2d", { alpha: false });
    if (!ctx) return;
    const palette = readRollPalette(document.documentElement);
    const css = (c: number[]) => `rgb(${Math.round(c[0] * 255)}, ${Math.round(c[1] * 255)}, ${Math.round(c[2] * 255)})`;
    let width = 0;
    const ro = new ResizeObserver(() => {
      width = box.clientWidth;
      redraw.current = true;
    });
    ro.observe(box);
    let seen = { version: -1, scroll: NaN, px: NaN, rev: -1 };
    let raf = 0;
    const frame = () => {
      raf = requestAnimationFrame(frame);
      const v = view.current;
      const rev = drag.current?.rev ?? -1;
      if (!redraw.current && seen.version === store.version && seen.scroll === v.scrollTick && seen.px === v.tickPx && seen.rev === rev) return;
      seen = { version: store.version, scroll: v.scrollTick, px: v.tickPx, rev };
      redraw.current = false;
      const dpr = window.devicePixelRatio || 1;
      const w = Math.round(width * dpr);
      const h = Math.round(LANE_HEIGHT * dpr);
      if (el.width !== w || el.height !== h) {
        el.width = w;
        el.height = h;
      }
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.fillStyle = css(palette.outside);
      ctx.fillRect(0, 0, width, LANE_HEIGHT);
      // Guides at full, half and silent.
      ctx.fillStyle = css([0.2, 0.22, 0.34]);
      for (const f of [0, 0.5]) ctx.fillRect(0, f * LANE_HEIGHT, width, 1);
      const { channel: ch, ink: own } = live.current;
      if (ch === null) return;
      const preview = drag.current?.values;
      const colour = palette.inks[own] ?? FALLBACK;
      const lastTick = v.scrollTick + width / v.tickPx;
      for (let i = 0; i < store.count; i++) {
        if (store.channels[i] !== ch) continue;
        const tick = store.ticks[i];
        if (tick > lastTick || tick + BAR_PX / v.tickPx < v.scrollTick) continue;
        const velocity = preview?.get(store.ids[i]) ?? store.velocities[i];
        const top = (1 - Math.min(1, Math.max(0, velocity))) * LANE_HEIGHT;
        ctx.fillStyle = css(store.selected[i] ? palette.select : colour);
        ctx.fillRect((tick - v.scrollTick) * v.tickPx, top, BAR_PX, LANE_HEIGHT - top);
      }
    };
    raf = requestAnimationFrame(frame);
    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
    };
  }, [store, view]);

  const point = (e: PointerEvent<HTMLCanvasElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    const v = view.current;
    return { tick: v.scrollTick + (e.clientX - rect.left) / v.tickPx, value: velocityAt(e.clientY - rect.top, LANE_HEIGHT) };
  };
  const apply = (e: PointerEvent<HTMLCanvasElement>) => {
    const d = drag.current;
    const ch = live.current.channel;
    if (!d || ch === null) return;
    const p = point(e);
    laneSet(store, ch, p.tick, p.value, REACH_PX / view.current.tickPx, d.values);
    d.rev++;
  };

  return (
    <div className="roll-lane">
      <div className="roll-lane-gutter" aria-hidden="true">
        Velocity
      </div>
      <div className="roll-lane-wrap" ref={wrap}>
        <canvas
          ref={canvas}
          className="roll-lane-canvas"
          aria-label="Velocity lane"
          data-hint="Drag to set the velocity of the selected notes. With none selected, drag across the bars to paint them."
          onPointerDown={(e) => {
            if (e.button !== 0 || drag.current || isProjectReplacing()) return;
            e.preventDefault();
            try {
              e.currentTarget.setPointerCapture(e.pointerId);
            } catch {
              /* no pointer capture here */
            }
            drag.current = { values: new Map(), rev: 0, origin: queue.stamp() };
            apply(e);
          }}
          onPointerMove={(e) => drag.current && apply(e)}
          onPointerUp={(e) => {
            const d = drag.current;
            if (!d || !queue.accepts(d.origin)) { drag.current = null; return; }
            apply(e);
            drag.current = null;
            const edit = laneEdit(store, d.values);
            if (edit) queue.commit(edit, d.origin);
            redraw.current = true;
          }}
          onPointerCancel={() => {
            drag.current = null;
            redraw.current = true;
          }}
        />
      </div>
    </div>
  );
}
