import { markerRange, type Region } from "./waveform";
/** Diagnosis only. Reading geometry never changes the stored marker pair. */
export function trimRepairState(region: Region) {
  const known = region.frames !== null && Number.isSafeInteger(region.frames) && region.frames > 0;
  return {
    damaged: known && (region.start > region.end || region.end * region.frames! - region.start * region.frames! < 1),
    stuck: known && markerRange("start", region) === null && markerRange("end", region) === null,
    wholeFrame: known && region.frames === 1 && region.start === 0 && region.end === 1,
  };
}
