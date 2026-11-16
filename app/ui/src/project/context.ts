import { createContext, useContext } from "react";
import type { Edit, ProjectView } from "../api/project";

export interface ProjectContextValue {
  /** The current project, `null` until the first `project_view` arrives. */
  view: ProjectView | null;
  /** Sends the edit to the engine and stores the view it returns. Edits are sent one at a
   * time, in order, so an older answer never overwrites a newer one. A rejection shows as a
   * toast; the promise still resolves. */
  apply: (edit: Edit) => Promise<void>;
  /** Stores a view that a command returned (`channelAdd`, `channelSetSample`, ...). */
  setView: (view: ProjectView) => void;
}

export const ProjectContext = createContext<ProjectContextValue | null>(null);

export function useProject(): ProjectContextValue {
  const value = useContext(ProjectContext);
  if (!value) throw new Error("useProject must be used inside a ProjectContext provider");
  return value;
}
