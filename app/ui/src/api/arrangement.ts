import { invoke } from "@tauri-apps/api/core";
import { decodeWaveform } from "./waveform";

export type ArrangementStamp = Readonly<{ generation: number; revision: number }>;
export type LoopRegion = Readonly<{ startTick: number; endTick: number; enabled: boolean }>;
export type AutomationTarget =
  | { type: "channelVolume" | "channelPan"; channel: number }
  | { type: "instrumentParam"; channel: number; instrument: "sampler" | "synth"; param: string };
export type CurvePoint = Readonly<{ tick: number; value: number; mode: "linear" | "hold" }>;
export type PatternContent = { type: "pattern"; pattern: number; offsetTick: number; lengthTicks: number };
export type AudioContent = { type: "audio"; asset: number; sourceStart: number; sourceEnd: number; gain: number; reverse: boolean; fadeIn: number; fadeOut: number };
export type AutomationContent = { type: "automation"; target: AutomationTarget; priority: number; offsetTick: number; lengthTicks: number; points: readonly CurvePoint[] };
export type ClipContent = PatternContent | AudioContent | AutomationContent;
export type ClipSummary = { id: number; track: number; name: string; startTick: number; muted: boolean; content: PatternContent | AudioContent | (Omit<AutomationContent, "points"> & { pointCount: number }) };
export type ArrangementPage = ArrangementStamp & {
  offset: number; total: number; clips: readonly ClipSummary[];
  tracks: readonly { id: number; name: string; color: number; mute: boolean }[];
  audioAssets: readonly { id: number; sourceRate: number; sourceFrames: number; sample: { path: string; file: string | null }; originalTempo: number | null; originalKey: { root: number; scale: string } | null }[];
  loopRegion: LoopRegion | null; playMode: "pattern" | "song"; songError: string | null; pendingRequests: readonly number[]; warnings: readonly string[];
};
export type ArrangementOperation =
  | { kind: "createTrack"; name: string; color: number }
  | { kind: "setTrack"; track: number; name: string; color: number; mute: boolean }
  | { kind: "deleteTrack"; track: number }
  | { kind: "createClip"; track: number; name: string; startTick: number; muted: boolean; content: ClipContent }
  | { kind: "createClipOnCreatedTrack"; trackOperation: number; name: string; startTick: number; muted: boolean; content: ClipContent }
  | { kind: "move" | "copy"; clip: number; track: number; startTick: number }
  | { kind: "slice"; clip: number; tick: number; audioFirstFrames: number | null }
  | { kind: "resize"; clip: number; startTick: number; content: ClipContent }
  | { kind: "setClip"; clip: number; name: string; muted: boolean }
  | { kind: "deleteClip" | "makeUnique"; clip: number }
  | { kind: "deleteAudioAsset"; asset: number }
  | { kind: "createPattern"; name: string }
  | { kind: "setLoop"; region: LoopRegion | null };
export type ArrangementReply = ArrangementStamp & {
  status: "applied" | "refused"; request: number | null; createdIds: readonly number[];
  changed: boolean; reload: boolean; message: string | null; warnings: readonly string[];
};
export type ProjectHistoryReply = ArrangementStamp & (
  | { status: "applied"; effect: { kind: "arrangement" | "empty" } | { kind: "notes"; pattern: number } | { kind: "samplerTrim"; channel: number } }
  | { status: "refused"; message: string; reload: true }
);
const integer = (value: number) => Number.isSafeInteger(value) && value >= 0;
export function validateStamp(stamp: ArrangementStamp): void {
  if (!integer(stamp.generation) || !integer(stamp.revision)) throw new Error("Invalid arrangement stamp");
}
export function encodeArrangementBatch(operations: readonly ArrangementOperation[]): string {
  if (operations.length > 100_000) throw new Error("Arrangement operation limit exceeded");
  const encoded = JSON.stringify(operations);
  if (new TextEncoder().encode(encoded).length > 64 * 1024 * 1024) throw new Error("Arrangement request exceeds 64 MiB");
  return encoded;
}
export const arrangementView = (offset = 0, limit = 256) => invoke<ArrangementPage>("arrangement_view", { offset, limit });
export function arrangementEdit(stamp: ArrangementStamp, operations: readonly ArrangementOperation[]) {
  validateStamp(stamp);
  return invoke<ArrangementReply>("arrangement_edit", { ...stamp, editsJson: encodeArrangementBatch(operations) });
}
export function arrangementHistory(stamp: ArrangementStamp, undo: boolean) {
  validateStamp(stamp); return invoke<ProjectHistoryReply>("arrangement_history", { ...stamp, undo });
}
export function arrangementCurve(stamp: ArrangementStamp, clip: number) {
  validateStamp(stamp); return invoke<ArrangementStamp & { clip: number; points: readonly CurvePoint[] }>("arrangement_curve", { ...stamp, clip });
}
export function audioImport(stamp: ArrangementStamp, input: { track: number; startTick: number; name: string; path: string }) {
  validateStamp(stamp); return invoke<ArrangementReply>("audio_import", { ...stamp, ...input });
}
export function audioRelink(stamp: ArrangementStamp, asset: number, path: string) {
  validateStamp(stamp); return invoke<ArrangementReply>("audio_relink", { ...stamp, asset, path });
}
/** Pending server request identities are available in arrangementView while preparation awaits. */
export const audioCancel = (request: number) => invoke<boolean>("audio_cancel", { request });
export async function arrangementWaveform(input: { generation: number; asset: number; request: number; start: number; end: number; bins: number }) {
  const packet = await invoke<ArrayBuffer>("arrangement_waveform", input);
  const decoded = decodeWaveform(packet);
  if (decoded.generation !== BigInt(input.generation) || decoded.request !== BigInt(input.request) || decoded.asset !== input.asset) throw new Error("Stale waveform reply");
  return decoded;
}
export function arrangementMode(stamp: ArrangementStamp, mode: "pattern" | "song") {
  validateStamp(stamp); return invoke<ArrangementReply>("arrangement_mode", { ...stamp, mode });
}
export function arrangementLoop(stamp: ArrangementStamp, region: LoopRegion | null) {
  validateStamp(stamp); return invoke<ArrangementReply>("arrangement_loop", { ...stamp, region });
}
export function arrangementSeek(stamp: ArrangementStamp, tick: number) {
  validateStamp(stamp); return invoke<ArrangementReply>("arrangement_seek", { ...stamp, tick });
}
