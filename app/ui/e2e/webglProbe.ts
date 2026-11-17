/** Init script (runs in the page before the app): every WebGL context is created with
 * `preserveDrawingBuffer: true`, so a screenshot reads the canvas's last drawn frame.
 *
 * Why: the grid draws only when something changed. Without a preserved buffer, Linux WebKit
 * (Playwright's WebKitGTK) shows a cleared or stale buffer for a canvas that went idle after its
 * last draw: about half the page loads screenshot a blank grid, and a later draw can be missed
 * the same way. macOS WebKit and Chromium present the last frame either way. The attribute only
 * changes how long the buffer lives, never what is drawn, so the pixel checks still read the
 * renderer's output. Tests that read canvas pixels get this through the `app` fixture and
 * `pixelsAt`; nothing in the app changes. */
export function keepWebglFrames() {
  const original = HTMLCanvasElement.prototype.getContext;
  HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, kind: string, options?: unknown) {
    const webgl = kind === "webgl" || kind === "webgl2" || kind === "experimental-webgl";
    return original.call(this, kind, webgl ? { ...(options as object), preserveDrawingBuffer: true } : options);
  } as typeof original;
}
