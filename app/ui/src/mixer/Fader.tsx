import { useRef, type CSSProperties, type KeyboardEvent } from "react";
import { useVerticalDrag, type ParamControl } from "../devices/controls/gesture";
import { keyValue, toNorm, type ParamRange } from "../devices/controls/paramDrag";

/** The fader's travel in CSS px: a drag follows the pointer one to one. */
export const FADER_PX = 148;

interface FaderProps {
  control: ParamControl;
  param: ParamRange;
  value: number;
  /** The accessible name ("Kick level"). */
  name: string;
  format: (value: number) => string;
  disabled?: boolean;
}

/** A vertical channel fader with the device controls' gestures: drag (Shift for fine), wheel,
 * arrow keys, Page keys, Home/End and a double-click for the default. */
export function Fader({ control, param, value, name, format, disabled }: FaderProps) {
  const el = useRef<HTMLDivElement>(null);
  const range = { ...param, dragPixels: FADER_PX };
  const { dragging, pointer } = useVerticalDrag(range, value, control, el);
  const n = toNorm(param, value);
  const zero = toNorm(param, 0);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const next = keyValue(param, value, e.key, e.shiftKey);
    if (next === null) return;
    e.preventDefault();
    if (next !== value) control.set(next);
  };

  return (
    <div
      ref={el}
      className={`mx-fader${dragging ? " dragging" : ""}`}
      role="slider"
      tabIndex={disabled ? -1 : 0}
      aria-label={name}
      aria-orientation="vertical"
      aria-disabled={disabled || undefined}
      aria-valuemin={param.min}
      aria-valuemax={param.max}
      aria-valuenow={Number(value.toFixed(2))}
      aria-valuetext={format(value)}
      style={{ "--n": n } as CSSProperties}
      data-hint="Drag up or down (Shift for fine), scroll or use the arrow keys; double-click resets it to 0 dB."
      onKeyDown={onKeyDown}
      onDoubleClick={() => {
        if (value !== param.default) control.set(param.default);
      }}
      {...pointer}
    >
      <span className="mx-fader-track" aria-hidden="true">
        <span className="mx-fader-fill" style={{ transform: `scaleY(${n})` }} />
        <span className="mx-fader-zero" style={{ bottom: `${zero * 100}%` }} />
      </span>
      <span className="mx-fader-cap" aria-hidden="true" />
    </div>
  );
}
