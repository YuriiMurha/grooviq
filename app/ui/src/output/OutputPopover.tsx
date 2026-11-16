import { useEffect, useRef } from "react";
import { SYSTEM_DEFAULT, type LogEntry, type Output } from "../audio";
import { AudioLog } from "../log/AudioLog";
import { focusInto, returnFocus } from "./focus";
import { kindIcon, kindWarns } from "./kind";

interface Props {
  outputs: Output[];
  /** The id chosen: `SYSTEM_DEFAULT` or a device's. */
  chosen: string;
  guard: boolean;
  log: LogEntry[];
  onSelect: (id: string) => void;
  onGuard: (on: boolean) => void;
  onClose: () => void;
}

/** The output menu: every device, the speaker guard and the audio log. Escape or a click
 * outside closes it. Focus moves into it on open and back to what opened it on close. */
export function OutputPopover({ outputs, chosen, guard, log, onSelect, onGuard, onClose }: Props) {
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = box.current;
    // The chip, usually; WebKit does not focus a button on click, so it may be the page body.
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    if (el) focusInto(el);
    return () => {
      const active = document.activeElement;
      // On unmount the dialog is already gone, so focus has fallen back to the page body unless
      // something outside took it.
      const movedElsewhere = active !== null && active !== document.body && !el?.contains(active);
      const target = opener === document.body ? document.querySelector<HTMLElement>(".chip") : opener;
      returnFocus(target, movedElsewhere);
    };
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    const onPointer = (e: PointerEvent) => {
      const target = e.target as Node;
      // The chip toggles the popover itself.
      if (box.current && !box.current.contains(target) && !(target as Element).closest?.(".chip")) {
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("pointerdown", onPointer);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("pointerdown", onPointer);
    };
  }, [onClose]);

  const defaultName = outputs.find((o) => o.isDefault)?.name;
  return (
    <div ref={box} className="popover" role="dialog" aria-label="Audio output" tabIndex={-1}>
      <fieldset className="devices">
        <legend>Output</legend>
        <label className="device">
          <input
            type="radio"
            name="output"
            checked={chosen === SYSTEM_DEFAULT}
            onChange={() => onSelect(SYSTEM_DEFAULT)}
          />
          <span className="device-icon" aria-hidden>
            ⚙️
          </span>
          <span className="device-name">
            System default
            {defaultName && <span className="device-note"> ({defaultName})</span>}
          </span>
        </label>
        {outputs.map((o) => (
          <label key={o.id} className={`device${kindWarns(o.kind) ? " warn" : ""}`}>
            <input type="radio" name="output" checked={chosen === o.id} onChange={() => onSelect(o.id)} />
            <span className="device-icon" aria-hidden>
              {kindIcon(o.kind)}
            </span>
            <span className="device-name">
              {o.name}
              {o.current && <span className="device-note"> · playing</span>}
            </span>
          </label>
        ))}
      </fieldset>
      <label className="guard">
        <input type="checkbox" checked={guard} onChange={(e) => onGuard(e.target.checked)} />
        Stop playback if sound would move to speakers
      </label>
      <AudioLog entries={log} />
    </div>
  );
}
