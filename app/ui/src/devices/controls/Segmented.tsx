import { useRef, type KeyboardEvent, type ReactNode } from "react";

export interface SegmentedProps {
  /** The parameter id, for `data-param` (none for a setting that is not a parameter). */
  id?: string;
  /** The accessible name ("Filter type"). */
  name: string;
  /** One label per value, from `min` up. */
  options: string[];
  /** Drawn instead of the label (the label stays the button's name). */
  icons?: ReactNode[];
  min: number;
  value: number;
  /** What it does (one sentence). */
  does: string;
  /** Called with the chosen value when it changes. */
  onChange: (value: number) => void;
  className?: string;
}

/** A row of buttons for an enum parameter, one per value: click, or the arrow keys (a radio
 * group, so Tab enters and leaves it as one stop). */
export function Segmented({ id, name, options, icons, min, value, does, onChange, className }: SegmentedProps) {
  const group = useRef<HTMLDivElement>(null);
  const index = Math.round(value) - min;

  const choose = (i: number, focus: boolean) => {
    const j = Math.min(options.length - 1, Math.max(0, i));
    if (focus) group.current?.querySelectorAll<HTMLButtonElement>("button")[j]?.focus();
    if (j !== index) onChange(j + min);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
    if (e.key === "Home" || e.key === "End") {
      e.preventDefault();
      choose(e.key === "Home" ? 0 : options.length - 1, true);
    } else if (step !== undefined) {
      e.preventDefault();
      choose(index + step, true);
    }
  };

  return (
    <div
      ref={group}
      className={`dseg${className ? ` ${className}` : ""}`}
      role="radiogroup"
      aria-label={name}
      data-param={id}
      data-hint={`${does} Click one, or use the arrow keys.`}
      onKeyDown={onKeyDown}
    >
      {options.map((label, i) => (
        <button
          key={label}
          type="button"
          role="radio"
          aria-checked={i === index}
          aria-label={icons ? label : undefined}
          title={icons ? label : undefined}
          tabIndex={i === index ? 0 : -1}
          className={i === index ? "on" : undefined}
          onClick={() => choose(i, false)}
        >
          {icons ? icons[i] : label}
        </button>
      ))}
    </div>
  );
}
