import { useEffect, useId, useRef, type KeyboardEvent, type ReactNode } from "react";

const FOCUSABLE =
  "button:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex='-1'])";

interface Props {
  title: string;
  /** Escape calls this. Leave it out for a dialog that must be answered. */
  onDismiss?: () => void;
  children: ReactNode;
}

/** A modal dialog: it takes focus (the element marked `data-autofocus`, else the first control),
 * keeps Tab inside, gives focus back when it closes, and Escape dismisses it if it can be. */
export function Modal({ title, onDismiss, children }: Props) {
  const box = useRef<HTMLDivElement>(null);
  const titleId = useId();

  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    const root = box.current;
    const first =
      root?.querySelector<HTMLElement>("[data-autofocus]") ?? root?.querySelector<HTMLElement>(FOCUSABLE);
    first?.focus();
    return () => {
      if (before?.isConnected) before.focus();
    };
  }, []);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape" && onDismiss) {
      e.stopPropagation();
      onDismiss();
    } else if (e.key === "Tab" && box.current) {
      const items = [...box.current.querySelectorAll<HTMLElement>(FOCUSABLE)];
      const firstItem = items[0];
      const lastItem = items[items.length - 1];
      if (!firstItem || !lastItem) return;
      if (e.shiftKey && document.activeElement === firstItem) {
        e.preventDefault();
        lastItem.focus();
      } else if (!e.shiftKey && document.activeElement === lastItem) {
        e.preventDefault();
        firstItem.focus();
      }
    }
  };

  return (
    <div className="modal-backdrop">
      <div
        ref={box}
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        onKeyDown={onKeyDown}
      >
        <h2 id={titleId}>{title}</h2>
        {children}
      </div>
    </div>
  );
}
