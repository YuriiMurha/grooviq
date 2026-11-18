import type { PatternStamp } from "./patterns";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// The shapes and commands of the project session (app/src-tauri/src/project_cmds.rs). Every
// command is async; a failure rejects with a sentence that is fit to show.

export interface TimeSignature {
  beats: number;
  unit: number;
}

export interface ChannelView {
  id: number;
  name: string;
  /** 0..5: the ink is `--ink-(color + 1)`. */
  color: number;
  sampleName: string | null;
  /** The sample could not be found when the project opened. */
  missing: boolean;
  /** The loaded sample's length in frames; `null` without one (missing, none, or a synth). */
  sampleFrames: number | null;
  /** Linear gain, 0..2 (1 = 0 dB). */
  volume: number;
  /** -1..1. */
  pan: number;
  mute: boolean;
  solo: boolean;
  /** Sampler parameter values by id (empty for a synth). */
  sampler: Record<string, number>;
  kind: "sampler" | "synth";
  /** Synth parameter values by id (empty for a sampler). */
  synth: Record<string, number>;
  /** The synth preset last loaded, if any. */
  preset: string | null;
  /** Choke group, 0..16 (0 = none). */
  choke: number;
  /** The channel has notes that are not plain steps: the rack shows a preview and the piano
   * roll edits them. */
  roll: boolean;
}

export interface PatternView {
  id: number;
  name: string;
  steps: number;
  /** 0..1. */
  swing: number;
  /** Per channel id: one cell per step, a velocity 0..1 or `null` for an empty step. */
  cells: Record<string, (number | null)[]>;
}

export interface ProjectView {
  name: string;
  /** `null` until the project is saved for the first time. */
  path: string | null;
  dirty: boolean;
  bpm: number;
  timeSignature: TimeSignature;
  channels: ChannelView[];
  pattern: PatternView;
  /** Unix seconds of the last autosave. */
  autosavedAt: number | null;
  key: ProjectKey | null;
  /** Bumped by every note change; `patternNotes` reports the same number. */
  notesRevision: number;
  /** Live notes are being recorded into the pattern. */
  recording: boolean;
  /** A note change can be undone (`notesUndo`). */
  canUndo: boolean;
  /** An undone note change can be made again (`notesRedo`). */
  canRedo: boolean;
  /** Counts projects: moves on with every New, Open and Restore. Edits carry the one they were
   * made in, and the engine refuses one made for a project that was replaced since. */
  generation: number;
  patternRevision?: number;
}

/** The project's key: a root (0..11, 0 = C) and a scale id ("minor", "dorian", ...). */
export interface ProjectKey {
  root: number;
  scale: string;
}

/** An edit to the project, as `project_edit` takes it. */
export type Edit =
  | { kind: "toggleStep"; channel: number; step: number }
  | { kind: "setStepVelocity"; channel: number; step: number; velocity: number }
  | { kind: "clearStep"; channel: number; step: number }
  | { kind: "removeChannel"; channel: number }
  | {
      kind: "setChannelMix";
      channel: number;
      volume: number;
      pan: number;
      mute: boolean;
      solo: boolean;
    }
  | { kind: "setSamplerParam"; channel: number; param: string; value: number }
  | { kind: "setPatternLength"; steps: number }
  | { kind: "setSwing"; swing: number }
  | { kind: "setTempo"; bpm: number }
  | { kind: "setTimeSignature"; beats: number; unit: number }
  | { kind: "setInstrumentParam"; channel: number; param: string; value: number }
  | { kind: "loadSynthPreset"; channel: number; preset: string }
  | { kind: "setChoke"; channel: number; group: number }
  | { kind: "setProjectKey"; key: ProjectKey | null };

export interface OpenResult {
  view: ProjectView;
  /** One sentence per thing that did not load as saved (a missing sample, ...). */
  warnings: string[];
}

export type ExportFormat = "pcm24" | "float32";

