import { useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { mixerEdit, mixerInsertDetail, mixerView } from "../api/mixer";
import { projectView } from "../api/project";
import { useProject } from "../project/context";
import { registerGestureCancellation } from "../project/pending";
import { isProjectReplacing, isProjectReplacementPending, onProjectReplaceFailed, onProjectReplaced, onProjectReplacing } from "../project/replacement";
import { createMixerHost, mixerHostControls } from "./mixerHost";
import type { MeterSource, MixerProps } from "./mixerModel";

type MixerHostOptions = Readonly<{
  meters?: MeterSource;
  clearClip?: (nodeId: string) => void;
}>;

/** Mount inside ProjectContext. The consumer owns the mixer and insert editor surfaces. */
export function useMixerHost(options: MixerHostOptions = {}) {
  const context = useProject();
  const { stamp, runFor, view } = context;
  const { clearClip } = options;
  const latestView = useRef(view);
  const [host] = useState(() => createMixerHost({
    stamp,
    runFor,
    blocked: isProjectReplacing,
    replacing: isProjectReplacementPending,
    readProject: projectView,
    readMixer: mixerView,
    readInsert: mixerInsertDetail,
    edit: mixerEdit,
    clearClip,
  }));
  useLayoutEffect(() => { host.connect({ stamp, runFor, clearClip }); }, [host, stamp, runFor, clearClip]);
  const state = useSyncExternalStore(host.subscribe, host.current);
  useLayoutEffect(() => {
    host.start();
    const offCancel = registerGestureCancellation(host.invalidateGestures);
    const offReplacing = onProjectReplacing(host.retire);
    const offReplaced = onProjectReplaced(host.retire);
    const offFailed = onProjectReplaceFailed(() => host.observe(latestView.current));
    return () => {
      offCancel();
      offReplacing();
      offReplaced();
      offFailed();
      host.stop();
    };
  }, [host]);
  useLayoutEffect(() => {
    latestView.current = view;
    host.observe(view);
  }, [host, view]);

  const controls = useMemo(() => state.binding && mixerHostControls(state.binding, state.pending), [state.binding, state.pending]);
  const props: MixerProps | null = controls && {
    ...controls,
    selectedId: state.selectedId,
    error: state.error,
    meters: options.meters,
    onCommand: host.command,
  };

  const owner = state.insert;
  const insert = owner && {
    ...owner,
    loading: owner.detail === null,
    pending: state.pending,
    setParams: (params: readonly number[]) => host.applyInsert(owner, { kind: "setInsert", params }),
    applyPreset: (preset: string) => host.applyInsert(owner, { kind: "applyPreset", preset }),
    close: () => host.closeInsert(owner),
  };
  const binding = state.binding;
  const routing = binding && {
    sources: binding.routing.sources,
    destinations: binding.routing.destinations,
    assign: (sourceId: string, destinationId: string) => host.assignSource(binding, sourceId, destinationId),
    addTrack: () => host.addTrack(binding),
  };
  return {
    props,
    routing,
    loading: state.binding === null && state.error === null,
    pending: state.pending,
    error: state.error,
    insert,
    closeInsert: () => host.closeInsert(),
    refresh: host.refresh,
    resolveTrack: state.binding?.resolveTrack,
  };
}
