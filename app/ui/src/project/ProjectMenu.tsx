import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { shortcutLabel, type MenuAction } from "./shortcuts";


const ITEMS: { action: MenuAction; label: string; hint: string }[] = [
  { action: "new", label: "New", hint: "Starts an empty project. It asks first if this one has unsaved changes." },
  { action: "open", label: "Open…", hint: "Opens a project file. It asks first if this one has unsaved changes." },
  { action: "save", label: "Save", hint: "Saves the project to its file. A new project asks where." },
  { action: "saveAs", label: "Save as…", hint: "Saves the project to a new file." },
  { action: "export", label: "Export WAV…", hint: "Renders the pattern to an audio file." },
];

interface Props {
  name: string;
  platform: string;
  onAction: (action: MenuAction) => void;
}

/** The project name as a menu button: New, Open, Save, Save as and Export, with their shortcuts. */
export function ProjectMenu({ name, platform, onAction }: Props) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const items = useRef<(HTMLButtonElement | null)[]>([]);

  useEffect(() => {
    if (!open) return;
    items.current[0]?.focus();
    const onPointer = (e: PointerEvent) => {
      if (anchor.current && !anchor.current.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("pointerdown", onPointer);
    return () => window.removeEventListener("pointerdown", onPointer);
  }, [open]);

  const close = (refocus: boolean) => {
    setOpen(false);
    if (refocus) trigger.current?.focus();
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      close(true);
      return;
    }
    const at = items.current.findIndex((el) => el === document.activeElement);
    const last = ITEMS.length - 1;
    const go = (i: number) => {
      e.preventDefault();
      items.current[i]?.focus();
    };
    if (e.key === "ArrowDown") go(at >= last ? 0 : at + 1);
    else if (e.key === "ArrowUp") go(at <= 0 ? last : at - 1);
    else if (e.key === "Home") go(0);
    else if (e.key === "End") go(last);
    else if (e.key === "Tab") close(false);
  };

  return (
    <div ref={anchor} className="project-anchor">
      <button
        ref={trigger}
        type="button"
        className="project-name"
        aria-haspopup="menu"
        aria-expanded={open}
        data-hint="The project menu: new, open, save and export."
        onClick={() => setOpen(!open)}
      >
        <span className="project-name-text">{name}</span>
        <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false" style={{ flexShrink: 0 }}>
          <path d="m2 4 3 3 3-3" />
        </svg>
      </button>
      {open && (
        <div className="menu" role="menu" aria-label="Project" onKeyDown={onKeyDown}>
          {ITEMS.map((item, i) => (
            <button
              key={item.action}
              ref={(el) => {
                items.current[i] = el;
              }}
              type="button"
              role="menuitem"
              className="menu-item"
              data-hint={item.hint}
              onClick={() => {
                close(false);
                onAction(item.action);
              }}
            >
              <span>{item.label}</span>
              <kbd aria-hidden>{shortcutLabel(item.action, platform)}</kbd>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
