import type { Edit, OpenReport, PatternStamp } from "./generated/contract";
import { invokeCommand } from "./ipc";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// The shapes and commands of the project session (app/src-tauri/src/project_cmds.rs). Every
// command is async; a failure rejects with a sentence that is fit to show.

export type { ChannelView, Edit, PatternView, ProjectKey, ProjectView, TimeSignature } from "./generated/contract";
export type OpenResult = OpenReport;

export type ExportFormat = "pcm24" | "float32";

export interface ExportOptions {
  /** 8000..=384000. */
  sampleRate: number;
  format: ExportFormat;
  /** 1..=999. */
  loops: number;
  /** Song only: export the enabled loop region; false/default exports the whole song. */
  songRegion?: boolean;
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

export const projectView = () => invokeCommand("project_view");
/** Applies `edit`, made in the project of `generation`. */
export const projectEdit = (edit: Edit, generation: number, stamp?: PatternStamp) =>
  invokeCommand("project_edit", { edit, generation, stamp });
/** Adds a channel playing the file at `path` to the project of `generation`. */
export const channelAdd = (path: string, generation: number) =>
  invokeCommand("channel_add", { path, generation });
/** Points a channel at another sample (relinking a missing one). */
export const channelSetSample = (channel: number, path: string, generation: number) =>
  invokeCommand("channel_set_sample", { channel, path, generation });
/** Plays the channel's sample for a preview: the press, which its release names. Only ever from a
 * click or key press. */
export const channelPreviewOn = (channel: number) => invoke<number>("channel_preview", { channel, on: true });
/** Stops the preview `press` of `channel`. */
export const channelPreviewOff = (channel: number, press: number) =>
  invoke<null>("channel_preview", { channel, on: false, press });
export const transportPlay = () => invokeCommand("transport_play");
export const transportStop = () => invokeCommand("transport_stop");
export const projectNew = () => invokeCommand("project_new");
export const projectOpen = (path: string) => invokeCommand("project_open", { path });
export const projectSave = () => invokeCommand("project_save");
export const projectSaveAs = (path: string) => invokeCommand("project_save_as", { path });
export const projectExport = (path: string, options: ExportOptions) =>
  invoke<ExportResult>("project_export", { path, options });
export const projectExportCancel = () => invoke<null>("project_export_cancel");
export const recoveryStatus = () => invoke<RecoveryStatus | null>("recovery_status");
export const recoveryRestore = () => invokeCommand("recovery_restore");
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
