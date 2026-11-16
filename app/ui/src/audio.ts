import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { LiveEndpoint } from "./live/client";
import type { OutputKind } from "./output/kind";

/** One output device, as the `audio_outputs` command lists it. */
export interface Output {
  id: string;
  name: string;
  kind: OutputKind;
  isDefault: boolean;
  /** It plays now. */
  current: boolean;
  /** The rate it runs at (or will be asked for), in Hz. */
  sampleRate: number;
}

export interface LogEntry {
  atMs: number;
  level: "info" | "warn" | "error";
  message: string;
}

export type AudioEventKind =
  | "opened"
  | "switched"
  | "guard-stopped"
  | "lost"
  | "no-device"
  | "open-failed"
  | "restarted"
  | "silenced";

export interface AudioEvent {
  kind: AudioEventKind;
  output?: Output;
  message: string;
}

/** Events that get the warning style: playback stopped, or something went wrong. */
export function eventWarns(kind: AudioEventKind): boolean {
  return kind !== "opened" && kind !== "switched";
}

/** The id `audioSelect` takes for "follow the system default". */
export const SYSTEM_DEFAULT = "default";

export const audioOutputs = () => invoke<Output[]>("audio_outputs");
export const audioSelect = (id: string) => invoke<void>("audio_select", { id });
/** The saved choice (`SYSTEM_DEFAULT` or a device id) and the guard. */
export interface AudioSettings {
  output: string;
  guard: boolean;
}
export const audioSettings = () => invoke<AudioSettings>("audio_settings");
export const audioSetGuard = (on: boolean) => invoke<void>("audio_set_guard", { on });
export const testTone = (on: boolean) => invoke<void>("test_tone", { on });
export const liveEndpoint = () => invoke<LiveEndpoint>("live_endpoint");
export const audioLog = () => invoke<LogEntry[]>("audio_log");

export function onAudioEvent(handler: (e: AudioEvent) => void): Promise<UnlistenFn> {
  return listen<AudioEvent>("audio-event", (e) => handler(e.payload));
}
