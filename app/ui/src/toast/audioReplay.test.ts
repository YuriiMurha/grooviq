import { describe, expect, it, vi } from "vitest";
import { AudioReplay } from "./audioReplay";
import type { AudioEvent, LogEntry } from "../audio";

const entry = (id: number, level: LogEntry["level"] = "warn"): LogEntry => ({ id, level, atMs: 7, message: "episode" });
const event = (id: number): AudioEvent => ({ id, kind: "no-device", message: "episode" });
const deferred = <T>() => { let resolve!: (value: T) => void; let reject!: (error: unknown) => void; const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };
const flush = async () => { await Promise.resolve(); await Promise.resolve(); };

describe("launch replay", () => {
  it("waits for listener readiness, merges concurrent delivery, preserves distinct episodes and live info", async () => {
    const ready = deferred<() => void>();
    const history = deferred<LogEntry[]>();
    let receive!: (e: AudioEvent) => void;
    const snapshot = vi.fn(() => history.promise);
    const show = vi.fn();
    new AudioReplay().connect((h) => { receive = h; return ready.promise; }, snapshot, show, () => {});
    expect(snapshot).not.toHaveBeenCalled();
    ready.resolve(() => {});
    await flush();
    expect(snapshot).toHaveBeenCalledOnce();
    receive(event(3));
    history.resolve([entry(1), entry(2, "info"), entry(3), entry(4, "error")]);
    await flush();
    receive(event(5));
    receive({ ...event(6), kind: "opened" });
    expect(show.mock.calls).toEqual([["episode", true], ["episode", true], ["episode", true], ["episode", true], ["episode", false]]);
  });
  it("cancels late readiness and late snapshots without claiming IDs needed on remount", async () => {
    const replay = new AudioReplay();
    const ready = deferred<() => void>();
    const stop = vi.fn(); const snapshot = vi.fn(async () => [entry(1)]); const show = vi.fn();
    const cancel = replay.connect(() => ready.promise, snapshot, show, () => {});
    cancel(); ready.resolve(stop); await flush();
    expect(stop).toHaveBeenCalledOnce(); expect(snapshot).not.toHaveBeenCalled();
    const history = deferred<LogEntry[]>(); let receive!: (e: AudioEvent) => void;
    const cancel2 = replay.connect(async (h) => { receive = h; return stop; }, () => history.promise, show, () => {});
    await flush(); cancel2(); receive(event(1)); history.resolve([entry(1)]); await flush();
    expect(show).not.toHaveBeenCalled();
    replay.connect(async () => stop, snapshot, show, () => {}); await flush();
    expect(show).toHaveBeenCalledOnce();
    replay.connect(async () => stop, snapshot, show, () => {}); await flush();
    expect(show).toHaveBeenCalledOnce();
  });
  it("keeps live delivery after query failure and bounds retained identities", async () => {
    const replay = new AudioReplay(); let receive!: (e: AudioEvent) => void;
    const show = vi.fn();
    replay.connect(async (h) => { receive = h; return () => {}; }, async () => { throw Error("offline"); }, show, () => {});
    await flush();
    // Delivery alone cannot distinguish bounded retention from an ever-growing Set.
    const identities = (replay as unknown as { seen: Set<number> }).seen;
    for (let id = 1; id <= 10_000; id++) {
      receive(event(id));
      expect(identities.size).toBeLessThanOrEqual(200);
    }
    receive(event(1)); receive(event(10_000)); receive(event(10_001));
    expect(show).toHaveBeenCalledTimes(10_001);
    expect(identities.size).toBeLessThanOrEqual(200);
  });
});
