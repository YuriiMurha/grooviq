import { describe, expect, it } from "vitest";
import type { MixerMeterCapture, MixerMeterRequest } from "../api/mixerMeter";
import type { LiveFrame } from "../live/decode";
import { createMixerMeterDriver, meterWindowNodes } from "./mixerMeterDriver";
import { createMixerMeters } from "./mixerMeters";

const scope = (generation: number, revision: number) => {
  const ids = new Map([[String(generation) + ":master", 0], [String(generation) + ":track", 7], [String(generation) + ":other", 9]]);
  return { stamp: { generation, revision }, masterId: `${generation}:master`, resolveTrack: (id: string) => ids.get(id) };
};

function setup() {
  let time = 100;
  let latest: LiveFrame | null = null;
  let liveReads = 0;
  const requests: MixerMeterRequest[] = [];
  const replies: ((capture: MixerMeterCapture | null) => void)[] = [];
  const cache = createMixerMeters((request) => new Promise((resolve) => {
    requests.push(request);
    replies.push(resolve);
  }), () => time);
  const driver = createMixerMeterDriver(cache, () => time);
  const out = new Float32Array(2);
  return {
    driver, requests,
    advance: (ms: number) => { time += ms; },
    publish(frames: bigint, peak: [number, number], receivedAt = time) {
      latest = { frames, receivedAt, peak, playing: false, tone: false, faults: 0, guardHits: 0, outputLatencyFrames: 0, positionTicks: 0n };
    },
    open() {
      driver.start(() => { liveReads++; return latest; });
    },
    liveReads: () => liveReads,
    read: (id: string) => driver.source.read(id, out, time) ? [...out] : null,
    answer(index: number, frames: bigint, clipFrame: bigint | null, peak: [number, number] = [0.25, 0.75]) {
      const input = requests[index];
      replies[index]({ ...input, mixRevision: 1n, frames, tracks: input.tracks.map((track) => ({ track, peak, clipFrame })) });
    },
    cancel(index: number) { replies[index](null); },
  };
}

