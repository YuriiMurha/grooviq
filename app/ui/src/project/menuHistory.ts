import { isTypingTarget } from "./shortcuts";

/** A menu can take native first responder without changing the DOM focus. Remember the
 * last DOM focus across that temporary blur; an actual focus change always replaces it. */
export function installMenuHistory(
  doc: Document,
  project: (redo: boolean) => void,
  blocked: () => boolean,
): { dispatch: (redo: boolean) => void; dispose: () => void } {
  let focused: Element | null = doc.activeElement;
  const focus = (e: FocusEvent) => { focused = e.target instanceof Element ? e.target : null; };
  const pointer = (e: PointerEvent) => {
    // A deliberate click on the page clears a stale field even if that page area is not focusable.
    if (!isTypingTarget(e.target)) focused = null;
  };
  doc.addEventListener("focusin", focus);
  doc.addEventListener("pointerdown", pointer);
  return {
    dispatch(redo) {
      const target = doc.activeElement && doc.activeElement !== doc.body
        ? doc.activeElement : focused;
      // Text history belongs to the browser, including when there is nothing left to undo.
      // Never fall through to project history when execCommand returns false.
      if (target?.isConnected && isTypingTarget(target)) {
        if (target instanceof HTMLElement) target.focus({ preventScroll: true });
        doc.execCommand(redo ? "redo" : "undo");
        return;
      }
      if (!blocked()) project(redo);
    },
    dispose() {
      doc.removeEventListener("focusin", focus);
      doc.removeEventListener("pointerdown", pointer);
    },
  };
}
