import type { ArrangementStamp } from "../api/arrangement";
import type { MixerEdit, MixerEditReply, MixerInsertDetail, MixerInsertReply, MixerInsertTarget, MixerSnapshot } from "../api/mixer";
import { errorText, type ProjectView } from "../api/project";
import type { ProjectContextValue, ProjectStamp } from "../project/context";
import { bindMixerSnapshot, type MixerBinding } from "./mixerBinding";
import { targetKey, type MixerCommand, type MixerError, type MixerTrack, type ParamTarget } from "./mixerModel";

type OpenInsert = Readonly<{ nodeId: string; slotId: string; detail: MixerInsertDetail | null }>;
export type MixerInsertAction =
  | { kind: "setInsert"; params: readonly number[] }
  | { kind: "applyPreset"; preset: string };

type MixerHostState = Readonly<{
  binding: MixerBinding | null;
  selectedId: string | null;
  pending: boolean;
  error: MixerError | null;
  insert: OpenInsert | null;
}>;

type MixerHostAdapter = Readonly<{
  stamp: () => ProjectStamp;
  blocked: () => boolean;
  replacing: () => boolean;
  runFor: ProjectContextValue["runFor"];
  readProject: () => Promise<ProjectView>;
  readMixer: () => Promise<MixerSnapshot>;
  readInsert: (target: MixerInsertTarget) => Promise<MixerInsertReply>;
  edit: (stamp: ArrangementStamp, edit: MixerEdit) => Promise<MixerEditReply>;
  clearClip?: (nodeId: string) => void;
}>;

const EMPTY: MixerHostState = { binding: null, selectedId: null, pending: false, error: null, insert: null };

function sameStamp(a: ArrangementStamp | undefined, b: ArrangementStamp): boolean {
  return a?.generation === b.generation && a.revision === b.revision;
}

/** Lock the controlled strips while a write settles, preserving their lazy route getters. */
export function mixerHostControls(binding: MixerBinding, pending: boolean): MixerBinding["props"] {
  if (!pending) return binding.props;
  const lock = <Track extends MixerTrack>(track: Track): Track => Object.create(
    Object.getPrototypeOf(track),
    { ...Object.getOwnPropertyDescriptors(track), pending: { value: true, enumerable: true } },
  );
  return { ...binding.props, tracks: binding.props.tracks.map(lock), master: lock(binding.props.master) };
}

