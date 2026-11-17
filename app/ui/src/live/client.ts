import { decodeLive, type LiveFrame } from "./decode";

export type { LiveFrame };

/** Where the live stream listens, as the `live_endpoint` command returns it. */
export interface LiveEndpoint {
  port: number;
  token: string;
}

export interface ConnectOptions {
  /** The WebSocket class to use (tests pass a fake). */
  WebSocketImpl?: typeof WebSocket;
  /** First and longest wait before reconnecting, in ms; each failure doubles the wait. */
  backoffMs?: [number, number];
}

/** Streams live frames from the engine to `onFrame`, reconnecting after a drop (250 ms, then
 * doubling up to 2 s; a connection that opens resets it). Frames of an unknown version are
 * skipped. Returns a function that disconnects for good. */
export function connectLive(
  endpoint: LiveEndpoint,
  onFrame: (frame: LiveFrame) => void,
  opts: ConnectOptions = {},
): () => void {
  const Impl = opts.WebSocketImpl ?? WebSocket;
  const [first, max] = opts.backoffMs ?? [250, 2000];
  const url = `ws://127.0.0.1:${endpoint.port}/live?token=${encodeURIComponent(endpoint.token)}`;
  let wait = first;
  let socket: WebSocket | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let stopped = false;

  const open = () => {
    timer = null;
    const ws = new Impl(url);
    socket = ws;
    ws.binaryType = "arraybuffer";
    ws.onopen = () => {
      wait = first;
    };
    ws.onmessage = (e: MessageEvent) => {
      if (!(e.data instanceof ArrayBuffer)) return;
      const frame = decodeLive(e.data);
      if (!frame) return;
      frame.receivedAt = performance.now();
      onFrame(frame);
    };
    ws.onclose = () => {
      if (stopped || socket !== ws) return;
      socket = null;
      timer = setTimeout(open, wait);
      wait = Math.min(wait * 2, max);
    };
  };

  open();
  return () => {
    stopped = true;
    if (timer !== null) clearTimeout(timer);
    socket?.close();
    socket = null;
  };
}
