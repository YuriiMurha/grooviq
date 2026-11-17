import { open } from "@tauri-apps/plugin-dialog";
import { channelSetSample, errorText, type ChannelView } from "../api/project";
import type { ProjectContextValue } from "../project/context";
import { sampleReplaced } from "./sampleRevision";

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

/** Asks for the file `channel`'s sample should come from and points the channel at it (the rack's
 * and the sampler editor's Relink), in the project shown when it was asked. A pick that comes
 * back after the project was replaced is dropped: channel ids start again in a new project. */
export function relinkChannel(
  channel: Pick<ChannelView, "id" | "name">,
  project: Pick<ProjectContextValue, "runFor" | "stamp">,
  onError: (message: string) => void,
) {
  const at = project.stamp();
  pickSound(`Find the sample for ${channel.name}`).then(
    (path) => {
      if (path === null) return;
      void project.runFor(at, async (g) => {
        const view = await channelSetSample(channel.id, path, g);
        // Before the view lands, so what is drawn from the old file is fetched again once.
        sampleReplaced(channel.id);
        return view;
      });
    },
    (e: unknown) => onError(errorText(e)),
  );
}
