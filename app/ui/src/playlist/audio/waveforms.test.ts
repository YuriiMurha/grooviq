import { expect, it, vi } from "vitest";
import type { ArrangementPage, AudioContent } from "../../api/arrangement";
import { projectClips, viewport } from "../viewport";
import { createWaveformLoader, packWaveforms, visibleWaveforms } from "./waveforms";

const asset = { id: 4, sample: { path: "loop.wav", file: null }, sourceRate: 8000, sourceFrames: 1600, originalTempo: null, originalKey: null };
const content: AudioContent = { type: "audio", asset: 4, sourceStart: 0, sourceEnd: 1600, gain: 1, reverse: false, fadeIn: 0, fadeOut: 0 };
const arrangement: ArrangementPage = { generation: 3, revision: 9, offset: 0, total: 2, audioAssets: [asset],
  tracks: [{ id: 2, name: "Track 1", color: 0, mute: false }, { id: 5, name: "Track 2", color: 1, mute: false }],
  clips: [{ id: 7, track: 2, name: "Loop", startTick: 0, muted: false, content },
    { id: 8, track: 5, name: "Loop", startTick: 960, muted: false, content }],
  warnings: [], playMode: "pattern", songError: null, pendingRequests: [], loopRegion: null };
const clips = projectClips(arrangement, 120);
const view = viewport(100, 64, 0, 0, 192, 3840);
const packet = { generation: 3n, request: 1n, asset: 4, rate: 8000, channels: 2, frames: 800,
  start: 0, end: 800, bins: 1, peaks: new Float32Array([-1, 0, 0, 1]) };

it("plans only on-screen audio and skips missing sources", () => {
  expect(visibleWaveforms(arrangement, clips, view, new Set()).map(target => target.clip.id)).toEqual([7]);
  expect(visibleWaveforms(arrangement, clips, { ...view, scrollTrack: 1 }, new Set()).map(target => target.clip.id)).toEqual([8]);
  expect(visibleWaveforms(arrangement, clips, { ...view, scrollTick: 6000 }, new Set())).toEqual([]);
  expect(visibleWaveforms(arrangement, clips, view, new Set([4]))).toEqual([]);
});

it("keeps one bucket across equivalent viewport updates and does not draw unavailable source frames", async () => {
  const desired = visibleWaveforms(arrangement, clips, view, new Set());
  const fetch = vi.fn(async ({ start, end, bins }: { start: number; end: number; bins: number }) => {
    if (end > packet.frames) throw "Invalid waveform range or bin count";
    return { ...packet, start, end, bins, peaks: new Float32Array(bins * 4) };
  });
  let bars = new Float32Array();
  const loader = createWaveformLoader(fetch, rows => { bars = new Float32Array(packWaveforms(rows)); });
  loader.update(3, new Map(), [asset], desired);
  loader.update(3, new Map(), [asset], visibleWaveforms(arrangement, clips, { ...view, width: 99 }, new Set()));
  await vi.waitFor(() => expect(bars.length).toBe(13 * 8));
  expect(fetch.mock.calls).toEqual([
    [{ generation: 3, asset: 4, request: 1, start: 0, end: 1600, bins: 13 }],
    [{ generation: 3, asset: 4, request: 2, start: 0, end: 1, bins: 1 }],
    [{ generation: 3, asset: 4, request: 3, start: 0, end: 800, bins: 13 }],
  ]);
  expect(bars.at(-7)! + bars.at(-5)!).toBeCloseTo(192);
  loader.update(3, new Map(), [asset], []);
  loader.update(3, new Map(), [asset], desired);
  expect(fetch).toHaveBeenCalledTimes(3);
});

it("maps trimmed reverse frames from the right and combines both channels' peaks", () => {
  const row = { clip: { ...clips[0], startTick: 100, lengthTicks: 200 },
    content: { ...content, sourceStart: 200, sourceEnd: 1000, reverse: true },
    packet: { ...packet, start: 200, end: 600 },
    key: { generation: 3, asset: 4, start: 200, end: 600, bins: 1 } };
  expect(Array.from(packWaveforms([row]))).toEqual([0, 200, 0, 100, -1, 1, 0, 0]);
});

it("keeps the last waveform while another bucket for the same clip loads", async () => {
  const [target] = visibleWaveforms(arrangement, clips, view, new Set());
  const next = { ...target, key: { ...target.key, end: 800 } };
  const newPacket = { ...packet, request: 2n, peaks: new Float32Array(4) };
  const fetch = vi.fn().mockResolvedValueOnce(packet).mockResolvedValueOnce(newPacket);
  const publish = vi.fn();
  const loader = createWaveformLoader(fetch, publish);
  loader.update(3, new Map(), [asset], [target]);
  await vi.waitFor(() => expect(publish.mock.lastCall?.[0]).toEqual([expect.objectContaining({ packet })]));
  loader.update(3, new Map(), [asset], [next]);
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(publish.mock.lastCall?.[0]).toEqual([expect.objectContaining({ packet })]);
  await vi.waitFor(() => expect(publish.mock.lastCall?.[0]).toEqual([expect.objectContaining({ packet: newPacket })]));
});

