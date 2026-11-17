import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import {
  appExit,
  errorText,
  onAutosaved,
  onCloseRequested,
  projectNew,
  projectOpen,
  projectSave,
  projectSaveAs,
  recoveryDiscard,
  recoveryRestore,
  recoveryStatus,
  type OpenResult,
  type ProjectView,
  type RecoveryStatus,
} from "../api/project";
import { formatClock } from "./ago";
import { ConfirmDialog } from "./ConfirmDialog";
import { ExportDialog } from "./ExportDialog";
import { projectReplaceFailed, projectReplaced, projectReplacing } from "./replacement";
import type { MenuAction } from "./shortcuts";

const PROJECT_FILTER = [{ name: "Grooviq project", extensions: ["grooviq"] }];

/** What the "Save changes?" prompt is for. */
type Why = "new" | "open" | "close";
const WHY_TEXT: Record<Why, string> = {
  new: "before starting a new project",
  open: "before opening another project",
  close: "before closing Grooviq",
};

interface Pending {
  why: Why;
  name: string;
  /** What happens once the changes are saved or dropped. */
  proceed: () => Promise<void>;
}

interface Deps {
  /** The current project; the flows read it through a ref, so this may change at will. */
  view: ProjectView | null;
  adopt: (view: ProjectView) => void;
  enqueue: <T>(fn: () => Promise<T>) => Promise<T>;
  toast: (message: string, warn: boolean) => void;
  /** Stops the transport if it plays (a project that is replaced must not keep playing). */
  stopIfPlaying: () => Promise<void>;
  /** A change was sent (or queued) and its view is not in yet: the project counts as unsaved. */
  unsettled: () => boolean;
  /** Resolves once the piano roll's queued edits are in the engine's project. */
  settle: () => Promise<void>;
}

/** Every project action (new, open, save, save as, export), the "Save changes?" guard, the
 * close request, crash recovery and the autosave clock. It returns the dialogs to render. */
