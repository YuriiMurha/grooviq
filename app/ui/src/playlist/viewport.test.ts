import { expect, it } from "vitest";
import type { ArrangementPage } from "../api/arrangement";
import { extent, hitClip, projectClips, splitTick, viewport } from "./viewport";

it("derives scroll in the same CSS coordinates as zoom and clips", () => {
  const v = viewport(800, 200, 160, 96, 192, 3840);
  expect(v.scrollTick).toBe(3200);
  expect(v.scrollTrack).toBe(1.5);
  expect(v.tickWidth).toBe(0.05);
  expect(v.rowHeight).toBe(64);
  expect(() => viewport(0, 200, 0, 0, 192, 3840)).toThrow();
  expect(() => viewport(800, 200, NaN, 0, 192, 3840)).toThrow();
});

it("keeps high tick coordinates exact before near-camera shader subtraction", () => {
  const at = splitTick(38_399_999);
  const camera = splitTick(38_399_500);
  expect(at[0] * 4096 + at[1]).toBe(38_399_999);
  expect(Math.fround(Math.fround(at[0] - camera[0]) * 4096 + Math.fround(at[1] - camera[1]))).toBe(499);
  expect(() => splitTick(Infinity)).toThrow();
});

it("projects the timeline extents of all clip kinds", () => {
  const page: ArrangementPage = { generation: 1, revision: 2, offset: 0, total: 3, tracks: [{ id: 5, name: "T", color: 2, mute: false }], clips: [
    { id: 10, track: 5, name: "Pattern", startTick: 960, muted: false, content: { type: "pattern", pattern: 1, offsetTick: 480, lengthTicks: 1920 } },
    { id: 11, track: 5, name: "Audio", startTick: 960, muted: false, content: { type: "audio", asset: 20, sourceStart: 0, sourceEnd: 48000, gain: 1, reverse: false, fadeIn: 0, fadeOut: 0 } },
    { id: 12, track: 5, name: "Automation", startTick: 240, muted: true, content: { type: "automation", target: { type: "channelPan", channel: 1 }, priority: 30, offsetTick: 120, lengthTicks: 960, pointCount: 2 } },
  ], audioAssets: [{ id: 20, sourceRate: 48000, sourceFrames: 48000, sample: { path: "generated.wav", file: null }, originalTempo: null, originalKey: null }], loopRegion: null, playMode: "song", songError: null, pendingRequests: [], warnings: [] };
  const clips = projectClips(page, 120);
  expect(clips.map(c => [c.id, c.kind, c.lengthTicks])).toEqual([[10, "pattern", 1920], [11, "audio", 1920], [12, "automation", 960]]);
  expect(clips.every(c => c.trackIndex === 0 && c.ink === 2)).toBe(true);
  expect(projectClips(page, 60)[1].lengthTicks).toBe(960);
  // The native project stores 120.000001 as f32 120: f64-only maths adds a tick.
  expect(projectClips(page, 120.000001)[1].lengthTicks).toBe(1920);
  const v = viewport(800, 200, 0, 0, 192, 3840);
  expect(extent(clips, 1, v, 3840)).toEqual({ width: 3216, height: 200 });
  expect(page.clips[0].content).toEqual({ type: "pattern", pattern: 1, offsetTick: 480, lengthTicks: 1920 });
  expect(hitClip(clips, 50, 30, v)?.clip.id).toBe(12);
  expect(hitClip(clips, 140, 30, v)?.clip.id).toBe(11);
  expect(hitClip(clips, 144, 30, v)).toBeNull();
  expect(hitClip(clips, 800, 30, v)).toBeNull();
});
