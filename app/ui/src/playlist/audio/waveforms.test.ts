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

it("refreshes only the asset relinked at the same sample reference", async () => {
  const [target] = visibleWaveforms(arrangement, clips, view, new Set());
  const other = { ...asset, id: 9 };
  const desired = [target, { ...target, key: { ...target.key, asset: 9 } }];
  const fetch = vi.fn(async ({ asset }: { asset: number }) => ({ ...packet, asset }));
  const publish = vi.fn<(rows: readonly unknown[]) => void>();
  const loader = createWaveformLoader(fetch, publish);
  loader.update(3, new Map(), [asset, other], desired);
  await vi.waitFor(() => expect(publish.mock.lastCall?.[0]).toHaveLength(2));
  loader.update(3, new Map([[4, 1]]), [asset, other], desired);
  expect(fetch.mock.calls.map(([request]) => request.asset)).toEqual([4, 9, 4]);
});
