import { test, expect } from "./fixture";

test.use({ seed: { holdAudioLog: true, audioLog: [
  { id: 1, atMs: 7, level: "warn", message: "qa-startup-a" },
  { id: 2, atMs: 7, level: "info", message: "qa-startup-info" },
  { id: 3, atMs: 7, level: "error", message: "qa-overlap" },
] } });

test("startup snapshot and concurrent live events each toast once", async ({ app, mock }) => {
  const messages = app.getByRole("status", { name: "Messages" });
  await expect.poll(async () => (await mock.calls("audio_log")).length).toBeGreaterThan(0);
  await mock.eval(`m => m.emit("audio-event", { id: 3, kind: "no-device", message: "qa-overlap" })`);
  await expect(messages.getByText("qa-overlap", { exact: true })).toHaveCount(1);
  await mock.eval(`m => m.releaseCommand("audio_log")`);
  await expect(messages.getByText("qa-startup-a", { exact: true })).toHaveCount(1);
  await expect(messages.getByText("qa-startup-info", { exact: true })).toHaveCount(0);
  await expect(messages.getByText("qa-overlap", { exact: true })).toHaveCount(1);
  await mock.eval(`m => { for (const id of [4, 5]) m.emit("audio-event", { id, kind: "no-device", message: "qa-repeat" }); }`);
  await expect(messages.getByText("qa-repeat", { exact: true })).toHaveCount(2);
  await messages.getByRole("button", { name: "Dismiss" }).first().click();
  await mock.eval(`m => m.emit("audio-event", { id: 3, kind: "no-device", message: "qa-overlap" })`);
  await expect(messages.getByText("qa-overlap", { exact: true })).toHaveCount(0);
  await app.locator("button.project-name").click();
  await app.getByRole("menuitem", { name: /^New/ }).click();
  await expect.poll(async () => (await mock.calls("project_new")).length).toBe(1);
  await expect(messages.getByText("qa-overlap", { exact: true })).toHaveCount(0);
  await expect(messages.getByText("qa-startup-a", { exact: true })).toHaveCount(1);
});

test("failed startup history leaves live warnings connected", async ({ app, mock }) => {
  await expect.poll(async () => (await mock.calls("audio_log")).length).toBeGreaterThan(0);
  await mock.eval(`m => { m.refuseNext("audio_log", "qa-query-failure"); m.releaseCommand("audio_log"); }`);
  await mock.eval(`m => m.emit("audio-event", { id: 9, kind: "no-device", message: "qa-after-failure" })`);
  await expect(app.getByRole("status", { name: "Messages" }).getByText("qa-after-failure", { exact: true })).toHaveCount(1);
});


import { position } from "./fixture";

