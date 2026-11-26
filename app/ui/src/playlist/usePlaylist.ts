import { useEffect, useLayoutEffect, useState } from "react";
import { arrangementEdit, arrangementSeek, arrangementView, audioCancelTicket, audioImport, audioRelink } from "../api/arrangement";
import { patternEdit, patternList } from "../api/patterns";
import { projectView, type ProjectView } from "../api/project";
import type { ProjectStamp } from "../project/context";
import { beginProjectTransition, registerSettlement } from "../project/pending";
import { isProjectReplacing, isProjectReplacementPending, onProjectReplacing, onProjectReplaced, onProjectReplaceFailed, projectGeneration } from "../project/replacement";
import { modalIsOpen } from "../project/shortcuts";
import { EMPTY_PLAYLIST, PlaylistController, type PlaylistState } from "./store";

type SessionAdapter = Readonly<{
  view: ProjectView | null;
  latest: { readonly current: { readonly view: ProjectView | null } };
  enqueue: <T>(command: () => Promise<T>) => Promise<T>;
  enqueueMutation: <T>(command: () => Promise<T>) => Promise<T>;
  adopt: (view: ProjectView) => void;
}>;

/** The binding stores its adapter, and reads native/UI refs only when a command executes. */
class SessionPlaylistBinding {
  private mounted = true;
  readonly controller: PlaylistController;
  constructor(private session: SessionAdapter, changed: (state: PlaylistState) => void) {
    const current = (at: ProjectStamp) => this.mounted && !isProjectReplacementPending() && projectGeneration() === at.replacements && this.session.latest.current.view?.generation === at.generation;
    this.controller = new PlaylistController({
      origin: () => ({ generation: this.session.latest.current.view?.generation ?? 0, replacements: projectGeneration() }),
      current, blocked: isProjectReplacing, seekBlocked: modalIsOpen, beginTransition: beginProjectTransition,
      enqueueRead: command => this.session.enqueue(command),
      enqueueMutation: command => this.session.enqueueMutation(command),
      seek: arrangementSeek, readPage: arrangementView, readPatterns: patternList, edit: arrangementEdit,
      importAudio: (stamp, input, ticket) => audioImport(stamp, { ...input, ticket }),
      relinkAudio: audioRelink, cancelAudio: audioCancelTicket, readProject: projectView,
      selectPattern: (base, pattern) => patternEdit(base, { kind: "select", pattern }),
      publishProject: (view, at) => {
        if (!current(at) || view.generation !== at.generation) return false;
        this.session.adopt(view);
        return true;
      },
      changed: next => { if (this.mounted) changed(next); },
    });
  }
  update(session: SessionAdapter): void { this.session = session; }
  mount(): () => void {
    this.mounted = true;
    return () => { this.mounted = false; this.controller.retireMedia(); this.controller.retire(); };
  }
}

/** Binding only: no second native queue or accepted-edit model lives in this hook. */
export function usePlaylist(session: SessionAdapter) {
  const [state, setState] = useState<PlaylistState>(EMPTY_PLAYLIST);
  const [binding] = useState(() => new SessionPlaylistBinding(session, setState));
  const { controller } = binding;
  useLayoutEffect(() => { binding.update(session); }, [binding, session]);
  useLayoutEffect(() => binding.mount(), [binding]);
  useEffect(() => registerSettlement(controller.settle), [controller]);
  // Save and Open must settle admitted media edits before reading or replacing the project.
  useEffect(() => registerSettlement(controller.settleMedia), [controller]);
  useEffect(() => onProjectReplaced(controller.replaceMedia), [controller]);
  useEffect(() => onProjectReplacing(controller.retire), [controller]);
  useEffect(() => onProjectReplaceFailed(() => { void controller.reload(); }), [controller]);
  // Ordinary observations do not adopt ProjectView, so its identity change cannot create a
  // read/adopt/read loop. An explicit unpublished-edit recovery may adopt once.
  // Generation changes retire identity; ordinary view changes include global-history replies.
  // Observe outside the mutation lane; do not enqueue a read from inside Session.history.
  useEffect(() => {
    if (session.view !== null) void controller.reload();
  }, [controller, session.view]);
  return { state, reload: controller.reload, mutate: controller.mutate, seek: controller.seek, unsettled: controller.unsettled,
    importAudio: controller.importAudio, relinkAudio: controller.relinkAudio, cancelMedia: controller.cancelMedia, settleMedia: controller.settleMedia,
    invalidateWaveforms: controller.invalidateWaveforms };
}
export type PlaylistModel = ReturnType<typeof usePlaylist>;
