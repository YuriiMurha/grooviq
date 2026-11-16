import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent, type WheelEvent } from "react";
import { BPM_MAX, BPM_MIN } from "../api/project";

/** Pixels of dragging for one BPM; a wheel notch is about this many `deltaY` units. */
const DRAG_PX = 4;
const WHEEL_UNITS = 40;

const round2 = (n: number) => Math.round(n * 100) / 100;
const clamp = (n: number) => Math.min(BPM_MAX, Math.max(BPM_MIN, n));

interface Props {
  bpm: number;
  /** Called with a new tempo (clamped, two decimals) when it differs from `bpm`. */
  onCommit: (bpm: number) => void;
}

/** The tempo: type it (Enter or leaving the field sets it, and Enter leaves it; Escape gives up),
 * scroll over it, drag it up or down, or use the arrow keys. Shift makes the steps 0.1. */
export function BpmField({ bpm, onCommit }: Props) {
  const [draft, setDraft] = useState<string | null>(null);
  // What the tempo is meant to be right now: `bpm` lags a round trip behind drags and scrolls.
  const target = useRef(bpm);
  const drag = useRef<{ y: number; start: number; moved: boolean } | null>(null);
  const wheel = useRef(0);
  // Set when the field is left on purpose without keeping what was typed (Escape, a drag).
  const discard = useRef(false);

  useEffect(() => {
    target.current = bpm;
  }, [bpm]);

  const send = (value: number) => {
    const next = round2(clamp(value));
    if (next === target.current) return;
    target.current = next;
    onCommit(next);
  };

  const commitText = (text: string) => {
    setDraft(null);
    const value = Number(text.trim().replace(",", "."));
    if (text.trim() === "" || !Number.isFinite(value)) return;
    send(value);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    const step = e.shiftKey ? 0.1 : 1;
    if (e.key === "Enter") {
      commitText(e.currentTarget.value);
      // Done: leave the field, so Space is play/stop again. What was typed is already sent.
      discard.current = true;
      e.currentTarget.blur();
    } else if (e.key === "Escape") {
      discard.current = true;
      setDraft(null);
      e.currentTarget.blur();
    } else if (e.key === "ArrowUp" || e.key === "ArrowDown") {
      e.preventDefault();
      setDraft(null);
      send(target.current + (e.key === "ArrowUp" ? step : -step));
    }
  };

  const onWheel = (e: WheelEvent<HTMLInputElement>) => {
    wheel.current += e.deltaY;
    const notches = Math.trunc(wheel.current / WHEEL_UNITS);
    if (notches === 0) return;
    wheel.current -= notches * WHEEL_UNITS;
    // Scrolling up raises the tempo.
    send(target.current - notches * (e.shiftKey ? 0.1 : 1));
  };

  const onPointerDown = (e: PointerEvent<HTMLInputElement>) => {
    drag.current = { y: e.clientY, start: target.current, moved: false };
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* no pointer capture here */
    }
  };
  const onPointerMove = (e: PointerEvent<HTMLInputElement>) => {
    const d = drag.current;
    if (!d) return;
    const dy = d.y - e.clientY;
    if (!d.moved && Math.abs(dy) < 3) return;
    if (!d.moved) {
      d.moved = true;
      // A drag is not an edit: leave the text field.
      discard.current = true;
      setDraft(null);
      e.currentTarget.blur();
    }
    const unit = e.shiftKey ? 0.1 : 1;
    send(d.start + Math.round(dy / DRAG_PX) * unit);
  };
  const endDrag = () => {
    drag.current = null;
  };

  return (
    <input
      className="bpm-field"
      type="text"
      inputMode="decimal"
      aria-label="BPM"
      data-hint="The tempo in beats per minute, 20 to 400. Type it, scroll over it, or drag it up and down. Hold Shift for fine steps."
      value={draft ?? bpm.toFixed(2)}
      onFocus={(e) => e.currentTarget.select()}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={(e) => {
        if (discard.current) discard.current = false;
        else if (draft !== null) commitText(e.currentTarget.value);
      }}
      onKeyDown={onKeyDown}
      onWheel={onWheel}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
    />
  );
}