test.describe("startup replay with delayed master observations", () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      const sockets: Socket[] = [];
      class Socket {
        binaryType = "arraybuffer";
        onopen: (() => void) | null = null;
        onclose: (() => void) | null = null;
        onmessage: ((event: MessageEvent) => void) | null = null;
        constructor() { sockets.push(this); setTimeout(() => this.onopen?.(), 0); }
        close() { this.onclose?.(); }
      }
      Object.defineProperty(window, "WebSocket", { configurable: true, get: () => Socket, set: () => {} });
      Object.assign(window, { startupMeterSockets: sockets });
    });
  });

  test.beforeEach(async ({ app, mock }) => {
    // The original seed deliberately holds only history, after the live listener registers.
    await expect.poll(async () => (await mock.calls("audio_log")).length).toBeGreaterThan(0);
    await expect.poll(async () => (await mock.calls("audio_outputs")).length).toBeGreaterThan(0);
    await app.evaluate(() => {
      let now = performance.now();
      let nextId = 100000;
      const pending = new Map<number, FrameRequestCallback>();
      Object.defineProperty(performance, "now", { configurable: true, value: () => now });
      window.requestAnimationFrame = callback => { pending.set(++nextId, callback); return nextId; };
      window.cancelAnimationFrame = handle => { pending.delete(handle); };
      Object.assign(window, {
        startupMeterReady: () => pending.size,
        startupMeterStep: (delta: number) => {
          now += delta;
          const callbacks = [...pending.values()]; pending.clear();
          callbacks.forEach(callback => callback(now));
        },
        startupMeterSend: (counter: number, left: number, right: number, ticks: number) => {
          const buffer = new ArrayBuffer(40);
          const view = new DataView(buffer);
          view.setUint8(0, 2); view.setUint8(1, 1);
          view.setFloat32(4, left, true); view.setFloat32(8, right, true);
          view.setBigUint64(12, BigInt(counter), true);
          view.setUint32(28, 4800, true);
          view.setBigUint64(32, BigInt(ticks), true);
          const sockets = (window as unknown as {
            startupMeterSockets: { onmessage: ((event: MessageEvent) => void) | null }[];
          }).startupMeterSockets;
          const socket = sockets.at(-1);
          if (!socket?.onmessage) throw new Error("Startup meter socket is not connected");
          socket.onmessage(new MessageEvent("message", { data: buffer }));
        },
      });
    });
    // Let the already scheduled real frame hand over to this case's virtual scheduler.
    await expect.poll(() => app.evaluate("startupMeterReady()")).toBeGreaterThan(0);
  });

  test("held startup history preserves pending stereo peaks and immediate transport", async ({ app, mock }) => {
    const messages = app.getByRole("status", { name: "Messages" });
    const bars = app.getByRole("img", { name: "Master level meter" }).locator(".meter-bar");
    const widths = () => bars.evaluateAll(elements => elements.map(element => element.getBoundingClientRect().width));
    await mock.eval(`m => m.emit("audio-event", { id: 3, kind: "no-device", message: "qa-overlap" })`);
    await expect(messages.getByText("qa-overlap", { exact: true })).toHaveCount(1);
    await app.evaluate("startupMeterSend(1, 1.2, 0.01, 9600); startupMeterStep(0)");
    expect(await widths()).toEqual([0, 0]);
    await expect(position(app)).not.toHaveText("1.1.01");
    await mock.eval(`m => m.releaseCommand("audio_log")`);
    await expect(messages.getByText("qa-startup-a", { exact: true })).toHaveCount(1);
    await expect(messages.getByText("qa-startup-info", { exact: true })).toHaveCount(0);
    await expect(messages.getByText("qa-overlap", { exact: true })).toHaveCount(1);
    await app.evaluate("startupMeterStep(99.999)");
    expect(await widths()).toEqual([0, 0]);
    await app.evaluate("startupMeterStep(0.001)");
    const shown = await widths();
    expect(shown[0]).toBeGreaterThan(shown[1]);
    expect(shown[1]).toBeGreaterThan(0);
    expect(await bars.first().evaluate(element => getComputedStyle(element).backgroundColor))
      .not.toEqual(await bars.last().evaluate(element => getComputedStyle(element).backgroundColor));
    await mock.eval(`m => { for (const id of [4, 5]) m.emit("audio-event", { id, kind: "no-device", message: "qa-distinct" }); }`);
    await expect(messages.getByText("qa-distinct", { exact: true })).toHaveCount(2);
  });

  test("every live event clears pending peaks even when its toast identity is duplicated", async ({ app, mock }) => {
    const messages = app.getByRole("status", { name: "Messages" });
    const bars = app.getByRole("img", { name: "Master level meter" }).locator(".meter-bar");
    const widths = () => bars.evaluateAll(elements => elements.map(element => element.getBoundingClientRect().width));
    await mock.eval(`m => m.releaseCommand("audio_log")`);
    await expect(messages.getByText("qa-startup-a", { exact: true })).toHaveCount(1);
    await app.evaluate("startupMeterSend(10, 1.2, 0.01, 9600); startupMeterStep(100)");
    expect((await widths())[0]).toBeGreaterThan(0);
    const beforeFirst = (await mock.calls("audio_outputs")).length;
    await mock.eval(`m => m.emit("audio-event", { id: 70, kind: "no-device", message: "qa-meter-epoch" })`);
    await expect.poll(async () => (await mock.calls("audio_outputs")).length).toBeGreaterThan(beforeFirst);
    await expect(messages.getByText("qa-meter-epoch", { exact: true })).toHaveCount(1);
    await app.evaluate("startupMeterStep(0)");
    expect(await widths()).toEqual([0, 0]);
    await app.evaluate("startupMeterSend(11, 1.2, 0.01, 10560); startupMeterStep(100)");
    expect((await widths())[0]).toBeGreaterThan(0);
    await app.evaluate("startupMeterSend(12, 0.4, 0.1, 11520); startupMeterStep(50)");
    const beforeDuplicate = (await mock.calls("audio_outputs")).length;
    await mock.eval(`m => m.emit("audio-event", { id: 70, kind: "no-device", message: "qa-meter-epoch" })`);
    await expect.poll(async () => (await mock.calls("audio_outputs")).length).toBeGreaterThan(beforeDuplicate);
    await expect(messages.getByText("qa-meter-epoch", { exact: true })).toHaveCount(1);
    await app.evaluate("startupMeterStep(0)");
    expect(await widths()).toEqual([0, 0]);
    await app.evaluate("startupMeterStep(50)");
    expect(await widths()).toEqual([0, 0]);
    // A genuinely new frame warms up for its own full delay after that invalidation.
    await app.evaluate("startupMeterSend(13, 1.2, 0.01, 12480); startupMeterStep(99.999)");
    expect(await widths()).toEqual([0, 0]);
    await app.evaluate("startupMeterStep(0.001)");
    const shown = await widths();
    expect(shown[0]).toBeGreaterThan(shown[1]);
    expect(shown[1]).toBeGreaterThan(0);
  });
});
