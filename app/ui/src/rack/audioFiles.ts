import { open } from "@tauri-apps/plugin-dialog";

/** The sample formats the loader reads. */
export const AUDIO_EXTENSIONS = ["wav", "aif", "aiff", "flac", "mp3", "ogg"];
const AUDIO_FILTER = [{ name: "Audio", extensions: AUDIO_EXTENSIONS }];

/** Asks for one or more sound files; `[]` when the person cancels. */
export async function pickSounds(): Promise<string[]> {
  const picked = await open({ title: "Add sounds", multiple: true, directory: false, filters: AUDIO_FILTER });
  if (picked === null) return [];
  return Array.isArray(picked) ? picked : [picked];
}

/** Asks for the file a channel's missing sample should come from; `null` when cancelled. */
export async function pickSound(title: string): Promise<string | null> {
  const picked = await open({ title, multiple: false, directory: false, filters: AUDIO_FILTER });
  return typeof picked === "string" ? picked : null;
}
