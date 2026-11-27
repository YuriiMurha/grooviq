import { invoke } from "@tauri-apps/api/core";

export const MIXER_METER_MAX_TRACKS = 64;
const MAX_U32 = 0xffff_ffff;
const MAX_U64 = 0xffff_ffff_ffff_ffffn;

export type MixerMeterStamp = Readonly<{ generation: number; revision: number }>;
export type MixerMeterRequest = MixerMeterStamp & { readonly tracks: readonly number[] };
export type MixerMeterRow = Readonly<{
  track: number;
  peak: readonly [number, number];
  clipFrame: bigint | null;
}>;
export type MixerMeterCapture = MixerMeterStamp & Readonly<{
  mixRevision: bigint;
  frames: bigint;
  tracks: readonly MixerMeterRow[];
}>;

function isSafeCounter(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

export function isMixerMeterTrack(value: unknown): value is number {
  return isSafeCounter(value) && value > 0 && value <= MAX_U32;
}

export function sameMixerMeterStamp(left: MixerMeterStamp, right: MixerMeterStamp): boolean {
  return left.generation === right.generation && left.revision === right.revision;
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function decodeU64(value: unknown): bigint | null {
  if (typeof value !== "string" || value.length === 0 || value.length > 20
    || /[^0-9]/.test(value) || (value.length > 1 && value[0] === "0")) return null;
  const parsed = BigInt(value);
  return parsed <= MAX_U64 ? parsed : null;
}

function isPeak(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

/** Verifies the project stamp and that the sorted capture contains only requested rows. */
export function decodeMixerMeter(raw: unknown, request: MixerMeterRequest): MixerMeterCapture | null {
  if (!record(raw)) return null;
  const { generation, revision, tracks } = raw;
  if (!isSafeCounter(generation) || !isSafeCounter(revision)
    || !sameMixerMeterStamp({ generation, revision }, request)
    || !Array.isArray(tracks) || tracks.length === 0 || tracks.length > MIXER_METER_MAX_TRACKS) return null;
  const frames = decodeU64(raw.frames);
  const mixRevision = decodeU64(raw.mixRevision);
  if (frames === null || frames === 0n || mixRevision === null) return null;

  const rows: MixerMeterRow[] = [];
  let previous = 0;
  for (const row of tracks) {
    if (!record(row)) return null;
    const { track, peak } = row;
    if (!isMixerMeterTrack(track) || track <= previous || !request.tracks.includes(track)
      || !Array.isArray(peak) || peak.length !== 2) return null;
    const [left, right] = peak;
    if (!isPeak(left) || !isPeak(right)) return null;
    const clipFrame = decodeU64(row.clipFrame);
    if (row.clipFrame !== null && (clipFrame === null || clipFrame === 0n || clipFrame > frames)) return null;
    rows.push({ track, peak: [left, right], clipFrame });
    previous = track;
  }
  return { generation, revision, mixRevision, frames, tracks: rows };
}

/** One transport call. The webview's meter cache owns serialization and lease cancellation. */
export async function requestMixerMeter(request: MixerMeterRequest): Promise<MixerMeterCapture | null> {
  const { generation, revision, tracks } = request;
  if (!isSafeCounter(generation) || !isSafeCounter(revision)) throw new Error("Invalid mixer meter stamp");
  if (tracks.length > MIXER_METER_MAX_TRACKS || [...tracks].some(track => !isMixerMeterTrack(track))) {
    throw new Error("Invalid mixer meter track selection");
  }
  const raw = await invoke<unknown>("mixer_meter", { generation, revision, tracks });
  return decodeMixerMeter(raw, request);
}
