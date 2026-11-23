import type { PatternStamp } from "../api/patterns";
import { createContext, useContext } from "react";
import type { Edit, ProjectView } from "../api/project";

export interface ProjectContextValue {
  publishPatterns?: (state: import("../api/patterns").PatternsView, origin: ProjectStamp) => boolean;
  trim?: (channel: number, action: import("../api/samplerTrim").TrimAction, still: () => boolean) => Promise<import("../api/samplerTrim").TrimReply | null>;
  /** The current project, `null` until the first `project_view` arrives. */
  view: ProjectView | null;
  /** Sends the edit to the engine and stores the view it returns. Edits are sent one at a
   * time, in order, so an older answer never overwrites a newer one. A rejection shows as a
   * toast; the promise still resolves, to whether the edit was applied. With `still`, the edit is
   * dropped quietly (resolving `false`) if `still()` is false when its turn in the queue comes:
   * an edit made for one project must not land in the one that replaced it. */
  apply: (edit: Edit, still?: () => boolean, origin?: PatternStamp) => Promise<boolean>;
  /** Runs another command that returns a view (`channelAdd`, `channelSetSample`, ...) in the
   * same queue as the edits and stores its view. A rejection shows as a toast; the promise
   * resolves to whether it succeeded. */
  run: (command: () => Promise<ProjectView>) => Promise<boolean>;
  /** Stores a view that a command returned. */
  setView: (view: ProjectView) => void;
  /** The project shown now, to stamp a request with (see [`runFor`]). */
  stamp: () => ProjectStamp;
  /** [`run`] for the project of `stamp` (taken when the person acted, before any dialog or
   * decoding): dropped quietly (resolving `false`) if, when its turn comes, a New, Open or
   * Restore is on its way or done since; `command` gets the engine's generation to send, so the
   * engine refuses it too. */
  runFor: (stamp: ProjectStamp, command: (generation: number) => Promise<ProjectView>) => Promise<boolean>;
}

/** A project as a request names it: the engine's generation (`ProjectView.generation`) and the
 * UI's replacement count (`projectGeneration`), which moves the moment a replacement lands. */
export interface ProjectStamp {
  generation: number;
  replacements: number;
}

export const ProjectContext = createContext<ProjectContextValue | null>(null);

export function useProject(): ProjectContextValue {
  const value = useContext(ProjectContext);
  if (!value) throw new Error("useProject must be used inside a ProjectContext provider");
  return value;
}