describe("mounted mixer meter driver", () => {
  it("keeps pinned and visible nodes ahead of overscan under the native cap", () => {
    const tracks = Array.from({ length: 90 }, (_, index) => ({ id: `node-${index}` }));
    const indices = [...tracks.slice(0, 80).map((_, index) => index), 88];
    const ids = meterWindowNodes(tracks, { start: 2, end: 79 }, [88, 2, -1], indices);
    expect(ids).toHaveLength(64);
    expect(ids.slice(0, 4)).toEqual(["node-88", "node-2", "node-3", "node-4"]);
    expect(ids).not.toContain("node-0");
    expect(meterWindowNodes(tracks, { start: 2, end: 3 }, [88], [0, 1, 2, 3, 88]))
      .toEqual(["node-88", "node-2", "node-0", "node-1", "node-3"]);
  });

  it("drops delayed selections and replacements without opening another in-flight slot", async () => {
    const { driver, requests, open, read, answer } = setup();
    open();
    driver.setScope(scope(1, 1));
    driver.source.setVisible(["1:track"]);
    const old = driver.tick();
    driver.source.setVisible(["1:other"]);
    driver.tick();
    expect(requests).toEqual([{ generation: 1, revision: 1, tracks: [7] }]);
    answer(0, 100n, 100n);
    await old;
    expect(read("1:track")).toBeNull();
    expect(read("1:other")).toBeNull();
    const retiring = driver.tick();
    driver.setScope(null);
    driver.source.setVisible(["2:track"]);
    driver.setScope(scope(2, 0));
    driver.tick();
    expect(requests).toHaveLength(2);
    answer(1, 200n, 200n);
    await retiring;
    expect(read("2:track")).toBeNull();
    expect(driver.source.isClipped("2:track")).toBe(false);

    const fresh = driver.tick();
    expect(requests[2]).toEqual({ generation: 2, revision: 0, tracks: [7] });
    answer(2, 210n, null);
    await fresh;
    expect(read("2:track")).toEqual([0.25, 0.75]);
    expect(read("1:track")).toBeNull();
  });

  it("expires stereo peaks and retains a clear through the same clip frame", async () => {
    const { driver, requests, open, read, answer, advance } = setup();
    open();
    driver.setScope(scope(1, 1));
    driver.source.setVisible(["1:track"]);
    let notifications = 0;
    const off = driver.source.subscribeClips(() => { notifications++; });
    const first = driver.tick();
    answer(0, 80n, 70n);
    await first;
    expect(read("1:track")).toEqual([0.25, 0.75]);
    expect(driver.source.isClipped("1:track")).toBe(true);
    driver.clearClip("2:track");
    expect(driver.source.isClipped("1:track")).toBe(true);
    driver.clearClip("1:track");
    const sameClip = driver.tick();
    answer(1, 90n, 70n, [0.5, 0.125]);
    await sameClip;
    expect(read("1:track")).toEqual([0.5, 0.125]);
    expect(driver.source.isClipped("1:track")).toBe(false);
    expect(notifications).toBe(2);
    const stalled = driver.tick();
    advance(250);
    expect(read("1:track")).toBeNull();
    driver.tick();
    expect(requests).toHaveLength(3);
    answer(2, 120n, 115n);
    await stalled;
    expect(driver.source.isClipped("1:track")).toBe(true);
    expect(notifications).toBe(3);
    off();
  });

  it("fences retained Master frames and only latches a new observed crossing after Clear", () => {
    const { driver, requests, open, read, publish, advance } = setup();
    publish(100n, [1.5, 0.25]);
    open();
    driver.setScope(scope(1, 1));
    driver.tick();
    expect(read("1:master")).toBeNull();
    advance(25);
    publish(110n, [1.25, 0.5]);
    driver.tick();
    expect(read("1:master")).toEqual([1.25, 0.5]);
    expect(driver.source.isClipped("1:master")).toBe(true);
    driver.clearClip("1:master");
    driver.tick();
    advance(25);
    publish(120n, [1.25, 0.5]);
    driver.tick();
    expect(driver.source.isClipped("1:master")).toBe(false);
    advance(25);
    publish(130n, [0.125, 0.5]);
    driver.tick();
    advance(25);
    publish(125n, [1.5, 0.25]);
    driver.tick();
    expect(driver.source.isClipped("1:master")).toBe(false);
    advance(25);
    publish(140n, [0.5, 1.5]);
    driver.tick();
    expect(driver.source.isClipped("1:master")).toBe(true);
    advance(25);
    publish(150n, [0.25, 0.25]);
    driver.clearClip("1:master"); // The next crossing is new even if the low frame came between ticks.
    advance(25);
    publish(160n, [0.5, 1.5]);
    driver.tick();
    expect(driver.source.isClipped("1:master")).toBe(true);
    advance(250);
    driver.tick();
    expect(read("1:master")).toBeNull();
    driver.setScope(scope(2, 0));
    driver.tick();
    expect(read("1:master")).toBeNull();
    expect(read("2:master")).toBeNull();
    expect(driver.source.isClipped("2:master")).toBe(false);
    expect(requests).toEqual([]);
  });

  it("closes a hidden lease and stops live reads and polling", async () => {
    const { driver, requests, open, read, answer, cancel, liveReads } = setup();
    open();
    driver.setScope(scope(1, 1));
    driver.source.setVisible(["1:track"]);
    const outstanding = driver.tick();
    driver.stop();
    const lastRead = liveReads();
    driver.tick();
    expect(read("1:track")).toBeNull();
    expect(read("1:master")).toBeNull();
    expect(liveReads()).toBe(lastRead);
    answer(0, 60n, 60n);
    await outstanding;
    expect(requests[1]).toEqual({ generation: 1, revision: 1, tracks: [] });
    driver.tick();
    expect(requests).toHaveLength(2);
    open();
    driver.setScope(scope(1, 2));
    expect(driver.tick()).toBeNull();
    cancel(1);
    await Promise.resolve();
    await Promise.resolve();
    expect(requests[2]).toEqual({ generation: 1, revision: 2, tracks: [] });
    expect(driver.tick()).toBeNull();
    cancel(2);
    await Promise.resolve();
    await Promise.resolve();
    const reopened = driver.tick();
    expect(requests[3]).toEqual({ generation: 1, revision: 2, tracks: [7] });
    answer(3, 80n, null);
    await reopened;
  });
});