/** Owns mixer UI request identities. The project owns the only mutation lane. */
export function createMixerHost(initialAdapter: MixerHostAdapter) {
  let adapter = initialAdapter;
  let state = EMPTY;
  let active = false;
  let epoch = 0;
  let observed: ProjectView | null = null;
  let ownView: ProjectView | null = null;
  let read: object | null = null;
  let readAgain = false;
  const subscribers = new Set<() => void>();
  const gestures = new Map<string, { target: ParamTarget; stamp: ArrangementStamp | null }>();

  function update(fields: Partial<MixerHostState>) {
    state = { ...state, ...fields };
    for (const notify of subscribers) notify();
  }

  function still(origin: ProjectStamp, atEpoch: number): boolean {
    if (!active || epoch !== atEpoch || adapter.replacing()) return false;
    const current = adapter.stamp();
    return origin.generation === current.generation && origin.replacements === current.replacements;
  }

  function invalidateGestures() {
    for (const gesture of gestures.values()) gesture.stamp = null;
  }

  function reset() {
    epoch++;
    observed = ownView = null;
    read = null;
    readAgain = false;
  }

  function retire() {
    reset();
    invalidateGestures();
    if (active && state !== EMPTY) {
      state = EMPTY;
      for (const notify of subscribers) notify();
    }
  }

  async function openChecked(owner: OpenInsert, target: MixerInsertTarget) {
    const origin = adapter.stamp();
    const atEpoch = epoch;
    const current = () => still(origin, atEpoch) && state.insert === owner && sameStamp(state.binding?.stamp, target);
    if (!current() || origin.generation !== target.generation) return;
    try {
      const reply = await adapter.readInsert(target);
      if (!current()) return;
      if (reply.status === "applied") update({ insert: { ...owner, detail: reply.detail } });
      else {
        update({ insert: null, error: { nodeId: owner.nodeId, message: reply.message } });
        publish(reply.state, origin.generation);
      }
    } catch (error) {
      if (current()) update({ insert: null, error: { nodeId: owner.nodeId, message: errorText(error) } });
    }
  }

  function publish(snapshot: MixerSnapshot, generation: number, error = state.error): boolean {
    const before = state.binding;
    if (snapshot.generation !== generation || (before && snapshot.revision < before.stamp.revision)) return false;
    if (sameStamp(before?.stamp, snapshot)) {
      if (error !== state.error) update({ error });
      return true;
    }
    const binding = bindMixerSnapshot(snapshot);
    invalidateGestures();
    for (const [key, gesture] of gestures) {
      if (!binding.ownsParamTarget(gesture.target)) gestures.delete(key);
    }
    const previousInsert = state.insert;
    const reopened = previousInsert && binding.translate({
      type: "openInsert", nodeId: previousInsert.nodeId, slotId: previousInsert.slotId,
    });
    const insert = previousInsert && reopened?.kind === "openInsert" ? { ...previousInsert, detail: null } : null;
    const selectedId = state.selectedId && binding.resolveTrack(state.selectedId) !== undefined
      ? state.selectedId : binding.props.master.id;
    update({ binding, selectedId, insert, error });
    if (insert && reopened?.kind === "openInsert") void openChecked(insert, reopened.target);
    return true;
  }

  async function refresh(): Promise<void> {
    if (!active || !observed || adapter.replacing()) return;
    if (read) { readAgain = true; return; }
    const identity = {};
    read = identity;
    const origin = adapter.stamp();
    const atEpoch = epoch;
    const before = state.binding;
    if (!before && state.error) update({ error: null });
    try {
      const snapshot = await adapter.readMixer();
      if (read === identity && !readAgain && still(origin, atEpoch)) {
        publish(snapshot, origin.generation, state.error?.nodeId === null ? null : state.error);
      }
    } catch (error) {
      if (read === identity && !readAgain && still(origin, atEpoch) && state.binding === before) {
        update({ error: { nodeId: null, message: errorText(error) } });
      }
    } finally {
      if (read === identity) {
        read = null;
        if (readAgain) { readAgain = false; void refresh(); }
      }
    }
  }

  function observe(view: ProjectView | null) {
    if (!active || observed === view) return;
    if (observed && observed.generation !== view?.generation) retire();
    observed = view;
    const synced = ownView === view;
    ownView = null;
    if (!synced) {
      // ProjectView has no arrangement revision. A peer view may have advanced history.
      invalidateGestures();
      void refresh();
    }
  }

  async function submit(nodeId: string, base: ArrangementStamp, action: MixerEdit, owner?: OpenInsert): Promise<boolean> {
    if (state.pending || adapter.blocked() || !sameStamp(state.binding?.stamp, base)) return false;
    const origin = adapter.stamp();
    const atEpoch = epoch;
    const current = () => still(origin, atEpoch);
    if (!current() || origin.generation !== base.generation) return false;
    let accepted = false;
    update({ pending: true, error: null });
    try {
      const ran = await adapter.runFor(origin, async (generation) => {
        let reconciled = false;
        if (current() && generation === base.generation && sameStamp(state.binding?.stamp, base) && (!owner || state.insert === owner)) {
          try {
            const reply = await adapter.edit(base, action);
            const error = reply.status === "refused" ? { nodeId, message: reply.message } : null;
            reconciled = current() && publish(reply.state, base.generation, error);
            accepted = reconciled && reply.status === "applied";
          } catch (error) {
            if (current()) update({ error: { nodeId, message: errorText(error) } });
          }
        }
        // runFor requires a ProjectView result even for a local cancellation at the front of its lane.
        // Publish the fresh view through runFor once, including after a native refusal.
        try {
          const view = await adapter.readProject();
          if (current() && reconciled && view.generation === base.generation) ownView = view;
          return view;
        } catch (error) {
          if (current() && !state.error) update({ error: { nodeId, message: errorText(error) } });
          throw error;
        }
      });
      return ran && accepted && current();
    } catch (error) {
      if (current()) update({ error: { nodeId, message: errorText(error) } });
      return false;
    } finally {
      if (active && epoch === atEpoch) update({ pending: false });
    }
  }

  function command(command: MixerCommand): void | Promise<boolean> {
    const binding = state.binding;
    const blocked = !active || !binding || adapter.blocked() || adapter.stamp().generation !== binding.stamp.generation;
    if (command.type === "param") {
      const key = targetKey(command.target);
      if (command.phase === "live") {
        if (binding?.ownsParamTarget(command.target) && !gestures.has(key)) {
          gestures.set(key, { target: command.target, stamp: blocked || state.pending ? null : binding.stamp });
        }
        return;
      }
      const gesture = gestures.get(key);
      gestures.delete(key);
      if (command.phase === "cancel" || (gesture && (!gesture.stamp || !sameStamp(binding?.stamp, gesture.stamp)))) return;
    }
    if (blocked) return;
    const intent = binding.translate(command);
    const nodeId = command.type === "param" ? command.target.nodeId : command.nodeId;
    switch (intent.kind) {
      case "ignore": return;
      case "edit": return submit(nodeId, intent.stamp, intent.edit);
      case "select":
        if (state.selectedId !== intent.nodeId) update({ selectedId: intent.nodeId, insert: null, error: null });
        return;
      case "clearClip": adapter.clearClip?.(intent.nodeId); return;
      case "refused":
      case "unsupported":
        update({ error: { nodeId: intent.nodeId, message: intent.message } });
        return;
      case "openInsert":
        if (command.type === "openInsert") {
          const owner = { nodeId, slotId: command.slotId, detail: null };
          update({ selectedId: nodeId, insert: owner, error: null });
          void openChecked(owner, intent.target);
        }
    }
  }

  function applyInsert(owner: OpenInsert, action: MixerInsertAction): Promise<boolean> {
    if (state.insert !== owner || !owner.detail) return Promise.resolve(false);
    const { track, insert, generation, revision } = owner.detail;
    const edit: MixerEdit = action.kind === "setInsert"
      ? { kind: "setInsert", track, insert, params: [...action.params] }
      : { kind: "applyPreset", track, insert, preset: action.preset };
    return submit(owner.nodeId, { generation, revision }, edit, owner);
  }

  function assignSource(binding: MixerBinding, sourceId: string, destinationId: string): Promise<boolean> {
    if (state.binding !== binding) return Promise.resolve(false);
    const edit = binding.routing.assignmentEdit(sourceId, destinationId);
    return edit ? submit(destinationId, binding.stamp, edit) : Promise.resolve(false);
  }

  function addTrack(binding: MixerBinding): Promise<boolean> {
    if (state.binding !== binding) return Promise.resolve(false);
    return submit(binding.props.master.id, binding.stamp, {
      kind: "createTrack", name: binding.routing.nextTrackName, output: 0,
    });
  }

  return {
    connect(next: Pick<MixerHostAdapter, "stamp" | "runFor" | "clearClip">) { adapter = { ...adapter, ...next }; },
    start() { active = true; },
    stop() { active = false; reset(); gestures.clear(); state = EMPTY; },
    current: () => state,
    subscribe(notify: () => void) { subscribers.add(notify); return () => { subscribers.delete(notify); }; },
    observe, refresh, retire, invalidateGestures, command, applyInsert, assignSource, addTrack,
    closeInsert(owner?: OpenInsert) {
      if (state.insert && (!owner || state.insert === owner)) update({ insert: null });
    },
  };
}