export function useProjectFlows({ view, adopt, enqueue, toast, stopIfPlaying, unsettled, settle }: Deps) {
  const [pending, setPending] = useState<Pending | null>(null);
  const [recovery, setRecovery] = useState<RecoveryStatus | null>(null);
  const [exporting, setExporting] = useState(false);
  const [autosavedAt, setAutosavedAt] = useState<number | null>(null);
  const viewRef = useRef(view);
  const busy = useRef(false);
  const unsettledRef = useRef(unsettled);
  const settleRef = useRef(settle);

  useEffect(() => {
    viewRef.current = view;
    unsettledRef.current = unsettled;
    settleRef.current = settle;
  }, [view, unsettled, settle]);

  const fail = useCallback((e: unknown) => toast(errorText(e), true), [toast]);

  /** Opens or recovers a project: the view, then one toast per warning. */
  const adoptOpened = useCallback(
    ({ view: opened, warnings }: OpenResult) => {
      adopt(opened);
      setAutosavedAt(null);
      for (const w of warnings) toast(w, true);
    },
    [adopt, toast],
  );

  /** Saves as a path the person picks; `false` if they cancel or it fails. */
  const saveAs = useCallback(async (): Promise<boolean> => {
    const current = viewRef.current;
    if (!current) return false;
    try {
      const path = await saveDialog({
        title: "Save project as",
        defaultPath: current.path ?? `${current.name}.grooviq`,
        filters: PROJECT_FILTER,
      });
      if (!path) return false;
      // The notes the person drew are saved too, even those still on their way.
      await settleRef.current();
      adopt(await enqueue(() => projectSaveAs(path)));
      return true;
    } catch (e) {
      fail(e);
      return false;
    }
  }, [adopt, enqueue, fail]);

  /** Saves in place (an untitled project asks for a path); `true` only if it is saved. */
  const save = useCallback(async (): Promise<boolean> => {
    const current = viewRef.current;
    if (!current) return false;
    if (current.path === null) return saveAs();
    try {
      await settleRef.current();
      adopt(await enqueue(projectSave));
      return true;
    } catch (e) {
      // Including "already being saved": a toast, and the app stays as it was.
      fail(e);
      return false;
    }
  }, [adopt, enqueue, fail, saveAs]);

  /** Sends a command that replaces the project (New, Open, Restore), telling what is keyed by
   * the old project's channels before it goes and once it succeeded or failed. */
  const replaceWith = useCallback(
    async <T,>(command: () => Promise<T>): Promise<T> => {
      projectReplacing();
      try {
        const result = await enqueue(command);
        projectReplaced();
        return result;
      } catch (e) {
        projectReplaceFailed();
        throw e;
      }
    },
    [enqueue],
  );

  const newProject = useCallback(async () => {
    await stopIfPlaying();
    adopt(await replaceWith(projectNew));
    setAutosavedAt(null);
  }, [adopt, replaceWith, stopIfPlaying]);

  const openProject = useCallback(async () => {
    const path = await openDialog({ title: "Open project", multiple: false, directory: false, filters: PROJECT_FILTER });
    if (typeof path !== "string") return;
    await stopIfPlaying();
    adoptOpened(await replaceWith(() => projectOpen(path)));
  }, [adoptOpened, replaceWith, stopIfPlaying]);

  /** Runs `proceed` now if nothing is unsaved, else after the "Save changes?" prompt. */
  const guarded = useCallback((why: Why, proceed: () => Promise<void>) => {
    const current = viewRef.current;
    // An edit still on its way may make it dirty: ask, rather than drop it unasked.
    if (current?.dirty === false && !unsettledRef.current()) {
      proceed().catch(fail);
    } else {
      setPending({ why, name: current?.name ?? "this project", proceed });
    }
  }, [fail]);

  const perform = useCallback(
    (action: MenuAction) => {
      if (action === "export") {
        setExporting(true);
        return;
      }
      // One flow at a time: a second shortcut while a file prompt is up does nothing.
      if (busy.current) return;
      const run = (work: () => Promise<unknown>) => {
        busy.current = true;
        work()
          .catch(fail)
          .finally(() => {
            busy.current = false;
          });
      };
      if (action === "new") guarded("new", newProject);
      else if (action === "open") guarded("open", openProject);
      else if (action === "save") run(save);
      else run(saveAs);
    },
    [fail, guarded, newProject, openProject, save, saveAs],
  );

  const answer = (p: Pending, how: "save" | "discard" | "cancel") => {
    setPending(null);
    if (how === "cancel") return;
    const go = async () => {
      if (how === "save" && !(await save())) return;
      await p.proceed();
    };
    go().catch(fail);
  };

  // The window was closed, or the app asked to quit, with unsaved changes.
  useEffect(() => {
    const unlisten = onCloseRequested(() => {
      setPending({
        why: "close",
        name: viewRef.current?.name ?? "this project",
        proceed: async () => {
          await appExit();
        },
      });
    });
    return () => {
      unlisten.then((stop) => stop());
    };
  }, []);

  useEffect(() => {
    const unlisten = onAutosaved(setAutosavedAt);
    return () => {
      unlisten.then((stop) => stop());
    };
  }, []);

  // A crash left an autosave behind: offer it.
  useEffect(() => {
    recoveryStatus().then(
      (status) => setRecovery(status),
      () => {},
    );
  }, []);

  const restore = () => {
    setRecovery(null);
    replaceWith(recoveryRestore).then(adoptOpened, fail);
  };
  const discard = () => {
    setRecovery(null);
    recoveryDiscard().catch(fail);
  };

  const dialogs: ReactNode = (
    <>
      {recovery && (
        <ConfirmDialog
          title="Restore your project?"
          message={`Grooviq closed unexpectedly. Restore the project autosaved at ${formatClock(recovery.savedAt)}?`}
          actions={[
            { label: "Restore", hint: "Opens the autosaved project, with its unsaved changes.", primary: true, onSelect: restore },
            { label: "Discard", hint: "Throws the autosave away.", onSelect: discard },
          ]}
        />
      )}
      {exporting && view && <ExportDialog name={view.name} onClose={() => setExporting(false)} />}
      {pending && (
        <ConfirmDialog
          title="Save changes?"
          message={`Save the changes to "${pending.name}" ${WHY_TEXT[pending.why]}?`}
          actions={[
            { label: "Save", hint: "Saves the project, then carries on.", primary: true, onSelect: () => answer(pending, "save") },
            { label: "Don't save", hint: "Carries on and loses the unsaved changes.", onSelect: () => answer(pending, "discard") },
            { label: "Cancel", hint: "Goes back to the project.", onSelect: () => answer(pending, "cancel") },
          ]}
          onDismiss={() => answer(pending, "cancel")}
        />
      )}
    </>
  );

  return { perform, dialogs, modalOpen: pending !== null || recovery !== null || exporting, autosavedAt };
}
