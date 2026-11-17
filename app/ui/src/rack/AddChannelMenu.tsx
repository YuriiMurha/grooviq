import { useEffect, useRef, useState, type CSSProperties, type KeyboardEvent } from "react";
import { SYNTH_PRESETS } from "../api/notes";

/** Room the menu needs below its button before it opens upwards instead, in CSS pixels. */
const MENU_ROOM = 320;

interface Props {
  onSample: () => void;
  onSynth: (preset: string) => void;
}

/** "+ Add channel": a sample from a file, or the basic synth with one of its presets. */
export function AddChannelMenu({ onSample, onSynth }: Props) {
  const [at, setAt] = useState<CSSProperties | null>(null);
  const anchor = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const items = useRef<(HTMLButtonElement | null)[]>([]);
  const open = at !== null;

  useEffect(() => {
    if (!open) return;
    items.current[0]?.focus();
    const onPointer = (e: PointerEvent) => {
      if (anchor.current && !anchor.current.contains(e.target as Node)) setAt(null);
    };
    window.addEventListener("pointerdown", onPointer);
    return () => window.removeEventListener("pointerdown", onPointer);
  }, [open]);

  const toggle = () => {
    if (open) return setAt(null);
    const r = trigger.current?.getBoundingClientRect();
    if (!r) return;
    // Fixed, so a scrolling panel does not cut it off; upwards when there is no room below.
    setAt(
      window.innerHeight - r.bottom >= MENU_ROOM
        ? { left: r.left, top: r.bottom + 6 }
        : { left: r.left, bottom: window.innerHeight - r.top + 6 },
    );
  };
  const close = (refocus: boolean) => {
    setAt(null);
    if (refocus) trigger.current?.focus();
  };
  const choose = (fn: () => void) => {
    close(false);
    fn();
  };

  const count = 1 + SYNTH_PRESETS.length;
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      close(true);
      return;
    }
    const i = items.current.findIndex((el) => el === document.activeElement);
    const go = (n: number) => {
      e.preventDefault();
      items.current[(n + count) % count]?.focus();
    };
    if (e.key === "ArrowDown") go(i + 1);
    else if (e.key === "ArrowUp") go(i <= 0 ? count - 1 : i - 1);
    else if (e.key === "Home") go(0);
    else if (e.key === "End") go(count - 1);
    else if (e.key === "Tab") close(false);
  };

  return (
    <div ref={anchor} className="add-anchor">
      <button
        ref={trigger}
        type="button"
        className="add-channel"
        aria-haspopup="menu"
        aria-expanded={open}
        data-hint="Adds a channel: a sound file, or the basic synth with one of its presets."
        onClick={toggle}
      >
        + Add channel
      </button>
      {at && (
        <div className="menu add-menu" role="menu" aria-label="Add channel" style={at} onKeyDown={onKeyDown}>
          <button
            ref={(el) => {
              items.current[0] = el;
            }}
            type="button"
            role="menuitem"
            className="menu-item"
            data-hint="Adds a channel for each sound file you choose: WAV, AIFF, FLAC, MP3 or Ogg."
            onClick={() => choose(onSample)}
          >
            Sample…
          </button>
          <div role="group" aria-label="Basic synth" className="menu-group">
            <span className="menu-label" aria-hidden>
              Basic synth
            </span>
            {SYNTH_PRESETS.map((preset, i) => (
              <button
                key={preset}
                ref={(el) => {
                  items.current[i + 1] = el;
                }}
                type="button"
                role="menuitem"
                className="menu-item"
                data-hint={`Adds the basic synth with its ${preset} preset.`}
                onClick={() => choose(() => onSynth(preset))}
              >
                {preset}
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
