import { expect, test } from "vitest";
import { MeterHistory } from "./history";
const context = { id: "a", rate: 48000 };
const obs = (frames = 1n, latency = 4800, peak: [number, number] = [1.2, 0.5]) => ({ frames, outputLatencyFrames: latency, peak });
test("reported delay drains without another push and retains stereo clipping", () => {
  const h = new MeterHistory();
  h.push(obs(), context, 0);
  expect(h.read(99.999)).toEqual([0, 0]);
  expect(h.read(100)).toEqual([1.2, 0.5]);
});
test("actual rate conversion and unknown metadata", () => {
  for (const rate of [96000, undefined, 0, NaN, Infinity, -1]) {
    const h = new MeterHistory();
    h.push(obs(1n, 9600), { id: "a", rate }, 0);
    expect(h.read(rate === 96000 ? 99.999 : 0)).toEqual(rate === 96000 ? [0, 0] : [1.2, 0.5]);
    expect(h.read(100)).toEqual([1.2, 0.5]);
  }
});
test("FIFO deadlines shift the original step/release without filtering", () => {
  const h = new MeterHistory();
  h.push(obs(1n, 9600), context, 0);
  h.push(obs(2n, 0, [0.3, 0.1]), context, 10);
  expect(h.read(199)).toEqual([0,0]);
  expect(h.read(200)).toEqual([0.3,0.1]);
  h.push(obs(3n, 9600, [0.2, 0]), context, 210);
  expect(h.read(409)).toEqual([0.3,0.1]);
  expect(h.read(410)).toEqual([0.2,0]);
});
test("FIFO deadlines survive eviction of the original delayed observation", () => {
  const h = new MeterHistory();
  h.push(obs(1n, 96000), context, 0);
  for (let i = 1; i <= 512; i++) h.push(obs(BigInt(i + 1), 0), context, i);
  expect(h.pendingCount).toBe(512);
  expect(h.read(513)).toEqual([0, 0]);
});
test("cap, fixed resource bound, and stale expiry without push", () => {
  const h = new MeterHistory();
  for (let i = 0; i < 800; i++) h.push(obs(BigInt(i), 0xffffffff), context, i);
  expect(h.pendingCount).toBe(512);
  expect(h.read(1999)).toEqual([0,0]);
  expect(h.read(2800)).toEqual([1.2,0.5]);
  expect(h.read(3799)).toEqual([0,0]);
  expect(h.pendingCount).toBe(0);
});
test("duplicate counters never refresh stalled age", () => {
  const h = new MeterHistory();
  h.push(obs(2n ** 60n), context, 0);
  h.push(obs(2n ** 60n, 0, [2,2]), context, 1000);
  expect(h.read(1100)).toEqual([0,0]);
});
test("context, counter, receipt gap, clock and explicit epoch resets", () => {
  for (const reset of ["device", "rate", "counter", "gap", "clock", "explicit"]) {
    const h = new MeterHistory();
    h.push(obs(10n), context, 100);
    expect(h.read(200)).toEqual([1.2,0.5]);
    if (reset === "explicit") h.reset();
    const ctx = reset === "device" ? { ...context, id: "b" } : reset === "rate" ? { ...context, rate: 96000 } : context;
    const time = reset === "gap" ? 2000 : reset === "clock" ? 0 : 201;
    h.push(obs(reset === "counter" ? 1n : 11n), ctx, time);
    expect(h.read(time)).toEqual([0,0]);
  }
});
test("sanitizes peaks, copies scalars, handles missing receipt and invalid clocks", () => {
  const h = new MeterHistory();
  const o = obs(1n, NaN, [NaN,-1]);
  h.push(o, context, 0); expect(h.read(0)).toEqual([0,0]);
  o.frames = 2n; o.peak = [2,0.2]; h.push(o, context, 1); o.peak[0] = 0;
  expect(h.read(1)).toEqual([2,0.2]);
  expect(h.read(NaN)).toEqual([0,0]);
});

test("invalid or absent latency adds zero, and adjacent large counters remain distinct", () => {
  for (const latency of [undefined, -1, Infinity, NaN, 0]) {
    const h = new MeterHistory();
    h.push({ frames: 2n ** 60n, peak: [0.2, 0.3], outputLatencyFrames: latency }, context, 0);
    expect(h.read(0)).toEqual([0.2, 0.3]);
    h.push(obs(2n ** 60n + 1n, 0, [0.4, 0.5]), context, 1);
    expect(h.read(1)).toEqual([0.4, 0.5]);
  }
});
