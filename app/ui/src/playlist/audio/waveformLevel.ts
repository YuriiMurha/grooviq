export type WaveformRequest = Readonly<{ start: number; end: number; bins: number }>;

export type WaveformView = Readonly<{
  /** Clip geometry in CSS pixels at the current zoom, before clipping to the viewport. */
  clipLeftPx: number;
  clipWidthPx: number;
  viewportWidthPx: number;
  sourceStart: number;
  sourceEnd: number;
  sourceFrames: number;
  reverse?: boolean;
}>;

// arrangement_cmds.rs::arrangement_waveform and waveform.rs::PeakPyramid::query
// accept a nonempty half-open source range with at most min(16_384, end - start) bins.
const MAX_BINS = 16_384;
const BINS_PER_BUCKET = 32;

export function waveformRequest(view: WaveformView): WaveformRequest | null {
  const { clipLeftPx, clipWidthPx, viewportWidthPx, sourceStart, sourceEnd, sourceFrames, reverse } = view;
  if (![clipLeftPx, clipWidthPx, viewportWidthPx].every(Number.isFinite) ||
      clipWidthPx <= 0 || viewportWidthPx <= 0 ||
      ![sourceStart, sourceEnd, sourceFrames].every(Number.isSafeInteger) ||
      sourceStart < 0 || sourceStart >= sourceEnd || sourceEnd > sourceFrames) return null;

  const visibleLeft = Math.max(0, -clipLeftPx);
  const visibleRight = Math.min(clipWidthPx, viewportWidthPx - clipLeftPx);
  if (visibleLeft >= visibleRight) return null;

  const sourceSpan = sourceEnd - sourceStart;
  const framesPerPixel = Math.max(1, sourceSpan / Math.max(1, clipWidthPx));
  const framesPerBin = 2 ** Math.ceil(Math.log2(framesPerPixel));
  const bucketFrames = framesPerBin * BINS_PER_BUCKET;
  const firstPixel = reverse ? clipWidthPx - visibleRight : visibleLeft;
  const lastPixel = reverse ? clipWidthPx - visibleLeft : visibleRight;
  // Source-aligned padding keeps the range and bin count stable inside a bucket.
  const firstFrame = sourceStart + firstPixel / clipWidthPx * sourceSpan;
  const lastFrame = sourceStart + lastPixel / clipWidthPx * sourceSpan;
  const start = Math.max(sourceStart, Math.floor(firstFrame / bucketFrames) * bucketFrames);
  const end = Math.min(sourceEnd, Math.ceil(lastFrame / bucketFrames) * bucketFrames);
  if (start >= end) return null;
  return { start, end, bins: Math.min(MAX_BINS, Math.ceil((end - start) / framesPerBin)) };
}
