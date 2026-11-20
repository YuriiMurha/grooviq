import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { connectLive, type LiveFrame } from "./client";

/** A stand-in for the browser WebSocket: records every instance; tests drive its events. */
class FakeSocket {
  static all: FakeSocket[] = [];
  binaryType = "blob";
  closed = false;
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(public url: string) {
    FakeSocket.all.push(this);
  }
  close() {
    this.closed = true;
  }
  /** The server went away: what the browser does is fire `close`. */
  drop() {
    this.onclose?.();
  }
}

/** A wire-format v2 frame (40 bytes) with only the left peak set. */
function v2(peakL: number): ArrayBuffer {
  const buf = new ArrayBuffer(40);
  const v = new DataView(buf);
  v.setUint8(0, 2);
  v.setFloat32(4, peakL, true);
  return buf;
}

beforeEach(() => {
  FakeSocket.all = [];
  vi.useFakeTimers();
});
afterEach(() => vi.useRealTimers());

const endpoint = { port: 41234, token: "abc123" };
const WebSocketImpl = FakeSocket as unknown as typeof WebSocket;

test("puts the token in the URL and reconnects with backoff from 250 ms up to 2 s", () => {
  const stop = connectLive(endpoint, () => {}, { WebSocketImpl });
  expect(FakeSocket.all).toHaveLength(1);
  expect(FakeSocket.all[0].url).toBe("ws://127.0.0.1:41234/live?token=abc123");
  expect(FakeSocket.all[0].binaryType).toBe("arraybuffer");

  // Each drop waits twice as long as the one before, capped at 2 s.
  const waits = [250, 500, 1000, 2000, 2000];
  for (const [i, wait] of waits.entries()) {
    FakeSocket.all[i].drop();
    vi.advanceTimersByTime(wait - 1);
    expect(FakeSocket.all).toHaveLength(i + 1);
    vi.advanceTimersByTime(1);
    expect(FakeSocket.all).toHaveLength(i + 2);
  }

  // A connection that opens resets the backoff.
  const live = FakeSocket.all.at(-1)!;
  live.onopen?.();
  live.drop();
  vi.advanceTimersByTime(250);
  expect(FakeSocket.all).toHaveLength(waits.length + 2);

  // Disconnecting closes the socket and stops reconnecting.
  stop();
  expect(FakeSocket.all.at(-1)!.closed).toBe(true);
  FakeSocket.all.at(-1)!.drop();
  vi.advanceTimersByTime(10_000);
  expect(FakeSocket.all).toHaveLength(waits.length + 2);
});

test("percent-encodes a token that has URL-special characters", () => {
  connectLive({ port: 41234, token: "a+b/c=d&e" }, () => {}, { WebSocketImpl });
  expect(FakeSocket.all[0].url).toBe("ws://127.0.0.1:41234/live?token=a%2Bb%2Fc%3Dd%26e");
});

test("stopping during the reconnect wait cancels the pending reconnect", () => {
  const stop = connectLive(endpoint, () => {}, { WebSocketImpl });
  FakeSocket.all[0].drop(); // arms the 250 ms reconnect timer
  vi.advanceTimersByTime(100);
  stop();
  vi.advanceTimersByTime(10_000);
  expect(FakeSocket.all).toHaveLength(1);
});

test("ignores frames with an unknown version", () => {
  const frames: LiveFrame[] = [];
  connectLive(endpoint, (f) => frames.push(f), { WebSocketImpl, backoffMs: [100, 400] });
  const ws = FakeSocket.all[0];
  ws.onopen?.();
  const bad = new Uint8Array(40).fill(3).buffer;
  ws.onmessage?.({ data: bad });
  // A v1 frame from an older engine.
  const old = new Uint8Array(32);
  old[0] = 1;
  ws.onmessage?.({ data: old.buffer });
  ws.onmessage?.({ data: "not binary" });
  ws.onmessage?.({ data: v2(0.5) });
  expect(frames).toHaveLength(1);
  expect(frames[0].peak[0]).toBe(0.5);
});

test("honours a custom backoff range", () => {
  connectLive(endpoint, () => {}, { WebSocketImpl, backoffMs: [100, 150] });
  FakeSocket.all[0].drop();
  vi.advanceTimersByTime(100);
  expect(FakeSocket.all).toHaveLength(2);
  FakeSocket.all[1].drop();
  vi.advanceTimersByTime(149);
  expect(FakeSocket.all).toHaveLength(2);
  vi.advanceTimersByTime(1);
  expect(FakeSocket.all).toHaveLength(3);
});

test("connection resets and ignores old or stopped socket deliveries", () => {
  const frames: LiveFrame[] = [];
  const lifecycle = vi.fn();
  const stop = connectLive(endpoint, f => frames.push(f), { WebSocketImpl, onConnectionChange: lifecycle });
  const old = FakeSocket.all[0];
  old.onopen?.(); old.drop();
  old.onmessage?.({ data: v2(1) });
  vi.advanceTimersByTime(250);
  old.onopen?.();
  const fresh = FakeSocket.all[1];
  fresh.onopen?.(); fresh.onmessage?.({ data: v2(0.2) });
  expect(frames).toHaveLength(1);
  stop(); fresh.onmessage?.({ data: v2(1) });
  expect(frames).toHaveLength(1);
  expect(lifecycle).toHaveBeenCalledTimes(4);
});