it.each([
  { change: "generation", generation: 4, assetId: 4, version: 0, clipId: 7 },
  { change: "asset", generation: 3, assetId: 9, version: 0, clipId: 7 },
  { change: "source version", generation: 3, assetId: 4, version: 1, clipId: 7 },
  { change: "clip", generation: 3, assetId: 4, version: 0, clipId: 8 },
])("drops the last waveform when the $change changes", async ({ generation, assetId, version, clipId }) => {
  const [target] = visibleWaveforms(arrangement, clips, view, new Set());
  const assets = [asset, { ...asset, id: 9 }];
  const fetch = vi.fn().mockResolvedValueOnce(packet).mockReturnValue(new Promise(() => {}));
  const publish = vi.fn();
  const loader = createWaveformLoader(fetch, publish);
  loader.update(3, new Map(), assets, [target]);
  await vi.waitFor(() => expect(publish.mock.lastCall?.[0]).toHaveLength(1));
  loader.update(generation, new Map([[assetId, version]]), assets, [{
    clip: { ...target.clip, id: clipId }, content: { ...target.content, asset: assetId },
    key: { ...target.key, generation, asset: assetId, end: 800 },
  }]);
  expect(publish.mock.lastCall?.[0]).toEqual([]);
});

it.each([
  { change: "start", source: { sourceStart: 100 } },
  { change: "end", source: { sourceEnd: 1500 } },
  { change: "direction", source: { reverse: true } },
])("drops the last waveform when the clip's source $change changes", async ({ source }) => {
  const [target] = visibleWaveforms(arrangement, clips, view, new Set());
  const fetch = vi.fn().mockResolvedValueOnce(packet).mockReturnValue(new Promise(() => {}));
  const publish = vi.fn();
  const loader = createWaveformLoader(fetch, publish);
  loader.update(3, new Map(), [asset], [target]);
  await vi.waitFor(() => expect(publish.mock.lastCall?.[0]).toHaveLength(1));
  loader.update(3, new Map(), [asset], [{ ...target, content: { ...target.content, ...source }, key: { ...target.key, end: 800 } }]);
  expect(publish.mock.lastCall?.[0]).toEqual([]);
});

it("drops a held waveform when the new bucket fails for another reason", async () => {
  const [target] = visibleWaveforms(arrangement, clips, view, new Set());
  const fetch = vi.fn().mockResolvedValueOnce(packet).mockRejectedValueOnce("Generated native failure");
  const publish = vi.fn();
  const loader = createWaveformLoader(fetch, publish);
  loader.update(3, new Map(), [asset], [target]);
  await vi.waitFor(() => expect(publish.mock.lastCall?.[0]).toHaveLength(1));
  loader.update(3, new Map(), [asset], [{ ...target, key: { ...target.key, end: 800 } }]);
  expect(publish.mock.lastCall?.[0]).toHaveLength(1);
  await vi.waitFor(() => expect(publish.mock.lastCall?.[0]).toEqual([]));
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("clears a held waveform when native reports its source was replaced", async () => {
  const [target] = visibleWaveforms(arrangement, clips, view, new Set());
  const fetch = vi.fn()
    .mockResolvedValueOnce(packet)
    .mockRejectedValueOnce("Waveform source replaced; reload")
    .mockReturnValue(new Promise(() => {}));
  const publish = vi.fn();
  const loader = createWaveformLoader(fetch, publish);
  loader.update(3, new Map(), [asset], [target]);
  await vi.waitFor(() => expect(publish.mock.lastCall?.[0]).toHaveLength(1));
  loader.update(3, new Map(), [asset], [{ ...target, key: { ...target.key, end: 800 } }]);
  expect(publish.mock.lastCall?.[0]).toHaveLength(1);
  await vi.waitFor(() => expect(fetch).toHaveBeenCalledTimes(3));
  expect(publish.mock.lastCall?.[0]).toEqual([]);
});

it("refreshes only the asset relinked at the same sample reference", async () => {
  const [target] = visibleWaveforms(arrangement, clips, view, new Set());
  const other = { ...asset, id: 9 };
  const desired = [target, { clip: clips[1], content: { ...target.content, asset: 9 }, key: { ...target.key, asset: 9 } }];
  const fetch = vi.fn(async ({ asset }: { asset: number }) => ({ ...packet, asset }));
  const publish = vi.fn<(rows: readonly unknown[]) => void>();
  const loader = createWaveformLoader(fetch, publish);
  loader.update(3, new Map(), [asset, other], desired);
  await vi.waitFor(() => expect(publish.mock.lastCall?.[0]).toHaveLength(2));
  loader.update(3, new Map([[4, 1]]), [asset, other], desired);
  expect(fetch.mock.calls.map(([request]) => request.asset)).toEqual([4, 9, 4]);
});
