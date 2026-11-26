import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";
import { DeviceScale } from "./deviceScale";
import { DESIGN_H, DESIGN_W, clampPlacement, containPlacement, fitScale, initialPlacement, type Placement } from "./windowPlacement";

const FOCUSABLE = "button:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex='-1'])";

function readViewport() {
  const hintBarHeight = getComputedStyle(document.documentElement).getPropertyValue("--hintbar-h");
  return {
    w: window.innerWidth,
    h: window.innerHeight,
    reserveBottom: Number.parseFloat(hintBarHeight) || 0,
  };
}

interface Props {
  /** The channel's name. */
  title: string;
  /** What the device is ("Synth"). */
  device: string;
  /** The channel's colour, 0..5. */
  color: number;
  /** The order it was opened in among the open windows (where it first appears). */
  index: number;
  /** Stacking order: higher is in front. */
  z: number;
  /** Controls in the title bar, after the title. */
  toolbar?: ReactNode;
  /** Where focus goes back to on close. */
  opener: HTMLElement | null;
  onClose: () => void;
  onRaise: () => void;
  children: ReactNode;
}

/** An in-app floating window for a device: laid out at the design size and scaled down to fit,
 * dragged by its title bar, closed with × or Escape. Focus moves in when it opens, Tab stays
 * inside it, and focus goes back to the opener when it closes. */
export function DeviceWindow({ title, device, color, index, z, toolbar, opener, onClose, onRaise, children }: Props) {
  const box = useRef<HTMLDivElement>(null);
  const [view, setView] = useState(readViewport);
  const scale = fitScale(view.w, view.h, view.reserveBottom);
  const [place, setPlace] = useState<Placement>(() => initialPlacement(view.w, view.h, scale, index, view.reserveBottom));
  const drag = useRef<{ x: number; y: number; start: Placement } | null>(null);
  const shown = clampPlacement(place, view.w, view.h, scale);

  useEffect(() => {
    const onResize = () => {
      const nextView = readViewport();
      const { w, h, reserveBottom } = nextView;
      // A captured drag's origin belongs to the old viewport. Cancel before moving the
      // window so subsequent pointer moves cannot restore that stale placement.
      drag.current = null;
      setPlace((previous) => containPlacement(previous, w, h, fitScale(w, h, reserveBottom), reserveBottom));
      setView(nextView);
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  // Focus in on open, back to the opener on close.
  const back = useRef(opener);
  useLayoutEffect(() => {
    const before = back.current ?? (document.activeElement as HTMLElement | null);
    const root = box.current;
    const first = root?.querySelector<HTMLElement>("[data-autofocus]") ?? root?.querySelector<HTMLElement>(FOCUSABLE);
    first?.focus({ preventScroll: true });
    return () => {
      if (before?.isConnected) before.focus({ preventScroll: true });
    };
  }, []);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      e.preventDefault();
      onClose();
    } else if (e.key === "Tab" && box.current) {
      // Tab cycles inside the window, the same in every engine (WebKit's own Tab skips buttons,
      // which would let focus out). A radio group is one stop: its checked button.
      const items = [...box.current.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((el) => el.tabIndex >= 0);
      if (items.length === 0) return;
      e.preventDefault();
      const at = items.indexOf(document.activeElement as HTMLElement);
      const next = at < 0 ? (e.shiftKey ? items.length - 1 : 0) : (at + (e.shiftKey ? -1 : 1) + items.length) % items.length;
      items[next].focus();
    }
  };

  const onBarDown = (e: PointerEvent<HTMLDivElement>) => {
    // Only the bar itself drags, not the controls on it.
    if (e.button !== 0 || (e.target as Element).closest("button, [role='slider'], [role='radiogroup']")) return;
    e.preventDefault();
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* no pointer capture here */
    }
    drag.current = { x: e.clientX, y: e.clientY, start: shown };
  };
  const onBarMove = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d) return;
    setPlace(clampPlacement({ x: d.start.x + e.clientX - d.x, y: d.start.y + e.clientY - d.y }, view.w, view.h, scale));
  };
  const endDrag = () => {
    drag.current = null;
  };

  const label = `${title} ${device.toLowerCase()}`;
  return (
    <div
      className="device-window"
      style={
        {
          left: shown.x,
          top: shown.y,
          width: DESIGN_W * scale,
          height: DESIGN_H * scale,
          zIndex: 30 + z,
          "--ink": `var(--ink-${color + 1})`,
        } as CSSProperties
      }
      onPointerDownCapture={onRaise}
      onFocusCapture={onRaise}
    >
      <div
        ref={box}
        className="device-frame"
        role="dialog"
        aria-label={label}
        data-device-window=""
        style={{ width: DESIGN_W, height: DESIGN_H, transform: scale === 1 ? undefined : `scale(${scale})` }}
        onKeyDown={onKeyDown}
      >
        <div
          className="device-bar"
          data-hint={`The ${label}. Drag here to move the window; Escape or × closes it.`}
          onPointerDown={onBarDown}
          onPointerMove={onBarMove}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
          onLostPointerCapture={endDrag}
        >
          <span className="device-dot" aria-hidden="true" />
          <h2 className="device-title">{title}</h2>
          <span className="device-kind">{device}</span>
          {toolbar}
          <button type="button" className="device-close" aria-label={`Close the ${label}`} data-hint={`Closes the ${label} (Escape).`} onClick={onClose}>
            <svg viewBox="0 0 10 10" aria-hidden="true">
              <path d="M2 2L8 8M8 2L2 8" />
            </svg>
          </button>
        </div>
        <DeviceScale.Provider value={scale}>{children}</DeviceScale.Provider>
      </div>
    </div>
  );
}
