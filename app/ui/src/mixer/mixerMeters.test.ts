import { describe, expect, it, vi } from "vitest";
import type { MixerMeterCapture, MixerMeterRequest } from "../api/mixerMeter";
import { createMixerMeters } from "./mixerMeters";

const stamp = { generation: 1, revision: 2 };
const replacement = { generation: 2, revision: 0 };

function capture(
  input: MixerMeterRequest,
  frames: bigint,
  clipFrame: bigint | null = null,
  peak: readonly [number, number] = [0.4, 0.2],
): MixerMeterCapture {
  return { ...input, mixRevision: 3n, frames, tracks: input.tracks.map(track => ({ track, peak, clipFrame })) };
}

function heldTransport() {
  const calls: MixerMeterRequest[] = [];
  const replies: Array<(frame: MixerMeterCapture | null) => void> = [];
  const transport = (input: MixerMeterRequest) => new Promise<MixerMeterCapture | null>(resolve => {
    calls.push(input);
    replies.push(resolve);
  });
  return { calls, replies, transport };
}

describe("visible mixer meter cache", () => {
  it("serializes across replacement and retires the old response even when numeric IDs are reused", async () => {
    const { calls, replies, transport } = heldTransport();
    const meters = createMixerMeters(transport);
    meters.setVisible(stamp, [0, 7, 7, 2]);
    expect(meters.read(7)).toBeNull();
    expect(calls).toEqual([]);
    const old = meters.poll();
    expect(calls).toEqual([{ ...stamp, tracks: [2, 7] }]);
    meters.setVisible(replacement, [7]);
    for (let tick = 0; tick < 10; tick++) expect(meters.poll()).toBeNull();
    expect(calls).toHaveLength(1);
    replies[0](capture(calls[0], 1024n, 1024n));
    await old;
    expect(meters.read(7)).toBeNull();
    expect(meters.isClipped(7)).toBe(false);

    const next = meters.poll();
    expect(calls[1]).toEqual({ ...replacement, tracks: [7] });
    replies[1](capture(calls[1], 64n));
    await next;
    expect(meters.read(7)).toEqual([0.4, 0.2]);
  });

  it("cancels a closed lease after the old poll settles and retires its result", async () => {
    const { calls, replies, transport } = heldTransport();
    const meters = createMixerMeters(transport);
    meters.setVisible(stamp, [7]);
    const old = meters.poll();
    meters.close();
    expect(meters.poll()).toBeNull();
    expect(calls).toEqual([{ ...stamp, tracks: [7] }]);
    replies[0](capture(calls[0], 64n, 64n));
    await old;
    expect(calls[1]).toEqual({ ...stamp, tracks: [] });
    expect(meters.read(7)).toBeNull();
    expect(meters.isClipped(7)).toBe(false);
    meters.setVisible(stamp, [4]);
    expect(meters.poll()).toBeNull();
    replies[1](null);
    await Promise.resolve();
    await Promise.resolve();
    const next = meters.poll();
    expect(calls[2]).toEqual({ ...stamp, tracks: [4] });
    replies[2](null);
    await next;
  });

  it("skips queued cancellation if the panel reopens before the pending request settles", async () => {
    const { calls, replies, transport } = heldTransport();
    const meters = createMixerMeters(transport);
    meters.setVisible(stamp, [7]);
    const old = meters.poll();
    meters.close();
    meters.setVisible(stamp, [7]);
    replies[0](capture(calls[0], 128n, 128n));
    await old;
    expect(calls).toHaveLength(1);
    expect(meters.read(7)).toBeNull();
    const next = meters.poll();
    expect(calls[1]).toEqual({ ...stamp, tracks: [7] });
    replies[1](null);
    await next;
  });

  it("expires unchanged captures, ignores older frames and recovers after a rejected poll", async () => {
    let wallTime = 100;
    const transport = vi.fn<(input: MixerMeterRequest) => Promise<MixerMeterCapture | null>>();
    const meters = createMixerMeters(transport, () => wallTime);
    const input = { ...stamp, tracks: [7] };
    meters.setVisible(stamp, input.tracks);
    transport.mockResolvedValueOnce(capture(input, 128n));
    await meters.poll();
    wallTime = 250;
    transport.mockResolvedValueOnce(capture(input, 64n, 64n, [1.4, 0.2]))
      .mockResolvedValueOnce(capture(input, 128n, 128n, [1.4, 0.2]))
      .mockRejectedValueOnce(new Error("Unavailable"));
    await meters.poll();
    await meters.poll();
    await meters.poll();
    wallTime = 349;
    expect(meters.read(7)).toEqual([0.4, 0.2]);
    expect(meters.isClipped(7)).toBe(false);
    wallTime = 350;
    expect(meters.read(7)).toBeNull();
    transport.mockResolvedValueOnce(capture(input, 192n));
    await meters.poll();
    expect(meters.read(7)).toEqual([0.4, 0.2]);
  });

  it("latches clip events without peak notifications and only re-latches on a newer clip", async () => {
    const input = { ...stamp, tracks: [7] };
    const transport = vi.fn<(input: MixerMeterRequest) => Promise<MixerMeterCapture | null>>();
    const meters = createMixerMeters(transport, () => 0);
    const listener = vi.fn();
    const unsubscribe = meters.subscribeClips(listener);
    meters.setVisible(stamp, [7]);

    transport.mockResolvedValueOnce(capture(input, 128n, 64n));
    await meters.poll();
    expect(meters.read(7)).toEqual([0.4, 0.2]);
    expect(meters.isClipped(7)).toBe(true);
    expect(listener).toHaveBeenCalledTimes(1);
    meters.clearClip(7);
    transport.mockResolvedValueOnce(capture(input, 128n, 64n))
      .mockResolvedValueOnce(capture(input, 192n, 64n));
    await meters.poll();
    await meters.poll();
    expect(meters.isClipped(7)).toBe(false);
    expect(listener).toHaveBeenCalledTimes(2);
    // An old clip also stays cleared if a locally changed selection returns to the
    // same server lease before the intervening selection was polled.
    meters.setVisible(stamp, [8]);
    meters.setVisible(stamp, [7]);
    transport.mockResolvedValueOnce(capture(input, 256n, 64n));
    await meters.poll();
    expect(meters.isClipped(7)).toBe(false);
    transport.mockResolvedValueOnce(capture(input, 320n, 256n));
    await meters.poll();
    expect(meters.isClipped(7)).toBe(true);
    expect(listener).toHaveBeenCalledTimes(3);
    unsubscribe();
    meters.clearClip(7);
    expect(listener).toHaveBeenCalledTimes(3);
  });

  it("keeps the first 64 distinct visible IDs before ordering them", async () => {
    const transport = vi.fn().mockResolvedValue(null);
    const meters = createMixerMeters(transport);
    meters.setVisible(stamp, [0, 70, 70, -1, ...Array.from({ length: 70 }, (_, index) => index + 1)]);
    await meters.poll();
    expect(transport).toHaveBeenCalledWith({
      ...stamp, tracks: [...Array.from({ length: 63 }, (_, index) => index + 1), 70],
    });
  });

  it("keeps clip latches on surviving visible tracks when the view changes", async () => {
    const transport = vi.fn<(input: MixerMeterRequest) => Promise<MixerMeterCapture | null>>();
    const meters = createMixerMeters(transport, () => 0);
    const listener = vi.fn();
    const unsubscribe = meters.subscribeClips(listener);
    const input = { ...stamp, tracks: [2, 7] };
    meters.setVisible(stamp, input.tracks);
    transport.mockResolvedValueOnce(capture(input, 128n, 64n));
    await meters.poll();
    expect(meters.isClipped(2)).toBe(true);
    expect(meters.isClipped(7)).toBe(true);

    meters.setVisible(stamp, [7, 8]);
    expect(meters.read(7)).toBeNull();
    expect(meters.isClipped(2)).toBe(false);
    expect(meters.isClipped(7)).toBe(true);
    expect(meters.isClipped(8)).toBe(false);
    expect(listener).toHaveBeenCalledTimes(2);

    meters.setVisible({ ...stamp, revision: stamp.revision + 1 }, [7, 8]);
    expect(meters.isClipped(7)).toBe(true);
    expect(listener).toHaveBeenCalledTimes(2);

    meters.setVisible(replacement, [7, 8]);
    expect(meters.isClipped(7)).toBe(false);
    expect(listener).toHaveBeenCalledTimes(3);
    unsubscribe();
  });
});
