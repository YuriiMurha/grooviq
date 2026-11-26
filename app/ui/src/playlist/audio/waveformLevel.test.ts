import { expect, it } from "vitest";
import { waveformRequest } from "./waveformLevel";

const fullClip = {
  clipLeftPx: 0, clipWidthPx: 256, viewportWidthPx: 1024,
  sourceStart: 1024, sourceEnd: 9216, sourceFrames: 16384,
};

it("increases bin density when zooming in and reduces it when zooming out", () => {
  expect(waveformRequest(fullClip)).toEqual({ start: 1024, end: 9216, bins: 256 });
  expect(waveformRequest({ ...fullClip, clipWidthPx: 512 })).toEqual({ start: 1024, end: 9216, bins: 512 });
  expect(waveformRequest({ ...fullClip, clipWidthPx: 64 })).toEqual({ start: 1024, end: 9216, bins: 64 });
});

it("queries exactly one bin for a one-frame clip", () => {
  expect(waveformRequest({ ...fullClip, clipWidthPx: 0.1, sourceEnd: 1025 }))
    .toEqual({ start: 1024, end: 1025, bins: 1 });
});

it("queries just the visible source span with bounded bucket padding", () => {
  const wide = { ...fullClip, clipLeftPx: -40, clipWidthPx: 1024, viewportWidthPx: 100 };
  expect(waveformRequest(wide)).toEqual({ start: 1280, end: 2304, bins: 128 });
  expect(waveformRequest({ ...wide, reverse: true })).toEqual({ start: 7936, end: 8960, bins: 128 });
});

it("reuses a bucket across a one-pixel scroll", () => {
  const wide = { ...fullClip, clipLeftPx: -40, clipWidthPx: 1024, viewportWidthPx: 100 };
  expect(waveformRequest({ ...wide, clipLeftPx: -41 })).toEqual(waveformRequest(wide));
});

it("caps bins at the native limit and at the number of source frames", () => {
  const wide = { ...fullClip, viewportWidthPx: 65536, clipWidthPx: 65536 };
  expect(waveformRequest(wide)?.bins).toBe(8192);
  expect(waveformRequest({ ...wide, sourceEnd: 65536, sourceFrames: 65536 })?.bins).toBe(16384);
});

it("skips offscreen clips and ranges outside the decoded source", () => {
  expect(waveformRequest({ ...fullClip, clipLeftPx: -256 })).toBeNull();
  expect(waveformRequest({ ...fullClip, clipLeftPx: 1024 })).toBeNull();
  expect(waveformRequest({ ...fullClip, sourceEnd: 17000 })).toBeNull();
  expect(waveformRequest({ ...fullClip, sourceEnd: 1024 })).toBeNull();
});
