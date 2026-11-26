import { useRef, useState, type PointerEvent } from "react";
import { drawnScale, useBoxSize, useVerticalDrag, type ParamControl } from "../controls/gesture";
import type { GestureGroup } from "../paramGestures";
import { quantize, type ParamRange } from "../controls/paramDrag";
import { filterDb, lfoWave, randomSteps } from "./waveShapes";

const path = (pts: [number, number][]) => pts.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)} ${y.toFixed(1)}`).join("");

/** The display's frequency axis: 20 Hz to 20 kHz, three decades. */
const F_LO = 20;
const DECADES = 3;
const DB_TOP = 24;
const DB_BOTTOM = -36;
/** Pixels of vertical drag for the whole resonance range. */
const RES_PX = 160;
/** Octaves the filter envelope sweeps at full amount (display only). */
const ENV_OCTAVES = 6;

interface FilterProps {
  /** `filterDb`'s type, or `null` for a filter that is off (a flat line). */
  type: number | null;
  cutoff: { id: string; param: ParamRange; value: number };
  res: { id: string; param: ParamRange; value: number };
  drive: number;
  envAmount: number;
  /** A drag of the editor's owner over several parameters. */
  group: () => GestureGroup;
}

/** The filter's response; its knee is the cutoff. Drag sideways for the cutoff, up or down for
 * the resonance. */
export function FilterDisplay({ type, cutoff, res, drive, envAmount, group }: FilterProps) {
  const host = useRef<HTMLDivElement>(null);
  const { w, h } = useBoxSize(host);
  const [dragging, setDragging] = useState(false);
  const drag = useRef<{ x: number; y: number; cutoff: number; res: number; scale: number; gestures: GestureGroup } | null>(null);
  const top = 10;
  const bottom = h - 16;
  const fx = (f: number) => (Math.log10(f / F_LO) / DECADES) * w;
  const xf = (x: number) => F_LO * Math.pow(10, (Math.min(w, Math.max(0, x)) / Math.max(1, w)) * DECADES);
  const yd = (db: number) => top + ((DB_TOP - Math.min(DB_TOP + 3, Math.max(DB_BOTTOM - 8, db))) / (DB_TOP - DB_BOTTOM)) * (bottom - top);

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* no pointer capture here */
    }
    drag.current?.gestures.cancel();
    drag.current = { x: e.clientX, y: e.clientY, cutoff: cutoff.value, res: res.value, scale: drawnScale(e.currentTarget), gestures: group() };
    setDragging(true);
  };
  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d) return;
    const fine = e.shiftKey ? 0.2 : 1;
    const dx = ((e.clientX - d.x) / d.scale) * fine;
    const dy = ((d.y - e.clientY) / d.scale) * fine;
    d.x = e.clientX;
    d.y = e.clientY;
    const c = quantize(cutoff.param, xf(fx(d.cutoff) + dx));
    const r = quantize(res.param, d.res + (dy / RES_PX) * (res.param.max - res.param.min));
    let alive = true;
    if (c !== d.cutoff) {
      d.cutoff = c;
      alive = d.gestures.input(cutoff.id, c);
    }
    if (alive && r !== d.res) {
      d.res = r;
      alive = d.gestures.input(res.id, r);
    }
    // The editor ended it (Escape, focus lost, a preset): let the rest of it go.
    if (!alive) finish(false);
  };
  const finish = (commit: boolean) => {
    const d = drag.current;
    drag.current = null;
    setDragging(false);
    if (!d) return;
    if (commit) d.gestures.commit();
    else d.gestures.cancel();
  };

  const db = (f: number) => (type === null ? 0 : filterDb(type, f, cutoff.value, res.value, drive));
  const N = Math.max(90, Math.round(w / 2));
  const curve: [number, number][] = [];
  if (w > 0) for (let i = 0; i <= N; i++) curve.push([(i / N) * w, yd(db(xf((i / N) * w)))]);
  const nx = fx(cutoff.value);
  const ny = Math.min(bottom, Math.max(top, yd(db(cutoff.value))));
  const y0 = Math.round(yd(0)) + 0.5;
  const sweep = Math.abs(envAmount) > 0.02 ? fx(Math.min(20000, Math.max(20, cutoff.value * Math.pow(2, envAmount * ENV_OCTAVES)))) : null;
  const ya = Math.min(bottom - 4, Math.max(top + 3, ny - 14));
  const dir = sweep === null ? 1 : Math.sign(sweep - nx) || 1;

  return (
    <div
      ref={host}
      className={`dwell filter-display${dragging ? " dragging" : ""}${type === null ? " off" : ""}`}
      data-hint="The filter's response. Drag sideways for the cutoff, up or down for the resonance (Shift for fine)."
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={() => finish(true)}
      onPointerCancel={() => finish(false)}
      onLostPointerCapture={() => finish(false)}
    >
      {w > 0 && (
        <svg width={w} height={h} viewBox={`0 0 ${w} ${h}`} role="img" aria-label="Filter response">
          {[50, 100, 200, 500, 1000, 2000, 5000, 10000].map((f) => {
            const x = Math.round(fx(f)) + 0.5;
            return <line key={f} className={`dw-grid${f === 100 || f === 1000 || f === 10000 ? "" : " minor"}`} x1={x} x2={x} y1={0} y2={h} />;
          })}
          <line className="dw-grid dash" x1={0} x2={w} y1={y0} y2={y0} />
          {(
            [
              [100, "100"],
              [1000, "1k"],
              [10000, "10k"],
            ] as const
          ).map(([f, s]) => {
            // Set the last tick inside its grid line at every graph width and font size.
            const last = f === 10000;
            return (
              <text key={f} className={last ? "dw-axis end" : "dw-axis"} x={fx(f) + (last ? -3 : 3)} y={h - 4}>
                {s}
              </text>
            );
          })}
          <text className="dw-axis end" x={w - 4} y={y0 - 3}>
            0 dB
          </text>
          <path className="dw-fill" d={`${path(curve)}L${w} ${h}L0 ${h}Z`} />
          <path className="dw-line" d={path(curve)} />
          {sweep !== null && (
            <path className="dw-sweep" d={`M${nx} ${ya}L${sweep - dir * 3} ${ya}M${sweep - dir * 6} ${ya - 3.5}L${sweep} ${ya}L${sweep - dir * 6} ${ya + 3.5}`} />
          )}
          <circle className="dw-node" cx={nx} cy={ny} r={5} />
        </svg>
      )}
    </div>
  );
}

interface LfoProps {
  control: ParamControl;
  shape: number;
  rate: number;
  amountParam: ParamRange;
  amount: number;
}

/** Two cycles of the LFO, as tall as its amount. Drag up or down for the amount. */
export function LfoDisplay({ shape, rate, amountParam, amount, control }: LfoProps) {
  const host = useRef<HTMLDivElement>(null);
  const { w, h } = useBoxSize(host);
  const { dragging, pointer } = useVerticalDrag(amountParam, amount, control, host);
  const pad = 8;
  const mid = h / 2;
  const amp = (h / 2 - pad) * Math.max(0.04, amount);
  const cycles = 2;
  const steps = randomSteps(cycles);
  const pts: [number, number][] = [];
  if (w > 0) {
    for (let x = 0; x <= w - 2 * pad; x += 1.5) {
      const ph = (x / (w - 2 * pad)) * cycles;
      const v = shape === 4 ? steps[Math.min(cycles - 1, Math.floor(ph))] : lfoWave(shape, ph);
      pts.push([pad + x, mid - v * amp]);
    }
  }
  return (
    <div
      ref={host}
      className={`dwell lfo-display${dragging ? " dragging" : ""}`}
      data-hint="The LFO. Drag up or down to change its amount (Shift for fine)."
      {...pointer}
    >
      {w > 0 && (
        <svg width={w} height={h} viewBox={`0 0 ${w} ${h}`} role="img" aria-label="LFO">
          <line className="dw-grid" x1={0} x2={w} y1={Math.round(mid) + 0.5} y2={Math.round(mid) + 0.5} />
          <path className="dw-line" d={path(pts)} style={{ opacity: amount > 0.001 ? 1 : 0.4 }} />
          <ellipse className="dw-node" data-synth-meter="lfo" data-meter-left={pad} data-meter-right={w - pad} data-meter-amount={amount} data-meter-top={pad} data-meter-bottom={h - pad} cx={w - 8} cy={mid} rx={3} ry={3} style={{ visibility: "hidden" }} />
          <text className="dw-axis end" x={w - 5} y={h - 5}>
            {`${rate < 10 ? rate.toFixed(2) : rate.toFixed(1)} Hz`}
          </text>
        </svg>
      )}
    </div>
  );
}

/** How a slide moves from one note to the next over the glide time (display only). */
export function GlideDisplay({ glide, max }: { glide: number; max: number }) {
  const host = useRef<HTMLDivElement>(null);
  const { w, h } = useBoxSize(host);
  const pad = 8;
  const span = w - 2 * pad;
  const t = Math.sqrt(glide / max);
  const x1 = pad + span * (0.15 + 0.7 * t);
  const lo = h - pad - 4;
  const hi = pad + 4;
  return (
    <div ref={host} className="dwell glide-display" data-hint="How long a note slides to the next one in mono and legato.">
      {w > 0 && (
        <svg width={w} height={h} viewBox={`0 0 ${w} ${h}`} role="img" aria-label="Glide">
          <line className="dw-grid" x1={pad} x2={w - pad} y1={lo} y2={lo} />
          <line className="dw-grid" x1={pad} x2={w - pad} y1={hi} y2={hi} />
          <path className="dw-line" d={`M${pad} ${lo}L${pad + span * 0.15} ${lo}L${x1} ${hi}L${w - pad} ${hi}`} />
        </svg>
      )}
    </div>
  );
}
