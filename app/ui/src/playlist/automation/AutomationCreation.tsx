import { useCallback, useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import type { ArrangementStamp } from "../../api/arrangement";
import type { ProjectView } from "../../api/project";
import { ListPicker } from "../../layout/ListPicker";
import { useProject } from "../../project/context";
import { registerGestureCancellation } from "../../project/pending";
import { isProjectReplacing, onProjectReplacing, projectGeneration } from "../../project/replacement";
import { modalIsOpen } from "../../project/shortcuts";
import type { Point } from "../geometry";
import type { PlaylistModel } from "../usePlaylist";
import { automationPlacement, automationStartOptions, buildAutomationCreation, currentControlValue, type ControlTarget, type StartChoice, type TrackChoice } from "./creation";
import { AutomationCreationContext, type PlaylistSelection } from "./creationContext";
import { pointTarget } from "./target";
import "./automation.css";

type Props = {
  model: PlaylistModel;
  rulerPosition: { readonly current: number | null };
  children: ReactNode;
};

type Invocation = {
  sessionView: ProjectView;
  stamp: ArrangementStamp;
  replacements: number;
  target: ControlTarget;
  value: number;
  anchor: HTMLElement;
  at: Point;
  track: number | null;
  starts: readonly StartChoice[];
};

const startNames: Record<StartChoice, string> = { playhead: "Playhead", loop: "Loop start", song: "Song start" };
const sameStamp = (a: ArrangementStamp | null | undefined, b: ArrangementStamp) => a?.generation === b.generation && a.revision === b.revision;

export function AutomationCreation({ model, rulerPosition, children }: Props) {
  const { view } = useProject();
  const [invocation, setInvocation] = useState<Invocation | null>(null);
  const selection = useRef<PlaylistSelection | null>(null);
  const gestureReaders = useRef(new Set<() => boolean>());
  const registerDeviceGesture = useCallback((hasActiveChanges: () => boolean) => {
    const readers = gestureReaders.current;
    readers.add(hasActiveChanges);
    return () => { readers.delete(hasActiveChanges); };
  }, []);
  const setSelection = useCallback((next: PlaylistSelection | null) => { selection.current = next; }, []);
  const dismiss = useCallback(() => setInvocation(null), []);
  const snapshot = model.state.snapshot;
  const active = invocation && invocation.sessionView === view && invocation.replacements === projectGeneration() &&
    sameStamp(snapshot, invocation.stamp) && !model.state.busy && model.state.media === null && !model.state.requiresRefresh ? invocation : null;
  if (invocation && !active) setInvocation(null);

  const deviceGestureActive = () => Array.from(gestureReaders.current).some(hasActiveChanges => hasActiveChanges());
  const editableArrangement = () => {
    const { sessionView, state } = model.automationContext();
    if (!state.snapshot || state.busy || state.media !== null || state.requiresRefresh || isProjectReplacing() || modalIsOpen()) return null;
    return { sessionView, snapshot: state.snapshot };
  };
  const available = () => {
    const current = editableArrangement();
    if (!current?.sessionView || current.sessionView.generation !== current.snapshot.generation || deviceGestureActive()) return null;
    return { sessionView: current.sessionView, snapshot: current.snapshot };
  };
  const stillCurrent = (sessionView: ProjectView) => () =>
    model.automationContext().sessionView === sessionView && !modalIsOpen() && !deviceGestureActive();
  const captureEditGuard = (stamp: ArrangementStamp) => {
    const current = editableArrangement();
    if (!current || !sameStamp(current.snapshot, stamp)) return null;
    if (!current.sessionView || deviceGestureActive()) return "devicePending" as const;
    return current.sessionView.generation === stamp.generation ? stillCurrent(current.sessionView) : null;
  };

  const open = (target: ControlTarget, shown: number, anchor: HTMLElement, at: Point) => {
    const current = available();
    if (!current) return;
    const { sessionView, snapshot } = current;
    const value = currentControlValue(sessionView.channels, target, shown);
    if (value === null) return;
    const selected = selection.current;
    setInvocation({
      sessionView, stamp: { generation: snapshot.generation, revision: snapshot.revision }, replacements: projectGeneration(),
      target, value, anchor, at, track: selected && sameStamp(selected.stamp, snapshot) ? selected.track : null,
      starts: automationStartOptions(rulerPosition.current, snapshot.loopRegion),
    });
  };

  const submit = async (request: Invocation, track: TrackChoice, start: StartChoice) => {
    const current = available();
    if (!request.anchor.isConnected || !current || current.sessionView !== request.sessionView || !sameStamp(current.snapshot, request.stamp) ||
        request.replacements !== projectGeneration()) return;
    if (request.track !== null && (selection.current?.track !== track || !sameStamp(selection.current?.stamp, request.stamp))) return;
    const { sessionView, snapshot } = current;
    const spec = pointTarget(request.target);
    const tick = rulerPosition.current;
    const placement = request.starts.includes(start)
      ? automationPlacement(start, tick, snapshot.loopRegion, sessionView.timeSignature) : null;
    if (!spec || !placement) return;
    const batch = buildAutomationCreation(request.target, request.value, spec.name, placement, snapshot.tracks, track);
    if (!batch) return;
    const result = await model.mutate(request.stamp, batch, stillCurrent(sessionView));
    if (result.status === "applied" && projectGeneration() === request.replacements) {
      const clip = result.reply.createdIds.at(-1);
      if (clip !== undefined) selection.current?.selectCreated(result.reply.generation, clip);
    }
  };

  return <AutomationCreationContext.Provider value={{ open, setSelection, registerDeviceGesture, captureEditGuard }}>
    {children}
    {active && snapshot && createPortal(<CreationMenu request={active} tracks={snapshot.tracks}
      starts={active.starts} dismiss={dismiss} submit={(track, start) => { void submit(active, track, start); }} />, document.body)}
  </AutomationCreationContext.Provider>;
}

function CreationMenu({ request, tracks, starts, dismiss, submit }: {
  request: Invocation;
  tracks: readonly { id: number; name: string }[];
  starts: readonly StartChoice[];
  dismiss: () => void;
  submit: (track: TrackChoice, start: StartChoice) => void;
}) {
  const [choose, setChoose] = useState(false);
  const [track, setTrack] = useState<TrackChoice | null>(request.track);
  const [start, setStart] = useState<StartChoice | null>(starts.length === 1 && starts[0] === "song" ? null : starts[0]);
  const box = useRef<HTMLDivElement>(null);
  const action = useRef<HTMLButtonElement>(null);
  const choices = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const dismissOutside = (event: Event) => {
      if (event.target instanceof Node && !box.current?.contains(event.target)) dismiss();
    };
    const observer = new MutationObserver(() => {
      if (modalIsOpen() || !request.anchor.isConnected) dismiss();
    });
    observer.observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["aria-modal"] });
    window.addEventListener("pointerdown", dismissOutside, true);
    window.addEventListener("wheel", dismissOutside, { capture: true, passive: true });
    window.addEventListener("blur", dismiss);
    const offTransition = registerGestureCancellation(dismiss);
    const offReplacement = onProjectReplacing(dismiss);
    return () => {
      observer.disconnect();
      window.removeEventListener("pointerdown", dismissOutside, true);
      window.removeEventListener("wheel", dismissOutside, true);
      window.removeEventListener("blur", dismiss);
      offTransition();
      offReplacement();
    };
  }, [dismiss, request.anchor]);

  useEffect(() => {
    if (choose) choices.current?.querySelector<HTMLElement>('button:not([aria-disabled="true"]):not([disabled])')?.focus({ preventScroll: true });
    else action.current?.focus({ preventScroll: true });
  }, [choose]);

  const close = () => {
    if (request.anchor.isConnected && !modalIsOpen() && !isProjectReplacing()) request.anchor.focus({ preventScroll: true });
    dismiss();
  };
  const accept = () => {
    if (track === null || start === null || !starts.includes(start)) return;
    close();
    submit(track, start);
  };
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close();
    } else if (event.key === "Tab" && !choose) {
      close();
    }
  };

  const at = {
    left: Math.max(8, Math.min(request.at.x, window.innerWidth - 288)),
    ...(window.innerHeight - request.at.y < (choose ? 220 : 48) ? { bottom: Math.max(8, window.innerHeight - request.at.y) } : { top: request.at.y }),
  };
  return <div ref={box} role={choose ? "dialog" : "menu"} aria-label={choose ? "Automation clip placement" : "Parameter actions"}
    className="menu row-menu automation-create" style={at} onKeyDown={onKeyDown}
    onBlur={event => { if (event.relatedTarget instanceof Node && !event.currentTarget.contains(event.relatedTarget)) dismiss(); }}>
    {!choose ? <button ref={action} role="menuitem" className="menu-item" type="button"
      onClick={() => { if (track !== null && starts.length === 1 && start !== null) accept(); else setChoose(true); }}>
      Create automation clip
    </button> : <div ref={choices} className="automation-create-choices">
      <ListPicker label="Automation track" value={track === null ? "" : String(track)} placeholder="Choose track"
        options={tracks.length ? tracks.map(item => ({ id: String(item.id), name: item.name })) : [{ id: "new", name: "Create Track 1" }]}
        disabled={request.track !== null}
        onCommit={id => setTrack(id === "new" ? id : Number(id))} />
      <ListPicker label="Automation start" value={start ?? ""} placeholder="Choose start"
        options={starts.map(id => ({ id, name: startNames[id] }))}
        onCommit={id => { if (id === "playhead" || id === "loop" || id === "song") setStart(id); }} />
      <div className="modal-actions">
        <button type="button" className="btn" onClick={close}>Cancel</button>
        <button type="button" className="btn" disabled={track === null || start === null || !starts.includes(start)} onClick={accept}>Create clip</button>
      </div>
    </div>}
  </div>;
}
