import { expect, it } from "vitest";
import type { AudioContent } from "../../api/arrangement";
import { audioDurationTicks, audioSlice, sourceFrameDelta, trimAudio } from "./geometry";

const clip: AudioContent = { type: "audio", asset: 1, sourceStart: 100, sourceEnd: 1100, reverse: false, gain: 0.5, fadeIn: 900, fadeOut: 800 };
it("keeps audible seconds while tempo and source rate vary", () => {
  for (const rate of [8000, 44100, 48000, 384000]) {
    for (const bpm of [60, 120, 137.1, 400]) {
      expect(audioDurationTicks(rate * 2, rate, bpm)).toBe(Math.ceil(2 * Math.fround(bpm) * 16));
    }
  }
  expect(audioDurationTicks(1, 48000, 120)).toBe(1);
});
it("rounds signed pointer deltas symmetrically in source frames", () => {
  expect(sourceFrameDelta(0.02, 48000, 120)).toBe(1);
  expect(sourceFrameDelta(-0.02, 48000, 120)).toBe(-1);
  expect(sourceFrameDelta(0.01, 48000, 120)).toBe(0);
});
it("clamps chronological trim edges tightly and keeps one frame, including reverse", () => {
  expect(trimAudio(clip, "start", 5000, 2000)).toEqual({ ...clip, sourceStart: 1099, fadeIn: 1, fadeOut: 1 });
  expect(trimAudio(clip, "end", -5000, 2000).sourceEnd).toBe(101);
  expect(trimAudio(clip, "start", -5000, 2000).sourceStart).toBe(0);
  expect(trimAudio(clip, "end", 5000, 2000).sourceEnd).toBe(2000);
  const reverse = { ...clip, reverse: true };
  expect(trimAudio(reverse, "start", 5000, 2000).sourceEnd).toBe(101);
  expect(trimAudio(reverse, "start", -5000, 2000).sourceEnd).toBe(2000);
  expect(trimAudio(reverse, "end", 5000, 2000).sourceStart).toBe(0);
  expect(trimAudio(reverse, "end", -5000, 2000).sourceStart).toBe(1099);
  expect(clip.sourceStart).toBe(100);
});
it("partitions half-open source spans and clears inner fades exactly as native Slice", () => {
  for (const reverse of [false, true]) {
    const result = audioSlice({ ...clip, reverse }, 960, 333, 44100, 137.1);
    expect(result.tick).toBe(977);
    expect(result.audioFirstFrames).toBe(333);
    expect([result.left.sourceStart, result.left.sourceEnd, result.right.sourceStart, result.right.sourceEnd]).toEqual(reverse ? [767, 1100, 100, 767] : [100, 433, 433, 1100]);
    expect([result.left.fadeIn, result.left.fadeOut, result.right.fadeIn, result.right.fadeOut]).toEqual([333, 0, 0, 667]);
    expect(result.left.gain).toBe(0.5);
  }
  expect(() => audioSlice(clip, 0, 0, 44100, 120)).toThrow();
  expect(() => audioSlice(clip, 0, 1000, 44100, 120)).toThrow();
});
it("matches canonical native arithmetic order and the project's f32 tempo contract", () => {
  // Native duration: ceil(frames / rate * f64(project.bpm) * 16).
  // Native slice: ceil(frames / rate * f64(project.bpm) * 960 / 60).
  for (const frames of [1, 333, 11025, 44100, 1234567]) {
    for (const rate of [44100, 48000, 96000]) {
      for (const bpm of [20, 120.000001, 137.1, 399.99999]) {
        const tempo = Math.fround(bpm);
        expect(audioDurationTicks(frames, rate, bpm)).toBe(Math.ceil(frames / rate * tempo * 16));
        const sliced = audioSlice({ ...clip, sourceStart: 0, sourceEnd: frames + 1, fadeIn: 0, fadeOut: 0 }, 0, frames, rate, bpm);
        expect(sliced.tick).toBe(Math.ceil(frames / rate * tempo * 960 / 60));
      }
    }
  }
});
it("rejects invalid geometry instead of emitting unsafe native edits", () => {
  for (const frames of [0, -1, 1.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1]) expect(() => audioDurationTicks(frames, 48000, 120)).toThrow();
  expect(() => audioDurationTicks(100, 0, 120)).toThrow();
  expect(() => sourceFrameDelta(Infinity, 48000, 120)).toThrow();
  expect(() => trimAudio(clip, "end", 1.5, 2000)).toThrow();
  expect(() => trimAudio(clip, "end", 1, 100)).toThrow();
});
