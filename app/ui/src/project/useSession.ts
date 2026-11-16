import { useCallback, useEffect, useRef, useState } from "react";
import { errorText, projectEdit, projectView, type Edit, type ProjectView } from "../api/project";
import type { ProjectContextValue } from "./context";

interface Session {
  view: ProjectView | null;
  /** When the project last became clean (opened, new, saved), in ms; for "Saved N s ago". */
  cleanSince: number;
}

/** The current project and the one queue every command that returns a view goes through. */
export function useSession(onError: (message: string) => void) {
  const [state, setState] = useState<Session>(() => ({ view: null, cleanSince: Date.now() }));
  const latest = useRef(state);
  const queue = useRef<Promise<unknown>>(Promise.resolve());

  const adopt = useCallback((view: ProjectView) => {
    const prev = latest.current;
    const becameClean =
      !view.dirty && (prev.view === null || prev.view.dirty || prev.view.path !== view.path);
    latest.current = { view, cleanSince: becameClean ? Date.now() : prev.cleanSince };
    setState(latest.current);
  }, []);

  /** Runs `fn` after everything before it has finished. */
  const enqueue = useCallback(<T,>(fn: () => Promise<T>): Promise<T> => {
    const run = queue.current.then(fn);
    queue.current = run.then(
      () => undefined,
      () => undefined,
    );
    return run;
  }, []);

  const apply = useCallback(
    async (edit: Edit) => {
      try {
        adopt(await enqueue(() => projectEdit(edit)));
      } catch (e) {
        onError(errorText(e));
      }
    },
    [adopt, enqueue, onError],
  );

  // The project the engine holds at start.
  useEffect(() => {
    enqueue(projectView).then(adopt, (e: unknown) => onError(errorText(e)));
  }, [adopt, enqueue, onError]);

  const context: ProjectContextValue = { view: state.view, apply, setView: adopt };
  return { view: state.view, cleanSince: state.cleanSince, latest, adopt, enqueue, apply, context };
}
