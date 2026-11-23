import { useCallback, useLayoutEffect, useRef, type RefObject } from "react";
import { placeFeelPanel } from "./feelPlacement";

type Options = {
  detailsRef: RefObject<HTMLDetailsElement | null>;
  panelRef: RefObject<HTMLDivElement | null>;
  cancelAmounts: () => void;
};

/** Presentation only: the existing details element and amount owner retain their state. */
export function useFeelPopover({ detailsRef, panelRef, cancelAmounts }: Options) {
  const synchronizeRef = useRef<() => void>(() => {});
  const synchronize = useCallback(() => synchronizeRef.current(), []);

  useLayoutEffect(() => {
    const details = detailsRef.current;
    const panel = panelRef.current;
    const summary = details?.querySelector("summary");
    if (!details || !panel || !summary) return;
    const native = typeof panel.showPopover === "function" && typeof panel.hidePopover === "function";
    if (native) panel.setAttribute("popover", "manual");
    else panel.removeAttribute("popover");
    panel.dataset.feelPresentation = native ? "native" : "fixed";
    let disposed = false;
    let ownHide = false;
    let observing = false;
    let scheduled = 0;

    const place = () => {
      scheduled = 0;
      if (disposed || !details.open || !details.isConnected || !panel.isConnected) return;
      const anchor = summary.getBoundingClientRect();
      const rect = panel.getBoundingClientRect();
      const hintTop = document.querySelector(".hintbar")?.getBoundingClientRect().top ?? window.innerHeight;
      const usable = { left: 8, top: 8, right: window.innerWidth - 8, bottom: Math.min(window.innerHeight, hintTop) - 8 };
      if (![anchor.left, anchor.top, anchor.right, anchor.bottom, rect.width, rect.height, usable.right, usable.bottom].every(Number.isFinite)
          || usable.right <= usable.left || usable.bottom <= usable.top || rect.width <= 0 || rect.height <= 0) return;
      // scrollHeight includes clipped content; never mistake a locally constrained height
      // for the full panel when deciding which side has room after a resize.
      const fullHeight = Math.max(rect.height, panel.scrollHeight + panel.offsetHeight - panel.clientHeight);
      const next = placeFeelPanel(anchor, { width: rect.width, height: fullHeight }, usable);
      const styles = { left: `${next.left}px`, top: `${next.top}px`, maxWidth: `${next.maxWidth}px`, maxHeight: `${next.maxHeight}px` };
      for (const [key, value] of Object.entries(styles)) {
        const property = key as keyof typeof styles;
        if (panel.style[property] !== value) panel.style[property] = value;
      }
    };
    const schedule = () => {
      if (!disposed && details.open && !scheduled) scheduled = requestAnimationFrame(place);
    };
    const size = new ResizeObserver(schedule);
    const connection = new MutationObserver(() => {
      if (!details.isConnected || !panel.isConnected) close();
    });
    const stopObserving = () => {
      if (scheduled) cancelAnimationFrame(scheduled);
      scheduled = 0;
      size.disconnect();
      connection.disconnect();
      window.removeEventListener("resize", schedule);
      window.removeEventListener("scroll", schedule, true);
      observing = false;
    };
    const hide = () => {
      stopObserving();
      if (native && panel.matches(":popover-open")) {
        ownHide = true;
        try { panel.hidePopover(); } finally { ownHide = false; }
      }
    };
    const close = () => {
      const wasOpen = details.open || (native && panel.matches(":popover-open"));
      details.open = false;
      if (wasOpen) cancelAmounts();
      hide();
    };
    const onNativeClosing = (event: Event) => {
      if (ownHide || (event as ToggleEvent).newState !== "closed") return;
      // beforetoggle is synchronous. Waiting for toggle lets a captured pointerup commit
      // after external hidePopover() but before the amount owner has been cancelled.
      details.open = false;
      cancelAmounts();
      stopObserving();
    };
    const focus = (event: FocusEvent) => {
      if (!details.open || !(event.target instanceof Element) || details.contains(event.target)) return;
      if (event.target.closest("[role='menu'], [role='dialog'], [role='alertdialog'], [data-device-window]")) close();
    };
    const sync = () => {
      if (disposed) return;
      if (!details.open || !details.isConnected || !panel.isConnected) { hide(); return; }
      if (native && !panel.matches(":popover-open")) panel.showPopover();
      place();
      if (!observing) {
        observing = true;
        size.observe(panel);
        size.observe(summary);
        const toolbar = details.closest(".roll-tools");
        const viewport = details.closest(".workspace-viewport");
        if (toolbar) size.observe(toolbar);
        if (viewport) size.observe(viewport);
        const hint = document.querySelector(".hintbar");
        if (hint) size.observe(hint);
        connection.observe(document.documentElement, { childList: true, subtree: true });
        window.addEventListener("resize", schedule);
        window.addEventListener("scroll", schedule, true);
      }
    };
    synchronizeRef.current = sync;
    panel.addEventListener("beforetoggle", onNativeClosing);
    window.addEventListener("focusin", focus);
    window.addEventListener("blur", close);
    sync();
    return () => {
      close();
      disposed = true;
      synchronizeRef.current = () => {};
      panel.removeEventListener("beforetoggle", onNativeClosing);
      window.removeEventListener("focusin", focus);
      window.removeEventListener("blur", close);
      panel.removeAttribute("popover");
      delete panel.dataset.feelPresentation;
    };
  }, [detailsRef, panelRef, cancelAmounts]);

  return { synchronize };
}
