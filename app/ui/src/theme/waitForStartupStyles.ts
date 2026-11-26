/** WebKit can run the entry module while the linked CSS is still loading. The document
 * load event settles those requests, including failures that theme validation must report. */
export function waitForStartupStyles(document: Pick<Document, "readyState">, window: EventTarget): Promise<void> {
  if (document.readyState === "complete") return Promise.resolve();
  return new Promise((resolve) => {
    window.addEventListener("load", () => resolve(), { once: true });
  });
}
