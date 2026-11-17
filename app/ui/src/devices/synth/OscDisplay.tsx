import { useRef } from "react";
import { useBoxSize, useVerticalDrag, type ParamControl } from "../controls/gesture";
import type { ParamRange } from "../controls/paramDrag";
import { shapeName } from "./synthLayout";
import { oscWave } from "./waveShapes";

interface Props {
  control: ParamControl;
  n: 1 | 2;
  shapeParam: ParamRange;
  shape: number;
  level: number;
  unison: number;
  detune: number;
  spread: number;
}

const path = (pts: [number, number][]) => pts.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)} ${y.toFixed(1)}`).join("");

/** One cycle of the oscillator, morphing with its shape; the unison copies fan out behind it
 * with detune and spread. Drag up or down to morph. */
export function OscDisplay({ n, shapeParam, shape, level, unison, detune, spread, control }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const { w, h } = useBoxSize(host);
  const { dragging, pointer } = useVerticalDrag(shapeParam, shape, control, host);

  const padX = 10;
  const top = 20;
  const bottom = h - (unison > 1 ? 18 : 10);
  const mid = (top + bottom) / 2;
  const amp = (bottom - top) / 2;
  const width = w - 2 * padX;
  const curve = (stretch: number, phase: number, xo: number, scale: number): [number, number][] => {
    const pts: [number, number][] = [];
    for (let x = 0; x <= width; x += 2) pts.push([padX + x + xo, mid - oscWave(shape, (x / width) * stretch + phase) * amp * scale]);
    return pts;
  };
  const main = w > 0 ? curve(1, 0, 0, 1) : [];
  const off = level <= 0.001;
  const voices = Array.from({ length: unison > 1 ? unison : 0 }, (_, i) => (i / (unison - 1)) * 2 - 1).filter(
    (pos) => !(unison % 2 && Math.abs(pos) < 1e-6),
  );

  return (
    <div
      ref={host}
      className={`dwell osc-display${off ? " off" : ""}${dragging ? " dragging" : ""}`}
      data-hint={`Osc ${n}'s waveform. Drag up or down to morph its shape from sine to square.`}
      {...pointer}
    >
      {w > 0 && (
        <svg width={w} height={h} viewBox={`0 0 ${w} ${h}`} role="img" aria-label={`Osc ${n} waveform: ${shapeName(shape)}`}>
          <line className="dw-grid" x1={0} x2={w} y1={Math.round(mid) + 0.5} y2={Math.round(mid) + 0.5} />
          {[1, 2, 3].map((i) => {
            const x = Math.round(padX + (width * i) / 4) + 0.5;
            return <line key={i} className="dw-grid" x1={x} x2={x} y1={top - 3} y2={bottom + 3} />;
          })}
          {voices.map((pos) => (
            <path
              key={pos}
              className="dw-ghost"
              style={{ opacity: 0.18 + 0.3 * (1 - Math.abs(pos) * 0.55) }}
              d={path(curve(1 + pos * detune * 0.07, pos * detune * 0.18, pos * spread * width * 0.055, 0.95 - Math.abs(pos) * 0.06))}
            />
          ))}
          <path className="dw-fill" d={`${path(main)}L${padX + width} ${mid}L${padX} ${mid}Z`} />
          <path className="dw-line" d={path(main)} />
          <text className="dw-label" x={8} y={13}>
            {shapeName(shape)}
          </text>
          {off && (
            <text className="dw-label end" x={w - 8} y={13}>
              Off
            </text>
          )}
          {unison > 1 && (
            <g className="dw-uni">
              <line className="dw-grid" x1={w / 2 - 48} x2={w / 2 + 48} y1={h - 8.5} y2={h - 8.5} />
              {voices.concat(unison % 2 ? [0] : []).map((pos) => (
                <circle key={pos} cx={w / 2 + pos * spread * 48} cy={h - 8.5} r={2.3} />
              ))}
            </g>
          )}
        </svg>
      )}
    </div>
  );
}