export interface ExportOptions {
  /** 8000..=384000. */
  sampleRate: number;
  format: ExportFormat;
  /** 1..=999. */
  loops: number;
}

export interface ExportResult {
  frames: number;
  seconds: number;
  /** Floored at -120 for silence. */
  peakDb: number;
  /** Samples that went past full scale. */
  clipped: number;
}

export interface RecoveryStatus {
  /** Unix seconds of the autosave. */
  savedAt: number;
  /** The path the project was saved at, if it had one. */
  original: string | null;
}

/** The sentence to show for a rejected command (they reject with a string; code may throw). */
export function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** BPM range: one place in the engine, project and here. */
export const BPM_MIN = 20;
export const BPM_MAX = 400;

export const projectView = () => invoke<ProjectView>("project_view");
/** Applies `edit`, made in the project of `generation`. */
export const projectEdit = (edit: Edit, generation: number, stamp?: PatternStamp) =>
  invoke<ProjectView>("project_edit", { edit, generation, stamp });
/** Adds a channel playing the file at `path` to the project of `generation`. */
export const channelAdd = (path: string, generation: number) =>
  invoke<ProjectView>("channel_add", { path, generation });
/** Points a channel at another sample (relinking a missing one). */
export const channelSetSample = (channel: number, path: string, generation: number) =>
  invoke<ProjectView>("channel_set_sample", { channel, path, generation });
/** Plays the channel's sample for a preview: the press, which its release names. Only ever from a
 * click or key press. */
export const channelPreviewOn = (channel: number) => invoke<number>("channel_preview", { channel, on: true });
/** Stops the preview `press` of `channel`. */
export const channelPreviewOff = (channel: number, press: number) =>
  invoke<null>("channel_preview", { channel, on: false, press });
export const transportPlay = () => invoke<null>("transport_play");
export const transportStop = () => invoke<null>("transport_stop");
export const projectNew = () => invoke<ProjectView>("project_new");
export const projectOpen = (path: string) => invoke<OpenResult>("project_open", { path });
export const projectSave = () => invoke<ProjectView>("project_save");
export const projectSaveAs = (path: string) => invoke<ProjectView>("project_save_as", { path });
export const projectExport = (path: string, options: ExportOptions) =>
  invoke<ExportResult>("project_export", { path, options });
export const projectExportCancel = () => invoke<null>("project_export_cancel");
export const recoveryStatus = () => invoke<RecoveryStatus | null>("recovery_status");
export const recoveryRestore = () => invoke<OpenResult>("recovery_restore");
export const recoveryDiscard = () => invoke<null>("recovery_discard");
/** Quits cleanly. The UI never closes the window itself. */
export const appExit = () => invoke<null>("app_exit");

/** Progress of an export, 0..1, at most once per percent. */
export function onExportProgress(handler: (fraction: number) => void): Promise<UnlistenFn> {
  return listen<number>("export-progress", (e) => handler(e.payload));
}

/** An autosave happened, at this unix time (seconds). */
export function onAutosaved(handler: (unixSeconds: number) => void): Promise<UnlistenFn> {
  return listen<number>("project-autosaved", (e) => handler(e.payload));
}

/** What the person is owed about autosave, kept by the backend until asked for. */
export interface AutosaveStatus {
  /** Why autosave is off in this copy for good (another Grooviq is running), or `null`. */
  disabled: string | null;
  /** The failing autosave going on now, or `null`. */
  failure: string | null;
}

export const autosaveStatus = () => invoke<AutosaveStatus>("autosave_status");

/** An autosave failed with an error not announced since the last success. Later failures only:
 * ask `autosaveStatus` for what happened before listening. */
export function onAutosaveFailed(handler: (message: string) => void): Promise<UnlistenFn> {
  return listen<string>("autosave-failed", (e) => handler(e.payload));
}

/** The window was closed, or the app was asked to quit, with unsaved changes. */
export function onCloseRequested(handler: () => void): Promise<UnlistenFn> {
  return listen<null>("close-requested", () => handler());
}
