import { invoke } from "@tauri-apps/api/core";
import { validateStamp, type ArrangementStamp } from "./arrangement";

type ChannelSource = Readonly<{ channel: number; audioTrack?: never }>;
type AudioSource = Readonly<{ audioTrack: number; channel?: never }>;
export type MixerSourceKey = ChannelSource | AudioSource;
export type MixerSource = Readonly<{ name: string } & (
  | { source: ChannelSource; instrument: "sampler" | "synth" }
  | { source: AudioSource; instrument: null }
)>;
export type MixerLevel = "silence" | Readonly<{ db: number }>;
export type MixerTap = "pre" | "post";

export type MixerInsert = Readonly<{
  id: number;
  device: string;
  params: readonly number[];
  preset: string | null;
  /** Canonical decimal u64. Preserve the string across the IPC boundary. */
  seed: string;
  bypass: boolean;
}>;
export type MixerTrack = Readonly<{
  id: number;
  name: string;
  output: number | null;
  inserts: readonly MixerInsert[];
  level: MixerLevel;
  pan: number;
  mute: boolean;
  solo: boolean;
}>;
export type MixerSend = Readonly<{
  id: number;
  from: number;
  to: number;
  tap: MixerTap;
  /** Linear gain, 0 through 4. */
  gain: number;
}>;
type MixerSidechain = Readonly<{ from: number; to: number; tap: MixerTap }>;
type MixerAssignment = Readonly<{ source: MixerSourceKey; destination: number }>;
type MixerUnsupported = Readonly<Record<"mute" | "solo" | "bypass" | "detector", string>>;
type MixerState = Readonly<{
  mode: "legacyDirect" | "routed";
  tracks: readonly MixerTrack[];
  sends: readonly MixerSend[];
  sidechains: readonly MixerSidechain[];
  /** Sources without an explicit route go to Master (0). */
  sources: readonly MixerAssignment[];
  nextTrackId: number;
  nextInsertId: number;
  nextSendId: number;
}>;
export type MixerRouteInput = Readonly<{ track: number; from: readonly number[] }>;
export type MixerSnapshot = ArrangementStamp & Readonly<{
  mixer: MixerState;
  /** Compiler predecessors per track in ID order. Disable routing choices when null. */
  routeInputs: readonly MixerRouteInput[] | null;
  sources: readonly MixerSource[];
  effects: readonly string[];
  unsupported: MixerUnsupported;
}>;

export type MixerEdit =
  | { kind: "createTrack"; name: string; output: number; source?: MixerSourceKey | null }
  | { kind: "setOutput"; track: number; output: number }
  | { kind: "assignSource"; source: MixerSourceKey; destination: number }
  | { kind: "setFader"; track: number; level?: MixerLevel | null; pan?: number | null }
  | { kind: "addInsert"; track: number; index: number; effectId: string }
  | { kind: "removeInsert"; track: number; insert: number }
  | { kind: "moveInsert"; track: number; insert: number; index: number; destinationTrack?: number | null }
  | { kind: "setInsert"; track: number; insert: number; params: readonly number[] }
  | { kind: "applyPreset"; track: number; insert: number; preset: string }
  | { kind: "addSend"; from: number; to: number; gain: number; tap?: MixerTap }
  | { kind: "setSend"; from: number; send: number; gain?: number | null; tap?: MixerTap | null; to?: number | null }
  | { kind: "removeSend"; from: number; send: number };

type MixerCreation = Readonly<{ kind: "track" | "insert" | "send"; id: number }>;
export type MixerEditReply = Readonly<{ state: MixerSnapshot } & (
  | { status: "applied"; changed: boolean; createdIds: readonly MixerCreation[]; message: null }
  | { status: "refused"; changed: false; createdIds: readonly []; message: string }
)>;

export type MixerInsertTarget = ArrangementStamp & Readonly<{ track: number; insert: number }>;
export type MixerInsertDetail = MixerInsertTarget & Readonly<{
  effectId: string;
  active: boolean;
  params: readonly number[];
  preset: string | null;
  factoryPresets: readonly string[];
}>;
export type MixerInsertReply = Readonly<
  | { status: "applied"; detail: MixerInsertDetail }
  | { status: "refused"; state: MixerSnapshot; message: string }
>;

const MAX_ID = 0xffff_ffff;
const MAX_SEED = "18446744073709551615";
const MAX_REQUEST_BYTES = 16 * 1024;

const isRecord = (raw: unknown): raw is Record<string, unknown> =>
  typeof raw === "object" && raw !== null && !Array.isArray(raw);
const isString = (raw: unknown): raw is string => typeof raw === "string";
const isFiniteNumber = (raw: unknown): raw is number => typeof raw === "number" && Number.isFinite(raw);
const isNatural = (raw: unknown): raw is number => isFiniteNumber(raw) && Number.isSafeInteger(raw) && raw >= 0;
const isId = (raw: unknown): raw is number => isNatural(raw) && raw <= MAX_ID;
const isPositiveId = (raw: unknown): raw is number => isId(raw) && raw > 0;
const isTap = (raw: unknown): raw is MixerTap => raw === "pre" || raw === "post";
const isLabel = (raw: unknown): raw is string | null => raw === null || isString(raw);

