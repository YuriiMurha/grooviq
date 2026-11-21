import { test, expect, position } from "./fixture";

test.beforeEach(async ({ page }) => {
  // Intercept only the fixture's socket assignment. Keep its Tauri/project semantics intact.
  await page.addInitScript(() => {
    const sockets: Socket[] = [];
    class Socket {
      binaryType = "arraybuffer";
      onopen: (() => void) | null = null;
      onclose: (() => void) | null = null;
      onmessage: ((e: MessageEvent) => void) | null = null;
      constructor() { sockets.push(this); setTimeout(() => this.onopen?.(), 0); }
      close() { this.onclose?.(); }
    }
    Object.defineProperty(window, "WebSocket", { configurable: true, get: () => Socket, set: () => {} });
    Object.assign(window, { meterSockets: sockets });
  });
});

test("master peaks wait, transport advances, reconnect and stale reads silence", async ({ app }) => {
  await app.evaluate(() => {
    let now = performance.now();
    const pending = new Map<number, FrameRequestCallback>();
    let id = 100000;
    Object.defineProperty(performance, "now", { configurable: true, value: () => now });
    window.requestAnimationFrame = callback => { pending.set(++id, callback); return id; };
    window.cancelAnimationFrame = handle => { pending.delete(handle); };
    Object.assign(window, {
      meterStep: (delta: number) => {
        now += delta;
        const callbacks = [...pending.values()]; pending.clear();
        callbacks.forEach(callback => callback(now));
      },
      meterSend: (counter: number, left: number, right: number, ticks: number) => {
        const data = new ArrayBuffer(40); const v = new DataView(data);
        v.setUint8(0, 2); v.setUint8(1, 1);
        v.setFloat32(4, left, true); v.setFloat32(8, right, true);
        v.setBigUint64(12, BigInt(counter), true); v.setUint32(28, 4800, true);
        v.setBigUint64(32, BigInt(ticks), true);
        const sockets = (window as unknown as { meterSockets: { onmessage: (e: MessageEvent) => void }[] }).meterSockets;
        sockets.at(-1)?.onmessage(new MessageEvent("message", { data }));
      },
      meterReady: () => pending.size,
    });
  });
  await expect.poll(() => app.evaluate("meterReady()")).toBeGreaterThan(0);
  const bars = app.getByRole("img", { name: "Master level meter" }).locator(".meter-bar");
  const widths = () => bars.evaluateAll(elements => elements.map(e => e.getBoundingClientRect().width));
  await app.evaluate("meterSend(1, 1.2, 0.01, 9600); meterStep(0)");
  expect(await widths()).toEqual([0, 0]);
  await expect(position(app)).not.toHaveText("1.1.01");
  await app.evaluate("meterStep(99.999)"); expect(await widths()).toEqual([0,0]);
  await app.evaluate("meterStep(0.001)");
  const shown = await widths(); expect(shown[0]).toBeGreaterThan(shown[1]); expect(shown[1]).toBeGreaterThan(0);
  expect(await bars.first().evaluate(e => getComputedStyle(e).backgroundColor)).not.toEqual(await bars.last().evaluate(e => getComputedStyle(e).backgroundColor));
  await app.evaluate("meterStep(1000)"); expect(await widths()).toEqual([0,0]);
  await app.evaluate("meterSend(2, 1.2, 1.2, 12000); meterStep(100)"); expect((await widths())[0]).toBeGreaterThan(0);
  await app.evaluate("meterSockets.at(-1).close(); meterStep(0)"); expect(await widths()).toEqual([0,0]);
  await expect.poll(() => app.evaluate("meterSockets.length")).toBe(2);
  await app.evaluate("meterSend(3, 1.2, 0, 14000); meterStep(99)"); expect(await widths()).toEqual([0,0]);
  await app.evaluate("meterStep(1)"); expect((await widths())[0]).toBeGreaterThan(0);
  // Closing the page owns teardown of all injected callbacks and fixture sockets.
});
