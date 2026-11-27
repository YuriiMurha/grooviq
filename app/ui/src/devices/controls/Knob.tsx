import { useRef, type CSSProperties, type KeyboardEvent } from "react";
import { useVerticalDrag, type ParamControl } from "./gesture";
import { keyValue, toNorm, type ParamRange } from "./paramDrag";

/** The knob's arc: 270° from the lower left. */
const ARC_START = 135;
const ARC_SWEEP = 270;
const R = 26;

const polar = (deg: number): [number, number] => {
  const a = (deg * Math.PI) / 180;
  return [32 + R * Math.cos(a), 32 + R * Math.sin(a)];
};
function arc(a0: number, a1: number): string {
  const [lo, hi] = a0 <= a1 ? [a0, a1] : [a1, a0];
  const end = Math.max(hi, lo + 0.5);
  const [x0, y0] = polar(lo);
  const [x1, y1] = polar(end);
  return `M${x0.toFixed(2)} ${y0.toFixed(2)}A${R} ${R} 0 ${end - lo > 180 ? 1 : 0} 1 ${x1.toFixed(2)} ${y1.toFixed(2)}`;
}
const TRACK = arc(ARC_START, ARC_START + ARC_SWEEP);

const GESTURES = "Drag up or down (Shift for fine), scroll or use the arrow keys; double-click resets it.";

export interface KnobProps {
  control: ParamControl;
  /** The parameter id, for `data-param`. */
  id: string;
  param: ParamRange;
  value: number;
  /** The short label under the knob. */
  label: string;
  /** The accessible name ("Osc 1 level"). */
  name: string;
  /** The value as words, with its unit. */
  format: (value: number) => string;
  /** What it does (one sentence); the gestures are added. */
  does: string;
  /** `knob` a rotary control; `field` a small number box (same gestures). */
  variant?: "knob" | "field";
  /** Label and value beside the knob instead of under it. */
  row?: boolean;
  /** Knob diameter, CSS px. */
  size?: number;
  onContextAction?: (anchor: HTMLDivElement, at: { x: number; y: number }) => void;
}

/** A device control: drag vertically (Shift fine), wheel, arrow keys, Home/End, double-click for
 * the default. Drags and wheel turns are gestures of the editor's owner; keys and the
 * double-click are one-shot changes. */
export function Knob({ id, param, value, label, name, format, does, variant = "knob", row, size, control, onContextAction }: KnobProps) {
  const el = useRef<HTMLDivElement>(null);
  const { dragging, pointer } = useVerticalDrag(param, value, control, el);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (onContextAction && (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10"))) {
      e.preventDefault();
      e.stopPropagation();
      if (!e.repeat && !dragging) {
        const rect = e.currentTarget.getBoundingClientRect();
        onContextAction(e.currentTarget, { x: rect.left, y: rect.bottom });
      }
      return;
    }
    const next = keyValue(param, value, e.key, e.shiftKey);
    if (next === null) return;
    e.preventDefault();
    if (next !== value) control.set(next);
  };

  const text = format(value);
  const n = toNorm(param, value);
  const bipolar = param.curve === "lin" && param.min < 0 && param.max > 0;
  const from = ARC_START + ARC_SWEEP * (bipolar ? toNorm(param, 0) : 0);
  const to = ARC_START + ARC_SWEEP * n;

  return (
    <div
      ref={el}
      className={`dctl ${variant === "field" ? "dfield" : "dknob"}${row ? " row" : ""}${dragging ? " dragging" : ""}`}
      role="slider"
      tabIndex={0}
      aria-label={name}
      aria-valuemin={param.min}
      aria-valuemax={param.max}
      aria-valuenow={Number(value.toFixed(4))}
      aria-valuetext={text}
      aria-haspopup={onContextAction ? "menu" : undefined}
      aria-keyshortcuts={onContextAction ? "Shift+F10" : undefined}
      data-param={id}
      data-hint={`${does} ${GESTURES}`}
      style={size ? ({ "--ks": `${size}px` } as CSSProperties) : undefined}
      onKeyDown={onKeyDown}
      onContextMenu={onContextAction && (event => {
        event.preventDefault();
        event.stopPropagation();
        if (!dragging) onContextAction(event.currentTarget, { x: event.clientX, y: event.clientY });
      })}
      onDoubleClick={() => {
        if (value !== param.default) control.set(param.default);
      }}
      {...pointer}
    >
      {variant === "knob" && (
        <svg viewBox="0 0 64 64" aria-hidden="true">
          <path className="dk-track" d={TRACK} />
          {Math.abs(to - from) >= 0.8 && <path className="dk-arc" d={arc(from, to)} />}
          <circle className="dk-body" cx="32" cy="32" r="19" />
          <line className="dk-ptr" x1="32" y1="25" x2="32" y2="15" transform={`rotate(${(to - 270).toFixed(2)} 32 32)`} />
        </svg>
      )}
      <span className="dk-label">{label}</span>
      <span className="dk-value">{text}</span>
    </div>
  );
}
