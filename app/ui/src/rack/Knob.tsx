import { useRef, useState, type CSSProperties, type KeyboardEvent, type PointerEvent } from "react";

/** Pixels of vertical drag for the knob's whole range. */
const DRAG_RANGE_PX = 100;
/** Pixels a press must move before it counts as a drag. */
const DRAG_START_PX = 2;

const round2 = (n: number) => Math.round(n * 100) / 100;

interface Props {
  label: string;
  value: number;
  min: number;
  max: number;
  /** What one arrow key press changes. */
  step: number;
  /** What a double-click puts back. */
  defaultValue: number;
  /** The value as words, for the slider's `aria-valuetext`. */
  format: (value: number) => string;
  hint: string;
  /** Called with a new value (clamped, two decimals) when it differs; a drag's value stays shown
   * until the promise settles. */
  onCommit: (value: number) => Promise<unknown>;
}

/** A small rotary control: drag up or down (one edit, on release), arrow keys, Home and End, and
 * a double-click for the default. */
export function Knob({ label, value, min, max, step, defaultValue, format, hint, onCommit }: Props) {
  const [dragged, setDragged] = useState<number | null>(null);
  const drag = useRef<{ y: number; start: number; moved: boolean } | null>(null);
  const clamp = (n: number) => round2(Math.min(max, Math.max(min, n)));
  const shown = dragged ?? value;

  const commit = (next: number): Promise<unknown> | null => {
    const v = clamp(next);
    return v !== value ? onCommit(v) : null;
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const moves: Record<string, number> = {
      ArrowUp: value + step,
      ArrowRight: value + step,
      ArrowDown: value - step,
      ArrowLeft: value - step,
      PageUp: value + step * 4,
      PageDown: value - step * 4,
      Home: min,
      End: max,
    };
    const next = moves[e.key];
    if (next === undefined) return;
    e.preventDefault();
    void commit(next);
  };

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    drag.current = { y: e.clientY, start: value, moved: false };
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* no pointer capture here */
    }
  };
  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d) return;
    const dy = d.y - e.clientY;
    if (!d.moved && Math.abs(dy) < DRAG_START_PX) return;
    d.moved = true;
    setDragged(clamp(d.start + (dy / DRAG_RANGE_PX) * (max - min)));
  };
  const onPointerUp = () => {
    const d = drag.current;
    drag.current = null;
    const sent = d?.moved && dragged !== null ? commit(dragged) : null;
    if (sent) void sent.finally(() => setDragged(null));
    else setDragged(null);
  };

  const fraction = (shown - min) / (max - min);
  return (
    <div
      className="knob"
      role="slider"
      tabIndex={0}
      aria-label={label}
      aria-valuemin={min}
      aria-valuemax={max}
      aria-valuenow={shown}
      aria-valuetext={format(shown)}
      data-hint={hint}
      style={{ "--v": String(fraction) } as CSSProperties}
      onKeyDown={onKeyDown}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={() => {
        drag.current = null;
        setDragged(null);
      }}
      onDoubleClick={() => void commit(defaultValue)}
    />
  );
}
