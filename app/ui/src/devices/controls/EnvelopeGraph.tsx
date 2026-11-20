import { useRef, useState, type PointerEvent } from "react";
import type { GestureGroup } from "../paramGestures";
import { drawnScale, useBoxSize } from "./gesture";
import { dragHandle, envCurve, envPoints, handleAt, type EnvBox, type EnvHandle, type EnvShape } from "./envelope";
import { fromNorm, toNorm, type ParamRange } from "./paramDrag";

/** Pixels from a point that still grab it. */
const REACH = 14;
const PAD = 9;

export interface EnvParam {
  id: string;
  param: ParamRange;
  value: number;
}

export interface EnvelopeGraphProps {
  /** The accessible name ("Amp envelope"). */
  name: string;
  meter?: "amp" | "flt";
  attack: EnvParam;
  decay: EnvParam;
  sustain: EnvParam;
  release: EnvParam;
  /** A drag of the editor's owner over several parameters. */
  group: () => GestureGroup;
}

const HANDLES: Record<EnvHandle, ("attack" | "decay" | "sustain" | "release")[]> = {
  attack: ["attack"],
  decay: ["decay", "sustain"],
  release: ["release"],
};

/** An ADSR curve with three points to drag: the peak (attack time), the decay end (decay time
 * and sustain level) and the release end (release time). The number fields beside it are the
 * keyboard way in. */
export function EnvelopeGraph({ meter, name, attack, decay, sustain, release, group }: EnvelopeGraphProps) {
  const host = useRef<HTMLDivElement>(null);
  const { w, h } = useBoxSize(host);
  const [hot, setHot] = useState<EnvHandle | null>(null);
  // `dx`, `dy`: where the pointer grabbed the point, from its centre, so a grab never moves it.
  const drag = useRef<{ handle: EnvHandle; dx: number; dy: number; gestures: GestureGroup } | null>(null);
  const params = { attack, decay, sustain, release };
  const shape: EnvShape = {
    attack: toNorm(attack.param, attack.value),
    decay: toNorm(decay.param, decay.value),
    sustain: toNorm(sustain.param, sustain.value),
    release: toNorm(release.param, release.value),
  };
  const box: EnvBox = { left: PAD, top: PAD + 2, width: Math.max(1, w - 2 * PAD), height: Math.max(1, h - 2 * PAD - 2) };

  const local = (e: PointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    const s = drawnScale(e.currentTarget);
    return [(e.clientX - r.left) / s, (e.clientY - r.top) / s] as const;
  };

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const [x, y] = local(e);
    const handle = handleAt(shape, box, x, y, REACH);
    if (!handle) return;
    e.preventDefault();
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* no pointer capture here */
    }
    const p = envPoints(shape, box);
    const [hx, hy] = handle === "attack" ? [p.xA, p.yTop] : handle === "decay" ? [p.xD, p.yS] : [p.xR, p.yBottom];
    drag.current?.gestures.cancel();
    drag.current = { handle, dx: x - hx, dy: y - hy, gestures: group() };
    setHot(handle);
  };
  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const [x, y] = local(e);
    const d = drag.current;
    if (!d) {
      setHot(handleAt(shape, box, x, y, REACH));
      return;
    }
    const next = dragHandle(shape, box, d.handle, x - d.dx, y - d.dy);
    for (const key of HANDLES[d.handle]) {
      const p = params[key];
      // Only what the pointer moved changes (a straight-up drag keeps the decay time).
      if (Math.abs(next[key] - shape[key]) < 1e-9) continue;
      const v = fromNorm(p.param, next[key]);
      if (v === p.value) continue;
      if (!d.gestures.input(p.id, v)) {
        // The editor ended it (Escape, focus lost, a preset): every member is already back.
        drag.current = null;
        setHot(null);
        return;
      }
    }
  };
  const finish = (commit: boolean) => {
    const d = drag.current;
    drag.current = null;
    if (!d) return;
    if (commit) d.gestures.commit();
    else d.gestures.cancel();
  };

  const p = envPoints(shape, box);
  const curve = w > 0 ? envCurve(shape, box) : [];
  const line = curve.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)} ${y.toFixed(1)}`).join("");
  const fill = curve.length ? `${line}L${p.xR.toFixed(1)} ${p.yBottom}L${box.left} ${p.yBottom}Z` : "";
  const point = (k: EnvHandle, x: number, y: number) => (
    <circle key={k} className={`env-pt${hot === k ? " hot" : ""}`} cx={x} cy={y} r={hot === k ? 4.6 : 3.4} />
  );

  return (
    <div
      ref={host}
      className={`dwell env-graph${hot ? " grab" : ""}`}
      data-hint={`${name}: drag the points for attack, decay, sustain and release.`}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={() => finish(true)}
      onPointerCancel={() => finish(false)}
      onLostPointerCapture={() => finish(false)}
      onPointerLeave={() => {
        if (!drag.current) setHot(null);
      }}
    >
      {w > 0 && (
        <svg width={w} height={h} viewBox={`0 0 ${w} ${h}`} role="img" aria-label={name}>
          <line className="dw-grid" x1={0} x2={w} y1={p.yBottom + 0.5} y2={p.yBottom + 0.5} />
          {[p.xA, p.xD, p.xS].map((x, i) => (
            <line key={i} className="dw-grid dash" x1={Math.round(x) + 0.5} x2={Math.round(x) + 0.5} y1={box.top - 2} y2={p.yBottom} />
          ))}
          <path className="dw-fill" d={fill} />
          <path className="dw-line" d={line} />
          {meter && <ellipse className="dw-node" data-synth-meter={meter} data-meter-top={box.top} data-meter-bottom={p.yBottom} cx={w - 8} cy={p.yBottom} rx={3} ry={3} style={{ visibility: "hidden" }} />}
          {point("attack", p.xA, p.yTop)}
          {point("decay", p.xD, p.yS)}
          {point("release", p.xR, p.yBottom)}
        </svg>
      )}
    </div>
  );
}
