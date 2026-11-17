import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ProjectView } from "./project";

// The pattern's notes and the live keyboard (app/src-tauri project_cmds.rs, Task 7). Notes come
// as one binary buffer (wire format v1) and change by id through `notesEdit`.

/** One note as the wire carries it. `velocity` is 0..1, `key` 0..127, `length` >= 1 tick. */
export interface WireNote {
  id: number;
  channel: number;
  tick: number;
  length: number;
  velocity: number;
  key: number;
  slide: boolean;
}

/** A note to add: the engine gives it an id. */
export interface NewNote {
  channel: number;
  tick: number;
  length: number;
  key: number;
  velocity: number;
  slide: boolean;
}

/** One edit to the notes: deletes, then updates, then adds. Made against `baseRevision`. */
export interface NotesEdit {
  baseRevision: number;
  add: NewNote[];
  update: WireNote[];
  delete: number[];
}

export interface NotesAck {
  revision: number;
  /** The ids of `add`, in order. */
  addedIds: number[];
  /** Ids of `update` and `delete` the pattern no longer had. */
  ignoredIds: number[];
  /** The edit was made against an old revision or had values clamped: fetch the notes again. */
  reload: boolean;
  view: ProjectView;
}

/** "GQN1", little-endian. */
export const NOTES_MAGIC = 0x314e5147;
const HEADER_BYTES = 16;
const NOTE_BYTES = 24;
const FLAG_SLIDE = 1;

/** Reads a wire format v1 buffer. Throws on a buffer that is not one. */
export function decodeNotes(buf: ArrayBuffer): { revision: number; notes: WireNote[] } {
  if (buf.byteLength < HEADER_BYTES) throw new Error(`The notes buffer is too short (${buf.byteLength} bytes).`);
  const v = new DataView(buf);
  if (v.getUint32(0, true) !== NOTES_MAGIC) throw new Error("This is not a notes buffer (wrong magic).");
  const revision = v.getUint32(4, true);
  const count = v.getUint32(8, true);
  if (buf.byteLength !== HEADER_BYTES + count * NOTE_BYTES) {
    throw new Error(`The notes buffer should hold ${count} notes but is ${buf.byteLength} bytes long.`);
  }
  const notes: WireNote[] = new Array(count);
  for (let i = 0, o = HEADER_BYTES; i < count; i++, o += NOTE_BYTES) {
    notes[i] = {
      id: v.getUint32(o, true),
      channel: v.getUint32(o + 4, true),
      tick: v.getUint32(o + 8, true),
      length: v.getUint32(o + 12, true),
      velocity: v.getFloat32(o + 16, true),
      key: v.getUint8(o + 20),
      slide: (v.getUint8(o + 21) & FLAG_SLIDE) !== 0,
    };
  }
  return { revision, notes };
}

/** Writes a wire format v1 buffer, notes sorted by (tick, channel, id) as the engine sends them.
 * For tests and fakes: the UI never sends notes this way. */
export function encodeNotes(revision: number, notes: WireNote[]): ArrayBuffer {
  const sorted = [...notes].sort((a, b) => a.tick - b.tick || a.channel - b.channel || a.id - b.id);
  const buf = new ArrayBuffer(HEADER_BYTES + sorted.length * NOTE_BYTES);
  const v = new DataView(buf);
  v.setUint32(0, NOTES_MAGIC, true);
  v.setUint32(4, revision, true);
  v.setUint32(8, sorted.length, true);
  sorted.forEach((n, i) => {
    const o = HEADER_BYTES + i * NOTE_BYTES;
    v.setUint32(o, n.id, true);
    v.setUint32(o + 4, n.channel, true);
    v.setUint32(o + 8, n.tick, true);
    v.setUint32(o + 12, n.length, true);
    v.setFloat32(o + 16, n.velocity, true);
    v.setUint8(o + 20, n.key);
    v.setUint8(o + 21, n.slide ? FLAG_SLIDE : 0);
  });
  return buf;
}

/** The basic synth's factory presets (`grooviq_dsp::synth::PRESETS`), "Init" first. */
export const SYNTH_PRESETS = [
  "Init",
  "808 Glide",
  "Sub Bass",
  "Reese",
  "Bell Keys",
  "Dark Pad",
  "Pluck",
  "Trap Lead",
] as const;
export type SynthPreset = (typeof SYNTH_PRESETS)[number];

/** The velocity of a note played from the typing keyboard. */
export const TYPING_VELOCITY = 0.8;

export const patternNotes = async () => decodeNotes(await invoke<ArrayBuffer>("pattern_notes"));
export const notesEdit = (edit: NotesEdit) => invoke<NotesAck>("notes_edit", { edit });
/** Adds a synth channel named after the preset. */
export const channelAddSynth = (preset: string) => invoke<ProjectView>("channel_add_synth", { preset });
/** Starts or stops a note on a channel now (typing keyboard, previews). Only from a key or a click. */
export const channelNote = (channel: number, key: number, velocity: number, on: boolean) =>
  invoke<null>("channel_note", { channel, key, velocity, on });
/** Sets a parameter in the engine only, while a control moves; the project changes on commit. */
export const channelParamLive = (channel: number, param: string, value: number) =>
  invoke<null>("channel_param_live", { channel, param, value });
/** The channel a MIDI keyboard plays (`null`: none). */
export const channelSelect = (channel: number | null) => invoke<null>("channel_select", { channel });
/** `points` (min, max) f32 pairs of the channel's sample, little-endian. */
export const channelWaveform = (channel: number, points: number) =>
  invoke<ArrayBuffer>("channel_waveform", { channel, points });
export const recordSet = (on: boolean) => invoke<ProjectView>("record_set", { on });
export const metronomeSet = (on: boolean) => invoke<null>("metronome_set", { on });

/** Recording switched itself off because recorded notes did not fit in the pattern. */
export interface RecordingStopped {
  reason: "noteCap" | "noteIds" | "error";
  /** Recorded notes that were not added. */
  dropped: number;
  /** A sentence to show. */
  message: string;
}

export function onRecordingStopped(handler: (stopped: RecordingStopped) => void): Promise<UnlistenFn> {
  return listen<RecordingStopped>("recording-stopped", (e) => handler(e.payload));
}

/** The notes changed outside an edit the UI made (recording, ...): the new revision. */
export function onNotesChanged(handler: (revision: number) => void): Promise<UnlistenFn> {
  return listen<number>("notes-changed", (e) => handler(e.payload));
}
