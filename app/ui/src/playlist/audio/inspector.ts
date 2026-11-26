import type { ArrangementOperation, ArrangementPage, ArrangementStamp, AudioContent, ClipSummary } from "../../api/arrangement";
import { NOTE_NAMES, scaleLabel } from "../../roll/scales";
import { audioDurationTicks } from "./geometry";

export type InspectorClip = Pick<ClipSummary, "id" | "startTick"> & { content: AudioContent };
export type InspectorAsset = ArrangementPage["audioAssets"][number];
export type SliderField = "gain" | "fadeIn" | "fadeOut";
export type SliderDraft = { field: SliderField; value: number; stamp: ArrangementStamp };
export type InspectorResize = Extract<ArrangementOperation, { kind: "resize" }>;
export type InspectorEdit = { field: SliderField; value: number } | { field: "reverse"; value: boolean };

export const clipFrames = (content: AudioContent) => content.sourceEnd - content.sourceStart;
export const MAX_GAIN = 2;
const clamp = (value: number, max: number) => Math.min(max, Math.max(0, value));

export const sameInspectorStamp = (a: ArrangementStamp, b: ArrangementStamp) =>
  a.generation === b.generation && a.revision === b.revision;

export function sliderDraftAt(draft: SliderDraft | null, field: SliderField, stamp: ArrangementStamp): SliderDraft | null {
  return draft?.field === field && sameInspectorStamp(draft.stamp, stamp) ? draft : null;
}

export function audioInspectorEdit(clip: InspectorClip, edit: InspectorEdit): InspectorResize | null {
  const { content } = clip;
  let next: AudioContent;
  if (edit.field === "reverse") {
    next = { ...content, reverse: edit.value };
  } else {
    if (!Number.isFinite(edit.value)) return null;
    const value = edit.field === "gain"
      ? clamp(edit.value, MAX_GAIN)
      : clamp(Math.round(edit.value), clipFrames(content));
    next = { ...content, [edit.field]: value };
  }
  if (next[edit.field] === content[edit.field]) return null;
  return { kind: "resize", clip: clip.id, startTick: clip.startTick, content: next };
}

const decimal = (value: number, places: number) => Number(value.toFixed(places));
const quantity = (value: number, unit: string) => `${value} ${unit}${value === 1 ? "" : "s"}`;

export const formatFade = (frames: number, sourceRate: number) => `${decimal(frames / sourceRate * 1000, 3)} ms`;

export function audioInspectorInfo(content: AudioContent, asset: Pick<InspectorAsset, "sourceRate" | "originalTempo" | "originalKey">,
  bpm: number, beats: number, unit: number) {
  const frames = clipFrames(content);
  const durationBeats = audioDurationTicks(frames, asset.sourceRate, bpm) / (3840 / unit);
  return {
    seconds: `${decimal(frames / asset.sourceRate, 6)} s`,
    musicalLength: [
      quantity(decimal(durationBeats, 3), "beat"),
      quantity(decimal(durationBeats / beats, 3), "bar"),
    ].join(" · "),
    originalTempo: asset.originalTempo === null ? "—" : `${decimal(asset.originalTempo, 2)} BPM`,
    originalKey: asset.originalKey === null ? "—"
      : `${NOTE_NAMES[asset.originalKey.root]} ${scaleLabel(asset.originalKey.scale).toLowerCase()}`,
  };
}
