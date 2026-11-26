const THEME_ATTRIBUTE = "data-theme";
const listeners = new Set<() => void>();
let observer: MutationObserver | undefined;

/** Observe changes to the theme the document actually paints after each mutation batch. */
export function subscribeThemeChange(listener: () => void): () => void {
  const notify = () => listener();
  listeners.add(notify);
  if (!observer) {
    const root = document.documentElement;
    let theme = root.getAttribute(THEME_ATTRIBUTE);
    observer = new MutationObserver(() => {
      const next = root.getAttribute(THEME_ATTRIBUTE);
      if (next === theme) return;
      theme = next;
      listeners.forEach((notify) => notify());
    });
    observer.observe(root, { attributeFilter: [THEME_ATTRIBUTE] });
  }

  return () => {
    if (!listeners.delete(notify) || listeners.size > 0) return;
    observer?.disconnect();
    observer = undefined;
  };
}