function isArrayOf<T>(raw: unknown, isItem: (item: unknown) => item is T): raw is readonly T[] {
  if (!Array.isArray(raw)) return false;
  for (const item of raw) {
    if (!isItem(item)) return false;
  }
  return true;
}

function isSeed(raw: unknown): raw is string {
  return isString(raw) && raw.length <= MAX_SEED.length && /^(?:0|[1-9][0-9]*)$/.test(raw)
    && (raw.length < MAX_SEED.length || raw <= MAX_SEED);
}

function isLevel(raw: unknown): raw is MixerLevel {
  return raw === "silence" || (isRecord(raw) && Object.keys(raw).length === 1
    && Object.hasOwn(raw, "db") && isFiniteNumber(raw.db));
}

function isSourceKey(raw: unknown): raw is MixerSourceKey {
  return isRecord(raw) && Object.keys(raw).length === 1
    && ((Object.hasOwn(raw, "channel") && isId(raw.channel)) || (Object.hasOwn(raw, "audioTrack") && isId(raw.audioTrack)));
}

function isSource(raw: unknown): raw is MixerSource {
  if (!isRecord(raw) || !isSourceKey(raw.source) || !isString(raw.name)) return false;
  return Object.hasOwn(raw.source, "channel")
    ? raw.instrument === "sampler" || raw.instrument === "synth"
    : raw.instrument === null;
}

function isInsert(raw: unknown): raw is MixerInsert {
  return isRecord(raw) && isPositiveId(raw.id) && isString(raw.device) && isArrayOf(raw.params, isFiniteNumber)
    && isLabel(raw.preset) && isSeed(raw.seed) && typeof raw.bypass === "boolean";
}

function isTrack(raw: unknown): raw is MixerTrack {
  return isRecord(raw) && isId(raw.id) && isString(raw.name) && (raw.output === null || isId(raw.output))
    && isArrayOf(raw.inserts, isInsert) && isLevel(raw.level) && isFiniteNumber(raw.pan)
    && typeof raw.mute === "boolean" && typeof raw.solo === "boolean";
}

function isSend(raw: unknown): raw is MixerSend {
  return isRecord(raw) && isPositiveId(raw.id) && isId(raw.from) && isId(raw.to) && isTap(raw.tap)
    && isFiniteNumber(raw.gain) && raw.gain >= 0 && raw.gain <= 4;
}

function isSidechain(raw: unknown): raw is MixerSidechain {
  return isRecord(raw) && isId(raw.from) && isPositiveId(raw.to) && isTap(raw.tap);
}

function isAssignment(raw: unknown): raw is MixerAssignment {
  return isRecord(raw) && isSourceKey(raw.source) && isId(raw.destination);
}

function isState(raw: unknown): raw is MixerState {
  return isRecord(raw) && (raw.mode === "legacyDirect" || raw.mode === "routed")
    && isArrayOf(raw.tracks, isTrack) && isArrayOf(raw.sends, isSend)
    && isArrayOf(raw.sidechains, isSidechain) && isArrayOf(raw.sources, isAssignment)
    && isPositiveId(raw.nextTrackId) && isPositiveId(raw.nextInsertId) && isPositiveId(raw.nextSendId);
}

function isUnsupported(raw: unknown): raw is MixerUnsupported {
  return isRecord(raw) && isString(raw.mute) && isString(raw.solo) && isString(raw.bypass) && isString(raw.detector);
}

function isRouteInput(raw: unknown): raw is MixerRouteInput {
  return isRecord(raw) && isId(raw.track) && isArrayOf(raw.from, isPositiveId);
}

function isRouteInputs(raw: unknown, tracks: readonly MixerTrack[]): raw is readonly MixerRouteInput[] | null {
  if (raw === null) return true;
  if (!isArrayOf(raw, isRouteInput) || raw.length !== tracks.length) return false;
  const ids = new Set(tracks.map(({ id }) => id));
  if (!ids.has(0) || ids.size !== tracks.length) return false;
  // Check the projection's identities and order. Native compilation owns edge validity.
  return raw.every(({ track, from }, index) => ids.has(track) && (index === 0 || raw[index - 1].track < track)
    && from.every((id, at) => ids.has(id) && id !== track && (at === 0 || from[at - 1] < id)));
}

function isSnapshot(raw: unknown): raw is MixerSnapshot {
  return isRecord(raw) && isNatural(raw.generation) && isNatural(raw.revision) && isState(raw.mixer)
    && isArrayOf(raw.sources, isSource) && isArrayOf(raw.effects, isString) && isUnsupported(raw.unsupported)
    && isRouteInputs(raw.routeInputs, raw.mixer.tracks);
}

export function decodeMixerSnapshot(raw: unknown): MixerSnapshot {
  if (!isSnapshot(raw)) throw new Error("Invalid mixer snapshot");
  return raw;
}

function isCreation(raw: unknown): raw is MixerCreation {
  return isRecord(raw) && (raw.kind === "track" || raw.kind === "insert" || raw.kind === "send") && isPositiveId(raw.id);
}

