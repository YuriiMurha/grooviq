import { useCallback, useEffect, useRef, useState } from "react";
import { patternAction, patternList, type PatternsView } from "../api/patterns";
import { arrangementMode, arrangementView, type ArrangementPage } from "../api/arrangement";
import { errorText, projectView, type ProjectView } from "../api/project";
import type { ProjectStamp } from "../project/context";
import { beginProjectTransition, registerSettlement, TransitionWork } from "../project/pending";
import { isProjectReplacing, isProjectReplacementPending, onProjectReplacing, onProjectReplaceFailed, projectGeneration } from "../project/replacement";
import { acceptPickerReply, canPublishPatterns, performPickerAction, pickerIntent, type PickerAction, type PickerState } from "./lifecycle";

interface SessionAdapter {
  view: ProjectView | null;
  publishPatterns: (state: PatternsView, origin: ProjectStamp, publication?: "admitted") => boolean;
  enqueue: <Result>(command: () => Promise<Result>) => Promise<Result>;
  run: (command: () => Promise<ProjectView>) => Promise<boolean>;
}
const EMPTY: PickerState = { canonical: null, pending: null, refusal: null };

export function usePatterns({ view, publishPatterns, enqueue, run }: SessionAdapter) {
  const [work] = useState(() => new TransitionWork());
  useEffect(() => registerSettlement(work.settle), [work]);
  const [state, setState] = useState<PickerState>(EMPTY);
  const current = useRef(state);
  const [loading, setLoading] = useState(true);
  const [message, setMessage] = useState<string | null>(null);
  const [mode, setMode] = useState<ArrangementPage["playMode"] | null>(null);
  const [modePending, setModePending] = useState(false);
  const request = useRef<object | null>(null);
  const modeRequest = useRef<object | null>(null);
  const generation = view?.generation;
  const revision = view?.patternRevision;
  const notesRevision = view?.notesRevision;
  const selected = view?.pattern.id;
  const update = useCallback((next: PickerState) => { current.current = next; setState(next); }, []);
  const still = useCallback((at: ProjectStamp) => !isProjectReplacementPending() && projectGeneration() === at.replacements, []);

  useEffect(() => onProjectReplacing(() => {
    request.current = null; modeRequest.current = null;
    update(EMPTY); setMode(null); setLoading(true); setModePending(false); setMessage(null);
  }), [update]);

  const reload = useCallback(async () => {
    if (generation === undefined || isProjectReplacing() || current.current.pending) return;
    const identity = {}; request.current = identity;
    const at = { generation, replacements: projectGeneration() };
    setLoading(true);
    try {
      const reply = await enqueue(patternList);
      if (request.current !== identity || !still(at)) return;
      const previous = current.current.canonical;
      if (canPublishPatterns(previous, reply, generation) && publishPatterns(reply, at)) {
        update({ ...current.current, canonical: reply });
      }
    } catch (error) {
      if (request.current === identity && still(at)) setMessage(errorText(error));
    } finally { if (request.current === identity) setLoading(false); }
  }, [generation, enqueue, publishPatterns, still, update]);
  useEffect(() => onProjectReplaceFailed(() => { void reload(); }), [reload]);
  useEffect(() => { void reload(); }, [reload, revision, notesRevision, selected]);

  const act = useCallback(async (action: PickerAction): Promise<boolean> => {
    const base = current.current.canonical;
    if (!base || current.current.pending || modeRequest.current || isProjectReplacing()) return false;
    request.current = null; setLoading(false); setMessage(null);
    const intent = pickerIntent(base, projectGeneration(), action);
    update({ ...current.current, pending: intent, refusal: null });
    const at = { generation: intent.generation, replacements: intent.replacements };
    const isCurrent = () => still(at) && current.current.pending?.identity === intent.identity;
    return work.run(async () => {
      try {
        const reply = await performPickerAction(intent, { isCurrent, canonical: () => current.current.canonical,
          action: (canonical, edit) => patternAction(canonical, edit, work.settleOthers) });
        if (!reply || !isCurrent()) return false;
        const next = acceptPickerReply(current.current, intent, projectGeneration(), reply);
        // Save waits this ticket through publication; an admitted reply cannot arrive after its clean view.
        const accepted = next.canonical !== null && publishPatterns(next.canonical, at, "admitted");
        update({ ...next, canonical: accepted ? next.canonical : current.current.canonical });
        if (reply.status === "refused") setMessage(reply.message);
        return accepted && reply.status === "applied";
      } catch (error) {
        if (isCurrent()) setMessage(errorText(error));
        return false;
      } finally {
        if (current.current.pending?.identity === intent.identity) update({ ...current.current, pending: null });
      }
    }, (accepted) => accepted);
  }, [still, update, publishPatterns, work]);

  useEffect(() => {
    if (generation === undefined) return;
    const at = { generation, replacements: projectGeneration() }; let disposed = false;
    void enqueue(arrangementView).then((reply) => {
      if (!disposed && still(at) && reply.generation === generation) setMode(reply.playMode);
    }, (error: unknown) => { if (!disposed && still(at)) setMessage(errorText(error)); });
    return () => { disposed = true; };
  }, [generation, revision, enqueue, still]);

  const changeMode = useCallback(async (next: "pattern" | "song") => {
    if (generation === undefined || modeRequest.current || current.current.pending || isProjectReplacing()) return;
    const at = { generation, replacements: projectGeneration() }; const identity = {};
    modeRequest.current = identity; setModePending(true); setMessage(null);
    const currentMode = () => projectGeneration() === at.replacements && modeRequest.current === identity;
    return work.run(async () => {
      const finish = beginProjectTransition();
      try {
        if (!await work.settleOthers()) throw new Error("Pending edits were refused. The playback mode was not changed.");
        const applied = await run(async () => {
          if (!currentMode()) throw new Error("The project changed before the mode change.");
          const base = await arrangementView();
          if (!currentMode() || base.generation !== generation) throw new Error("The project changed before the mode change.");
          const reply = await arrangementMode(base, next);
          if (!currentMode() || reply.generation !== generation) throw new Error("The project changed during the mode change.");
          if (reply.status === "refused") throw new Error(reply.message ?? "The playback mode was refused.");
          const canonical = await arrangementView();
          if (!currentMode() || canonical.generation !== generation) throw new Error("The project changed during the mode change.");
          const fresh = await projectView();
          if (!currentMode() || fresh.generation !== generation) throw new Error("The project changed during the mode change.");
          setMode(canonical.playMode);
          return fresh;
        });
        if (!applied && currentMode()) setMessage("The playback mode could not be changed. Refresh patterns and try again.");
        return applied;
      } catch (error) { if (currentMode()) setMessage(errorText(error)); return false; }
      finally {
        finish();
        if (modeRequest.current === identity) { modeRequest.current = null; setModePending(false); }
      }
    }, (accepted) => accepted);
  }, [generation, run, work]);

  return { state, loading, message, mode, modePending, act, reload, changeMode, unsettled: work.unsettled };
}
export type PatternPicker = ReturnType<typeof usePatterns>;
