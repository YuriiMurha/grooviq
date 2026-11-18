import { PatternRevisionChain, ProjectCommandQueue, registerSettlement } from "./pending";
import type { PatternStamp } from "../api/patterns";
import { useCallback, useEffect, useRef, useState } from "react";
import { errorText, projectEdit, projectView, type Edit, type ProjectView } from "../api/project";
import type { ProjectContextValue, ProjectStamp } from "./context";
import { isProjectReplacing, projectGeneration } from "./replacement";

/** An edit whose project was replaced before its turn came: dropped without a toast. */
const STALE = new Error("stale edit");

interface Session {
  view: ProjectView | null;
  /** When the project last became clean (opened, new, saved), in ms; for "Saved N s ago". */
  cleanSince: number;
}

/** The current project and the one queue every command that returns a view goes through. */
export function useSession(onError: (message: string) => void) {
  const [state, setState] = useState<Session>(() => ({ view: null, cleanSince: Date.now() }));
  const latest = useRef(state);
  const queue = useRef(new ProjectCommandQueue());
  const revisions = useRef(new PatternRevisionChain());
  useEffect(() => registerSettlement(() => queue.current.flush()), []);
  /** Commands that change the project, sent or queued, whose view is not in yet. */
  const unanswered = useRef(0);

  const adopt = useCallback((view: ProjectView) => {
    const prev = latest.current;
    // A late reply about a project that was replaced since: the open one is drawn, not it.
    if (prev.view && (view.generation < prev.view.generation ||
      (view.generation === prev.view.generation && (view.patternRevision ?? 0) < (prev.view.patternRevision ?? 0)))) return;
    const becameClean =
      !view.dirty && (prev.view === null || prev.view.dirty || prev.view.path !== view.path);
    latest.current = { view, cleanSince: becameClean ? Date.now() : prev.cleanSince };
    setState(latest.current);
  }, []);

  /** Runs `fn` after everything before it has finished. */
  const enqueue = useCallback(<T,>(fn: () => Promise<T>): Promise<T> => {
    return queue.current.enqueue(fn, false);
  }, []);

  const enqueueMutation = useCallback(<T,>(fn: () => Promise<T>): Promise<T> => queue.current.enqueue(fn, true), []);
  const run = useCallback(
    async (command: () => Promise<ProjectView>) => {
      unanswered.current += 1;
      try {
        adopt(await enqueueMutation(command));
        return true;
      } catch (e) {
        if (e !== STALE) onError(errorText(e));
        return false;
      } finally {
        unanswered.current -= 1;
      }
    },
    [adopt, enqueueMutation, onError],
  );
  /** Sends `edit` for the project shown now: if another replaced it before the edit's turn, the
   * engine refuses it. */
  const apply = useCallback(
    (edit: Edit, still?: () => boolean, origin?: PatternStamp) => {
      if (isProjectReplacing()) return Promise.resolve(false);
      const generation = latest.current.view?.generation ?? 0;
      const at = origin ?? { generation, patternId: latest.current.view?.pattern.id ?? 0, revision: latest.current.view?.notesRevision ?? 1 };
      return run(async () => {
        if (still && !still()) throw STALE;
        const resolved = revisions.current.resolve(at);
        const next = await projectEdit(edit, generation, resolved);
        if (next.generation === resolved.generation && next.pattern.id === resolved.patternId) revisions.current.accepted(resolved, next.notesRevision);
        return next;
      });
    },
    [run],
  );
  /** A change was sent and its view is not in yet: the project may be dirtier than it shows. */
  const unsettled = useCallback(() => unanswered.current > 0, []);
  const stamp = useCallback(
    (): ProjectStamp => ({ generation: latest.current.view?.generation ?? 0, replacements: projectGeneration() }),
    [],
  );
  const runFor = useCallback(
    (at: ProjectStamp, command: (generation: number) => Promise<ProjectView>) =>
      run(() =>
        isProjectReplacing() || projectGeneration() !== at.replacements ? Promise.reject(STALE) : command(at.generation),
      ),
    [run],
  );

  // The project the engine holds at start.
  useEffect(() => {
    enqueue(projectView).then(adopt, (e: unknown) => onError(errorText(e)));
  }, [adopt, enqueue, onError]);

  const context: ProjectContextValue = { view: state.view, apply, run, setView: adopt, stamp, runFor };
  return { view: state.view, cleanSince: state.cleanSince, latest, adopt, enqueue, enqueueMutation, resolvePatternStamp: (origin: PatternStamp) => revisions.current.resolve(origin), apply, unsettled, context };
}