export function decodeMixerEditReply(raw: unknown, expected: ArrangementStamp): MixerEditReply {
  if (!isRecord(raw)) throw new Error("Invalid mixer edit reply");
  const { state, status, changed, createdIds, message } = raw;
  if (isSnapshot(state) && isArrayOf(createdIds, isCreation)) {
    if (status === "refused" && changed === false && createdIds.length === 0 && isString(message)) {
      // Refusals can resync a caller after an edit or project replacement.
      return { status, state, changed, createdIds: [], message };
    }
    if (status === "applied" && typeof changed === "boolean" && message === null
      && createdIds.length <= (changed ? 1 : 0)
      && state.generation === expected.generation && state.revision === expected.revision + (changed ? 1 : 0)) {
      return { status, state, changed, createdIds, message };
    }
  }
  throw new Error("Invalid or stale mixer edit reply");
}

function isInsertDetail(raw: unknown): raw is MixerInsertDetail {
  return isRecord(raw) && isNatural(raw.generation) && isNatural(raw.revision)
    && isId(raw.track) && isPositiveId(raw.insert) && isString(raw.effectId) && typeof raw.active === "boolean"
    && isArrayOf(raw.params, isFiniteNumber) && isLabel(raw.preset) && isArrayOf(raw.factoryPresets, isString);
}

export function decodeMixerInsertReply(raw: unknown, expected: MixerInsertTarget): MixerInsertReply {
  if (!isRecord(raw)) throw new Error("Invalid mixer insert reply");
  const { status, state, message, detail } = raw;
  if (status === "refused" && isSnapshot(state) && isString(message)) return { status, state, message };
  if (status === "applied" && isInsertDetail(detail)
    && detail.generation === expected.generation && detail.revision === expected.revision
    && detail.track === expected.track && detail.insert === expected.insert) {
    return { status, detail };
  }
  throw new Error("Invalid or stale mixer insert reply");
}

function isOptional(raw: unknown, isPresent: (item: unknown) => boolean): boolean {
  return raw === undefined || raw === null || isPresent(raw);
}

/** Check wire types before JSON.stringify, which would turn a non-finite number into null. */
function hasWireEditValues(edit: MixerEdit): boolean {
  switch (edit.kind) {
    case "createTrack":
      return isString(edit.name) && isId(edit.output) && isOptional(edit.source, isSourceKey);
    case "setOutput":
      return isId(edit.track) && isId(edit.output);
    case "assignSource":
      return isSourceKey(edit.source) && isId(edit.destination);
    case "setFader":
      return isId(edit.track) && isOptional(edit.level, isLevel) && isOptional(edit.pan, isFiniteNumber);
    case "addInsert":
      return isId(edit.track) && isNatural(edit.index) && isString(edit.effectId);
    case "removeInsert":
      return isId(edit.track) && isPositiveId(edit.insert);
    case "moveInsert":
      return isId(edit.track) && isPositiveId(edit.insert) && isNatural(edit.index)
        && isOptional(edit.destinationTrack, isId);
    case "setInsert":
      return isId(edit.track) && isPositiveId(edit.insert) && isArrayOf(edit.params, isFiniteNumber);
    case "applyPreset":
      return isId(edit.track) && isPositiveId(edit.insert) && isString(edit.preset);
    case "addSend":
      return isId(edit.from) && isId(edit.to) && isFiniteNumber(edit.gain) && (edit.tap === undefined || isTap(edit.tap));
    case "setSend":
      return isId(edit.from) && isPositiveId(edit.send) && isOptional(edit.to, isId)
        && isOptional(edit.gain, isFiniteNumber) && isOptional(edit.tap, isTap);
    case "removeSend":
      return isId(edit.from) && isPositiveId(edit.send);
  }
}

export function encodeMixerEdit(stamp: ArrangementStamp, edit: MixerEdit): string {
  validateStamp(stamp);
  if (!hasWireEditValues(edit)) throw new Error("Invalid mixer edit");
  const { generation, revision } = stamp;
  const requestJson = JSON.stringify({ generation, revision, edit });
  if (new TextEncoder().encode(requestJson).length > MAX_REQUEST_BYTES) throw new Error("Mixer edit request is too large");
  return requestJson;
}

export async function mixerView(): Promise<MixerSnapshot> {
  return decodeMixerSnapshot(await invoke<unknown>("mixer_view"));
}

export async function mixerEdit(stamp: ArrangementStamp, edit: MixerEdit): Promise<MixerEditReply> {
  const { generation, revision } = stamp;
  const expected = { generation, revision };
  const requestJson = encodeMixerEdit(expected, edit);
  return decodeMixerEditReply(await invoke<unknown>("mixer_edit", { requestJson }), expected);
}

export async function mixerInsertDetail(target: MixerInsertTarget): Promise<MixerInsertReply> {
  const { generation, revision, track, insert } = target;
  const expected = { generation, revision, track, insert };
  validateStamp(expected);
  if (!isId(track) || !isPositiveId(insert)) throw new Error("Invalid mixer insert target");
  return decodeMixerInsertReply(await invoke<unknown>("mixer_insert_detail", expected), expected);
}
