import { useEffect, useState } from "react";

/** What the bar says when nothing under the pointer or focus has a hint. */
export const HINT_IDLE = "Point at anything to see what it does.";

const HINTED = "[data-hint]";

function hintOf(el: Element | null): string | null {
  return el?.getAttribute("data-hint") ?? null;
}

function closestHinted(target: EventTarget | null): Element | null {
  return target instanceof Element ? target.closest(HINTED) : null;
}

/** The line at the bottom: the `data-hint` of the closest element under the pointer, else of
 * the focused one. Elements may rewrite their hint while the pointer is over them, so it is
 * read again on every pointer move and whenever any `data-hint` changes. */
export function HintBar() {
  const [text, setText] = useState(HINT_IDLE);

  useEffect(() => {
    let hovered: Element | null = null;
    let focused: Element | null = null;

    const show = () => {
      // A removed element has no hint any more.
      if (hovered && !hovered.isConnected) hovered = null;
      if (focused && !focused.isConnected) focused = null;
      setText(hintOf(hovered) ?? hintOf(focused) ?? HINT_IDLE);
    };
    const track = (e: Event) => {
      hovered = closestHinted(e.target);
      show();
    };
    const leave = (e: PointerEvent) => {
      // Out of the window entirely: there is no pointer to follow.
      if (e.relatedTarget === null) {
        hovered = null;
        show();
      }
    };
    const focusIn = (e: FocusEvent) => {
      focused = closestHinted(e.target);
      show();
    };
    const focusOut = () => {
      focused = null;
      show();
    };
    const observer = new MutationObserver(show);

    document.addEventListener("pointerover", track);
    document.addEventListener("pointermove", track);
    document.addEventListener("pointerout", leave);
    document.addEventListener("focusin", focusIn);
    document.addEventListener("focusout", focusOut);
    observer.observe(document.body, {
      subtree: true,
      attributes: true,
      attributeFilter: ["data-hint"],
    });
    // Something may already hold focus.
    focused = closestHinted(document.activeElement);
    show();

    return () => {
      document.removeEventListener("pointerover", track);
      document.removeEventListener("pointermove", track);
      document.removeEventListener("pointerout", leave);
      document.removeEventListener("focusin", focusIn);
      document.removeEventListener("focusout", focusOut);
      observer.disconnect();
    };
  }, []);

  return (
    <footer className="hintbar" aria-label="Hint">
      {text}
    </footer>
  );
}
