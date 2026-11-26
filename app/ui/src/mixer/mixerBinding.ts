import type { ArrangementStamp } from "../api/arrangement";
import type { MixerEdit, MixerInsertTarget, MixerSend as WireSend, MixerSnapshot, MixerSourceKey, MixerTrack as WireTrack } from "../api/mixer";
import { LEVEL, unsupportedReason, type MixerCommand, type MixerMaster, type MixerOption, type MixerProps, type MixerTrack, type ParamTarget } from "./mixerModel";

export type MixerIntent =
  | { kind: "edit"; stamp: ArrangementStamp; edit: MixerEdit }
  | { kind: "select"; nodeId: string }
  | { kind: "clearClip"; nodeId: string }
  | { kind: "openInsert"; target: MixerInsertTarget }
  | { kind: "refused"; nodeId: string; message: string }
  | { kind: "unsupported"; nodeId: string; message: string }
  | { kind: "ignore" };

type SourceOption = Readonly<MixerOption & { destinationId: string }>;

export type MixerBinding = Readonly<{
  props: Pick<MixerProps, "tracks" | "master" | "effects" | "unsupported">;
  stamp: ArrangementStamp;
  resolveTrack: (nodeId: string) => number | undefined;
  ownsParamTarget: (target: ParamTarget) => boolean;
  translate: (command: MixerCommand) => MixerIntent;
  routing: Readonly<{
    sources: readonly SourceOption[];
    destinations: readonly MixerOption[];
    nextTrackName: string;
    assignmentEdit: (sourceId: string, destinationId: string) => Extract<MixerEdit, { kind: "assignSource" }> | undefined;
  }>;
}>;

type RouteChoices = Pick<MixerTrack, "outputs" | "sendDestinations">;
const NO_ROUTES: RouteChoices = { outputs: [], sendDestinations: [] };
const MASTER_ID = 0;
// Keep a window and its pinned strips warm without retaining a row for every strip scrolled past.
const ROUTE_CACHE_ROWS = 32;
const EFFECT_LABELS = new Map([
  ["eq", "EQ"],
  ["comp", "Compressor"],
  ["ott", "Multiband Compressor"],
  ["lofi_colour", "Lo-fi Colour"],
]);

function effectLabel(id: string): string {
  return EFFECT_LABELS.get(id) ?? id.split(/[_-]+/).filter(Boolean)
    .map((word) => word.charAt(0).toUpperCase() + word.slice(1)).join(" ");
}

/** Binds one validated snapshot. Fields on individual strips resolve route options on access. */
export function bindMixerSnapshot(snapshot: MixerSnapshot): MixerBinding {
  const { generation, revision } = snapshot;
  const stamp: ArrangementStamp = { generation, revision };
  const trackKey = (id: number) => `${generation}:track:${id}`;
  const slotKey = (id: number) => `${generation}:insert:${id}`;
  const sendKey = (id: number) => `${generation}:send:${id}`;
  const sourceKey = (source: MixerSourceKey) => source.channel === undefined
    ? `${generation}:audio-source:${source.audioTrack}`
    : `${generation}:instrument-source:${source.channel}`;

  const tracksByNode = new Map<string, WireTrack>();
  const trackChoices = new Map<number, MixerOption>();
  const slotsById = new Map<string, { track: number; insert: number; index: number }>();
  for (const track of snapshot.mixer.tracks) {
    const id = trackKey(track.id);
    tracksByNode.set(id, track);
    trackChoices.set(track.id, { id, name: track.name });
    track.inserts.forEach(({ id }, index) => slotsById.set(slotKey(id), { track: track.id, insert: id, index }));
  }

  const sendsById = new Map<string, WireSend>();
  const sendsBySource = new Map<number, WireSend[]>();
  for (const send of snapshot.mixer.sends) {
    sendsById.set(sendKey(send.id), send);
    const group = sendsBySource.get(send.from);
    if (group) group.push(send);
    else sendsBySource.set(send.from, [send]);
  }

  const trackKinds = new Map<number, "Instrument" | "Audio" | "Track">();
  const assignments = new Map<string, number>();
  for (const { source, destination } of snapshot.mixer.sources) {
    assignments.set(sourceKey(source), destination);
    const kind = source.channel === undefined ? "Audio" : "Instrument";
    const current = trackKinds.get(destination);
    trackKinds.set(destination, current === undefined || current === kind ? kind : "Track");
  }
  const sourceKeys = new Map<string, MixerSourceKey>();
  const sources = snapshot.sources.map(({ source, name }): SourceOption => {
    const id = sourceKey(source);
    sourceKeys.set(id, source);
    const kind = source.channel === undefined ? "Audio track" : "Instrument channel";
    return {
      id,
      name: name ? `${name} · ${kind.toLowerCase()}` : `${kind} ${source.channel ?? source.audioTrack}`,
      destinationId: trackKey(assignments.get(id) ?? MASTER_ID),
    };
  });

  const effects = snapshot.effects.map((id) => ({ id, name: effectLabel(id) }));
  const effectNames = new Map(effects.map(({ id, name }) => [id, name]));
  const routes = snapshot.routeInputs && new Map(snapshot.routeInputs.map((row) => [row.track, row]));
  const routesCache = new Map<number, RouteChoices>();

  function routeChoices(track: WireTrack): RouteChoices {
    if (track.id === MASTER_ID || routes === null) return NO_ROUTES;
    const cached = routesCache.get(track.id);
    if (cached) {
      routesCache.delete(track.id);
      routesCache.set(track.id, cached);
      return cached;
    }

    // If a destination can already reach this source, adding the proposed edge closes a cycle.
    const blocked = new Set([track.id]);
    const pending = [track.id];
    for (let id = pending.pop(); id !== undefined; id = pending.pop()) {
      const row = routes.get(id);
      if (!row) return NO_ROUTES;
      for (const predecessor of row.from) {
        if (blocked.has(predecessor)) continue;
        blocked.add(predecessor);
        pending.push(predecessor);
      }
    }

    const usedSends = new Set(sendsBySource.get(track.id)?.map(({ to }) => to));
    const outputs: MixerOption[] = [];
    const sendDestinations: MixerOption[] = [];
    for (const [id, choice] of trackChoices) {
      if (blocked.has(id)) continue;
      outputs.push(choice);
      if (id !== track.output && !usedSends.has(id)) sendDestinations.push(choice);
    }
    const choices = { outputs, sendDestinations };
    routesCache.set(track.id, choices);
    if (routesCache.size > ROUTE_CACHE_ROWS) {
      const oldest = routesCache.keys().next().value;
      if (oldest !== undefined) routesCache.delete(oldest);
    }
    return choices;
  }

  function projectTrack(track: WireTrack): MixerTrack {
    return {
      id: trackKey(track.id),
      name: track.name,
      kind: track.id === MASTER_ID ? "Master" : (trackKinds.get(track.id) ?? "Track"),
      ink: (track.id % 6) + 1,
      level: track.level === "silence" ? LEVEL.min : track.level.db,
      pan: track.pan,
      mute: track.mute,
      solo: track.solo,
      clipped: false,
      outputId: track.output === null ? null : trackKey(track.output),
      inserts: track.inserts.map((insert) => ({
        id: slotKey(insert.id),
        name: effectNames.get(insert.device) ?? effectLabel(insert.device),
        active: !insert.bypass,
      })),
      sends: (sendsBySource.get(track.id) ?? []).map((send) => ({
        id: sendKey(send.id),
        destinationId: trackKey(send.to),
        amount: send.gain,
        pre: send.tap === "pre",
      })),
      get outputs() { return routeChoices(track).outputs; },
      get sendDestinations() { return routeChoices(track).sendDestinations; },
    };
  }

  function translate(command: MixerCommand): MixerIntent {
    if (command.type === "param" && command.phase !== "commit") return { kind: "ignore" };
    const nodeId = command.type === "param" ? command.target.nodeId : command.nodeId;
    const track = tracksByNode.get(nodeId);
    const refused = (message: string): MixerIntent => ({ kind: "refused", nodeId, message });
    if (!track) return refused("This mixer track is no longer available.");
    const edit = (edit: MixerEdit): MixerIntent => ({ kind: "edit", stamp, edit });
    const unavailable = (): MixerIntent => ({
      kind: "unsupported",
      nodeId,
      message: unsupportedReason(snapshot.unsupported, command.type) ?? "This mixer control is unavailable.",
    });

    switch (command.type) {
      case "select":
      case "clearClip":
        return { kind: command.type, nodeId };
      case "mute":
      case "solo":
        return unavailable();
      case "param": {
        const { target, value } = command;
        if (!Number.isFinite(value)) return refused("The mixer value must be finite.");
        if (target.kind === "send") {
          const send = sendsById.get(target.sendId);
          if (send?.from !== track.id) return refused("This send is no longer on the mixer track.");
          return edit({ kind: "setSend", from: track.id, send: send.id, gain: value });
        }
        if (target.kind === "pan") return edit({ kind: "setFader", track: track.id, pan: value });
        return edit({ kind: "setFader", track: track.id, level: value === LEVEL.min ? "silence" : { db: value } });
      }
      case "setOutput":
      case "addSend": {
        const targetId = command.type === "setOutput" ? command.outputId : command.destinationId;
        const target = tracksByNode.get(targetId);
        if (!target) return refused("This route destination is no longer available.");
        if (routes === null) return refused("Mixer routing choices are unavailable.");
        const choices = routeChoices(track);
        const allowed = command.type === "setOutput" ? choices.outputs : choices.sendDestinations;
        if (!allowed.some(({ id }) => id === targetId)) return refused("This mixer track cannot route to that destination.");
        if (command.type === "setOutput") return edit({ kind: "setOutput", track: track.id, output: target.id });
        return edit({ kind: "addSend", from: track.id, to: target.id, gain: command.amount, tap: "post" });
      }
      case "setSendPre":
      case "removeSend": {
        const send = sendsById.get(command.sendId);
        if (send?.from !== track.id) return refused("This send is no longer on the mixer track.");
        if (command.type === "removeSend") return edit({ kind: "removeSend", from: track.id, send: send.id });
        return edit({ kind: "setSend", from: track.id, send: send.id, tap: command.pre ? "pre" : "post" });
      }
      case "addInsert":
        if (!effectNames.has(command.effectId)) return refused("This mixer effect is unavailable.");
        return edit({ kind: "addInsert", track: track.id, index: track.inserts.length, effectId: command.effectId });
      case "openInsert":
      case "removeInsert":
      case "setInsertActive":
      case "setDetector":
      case "moveInsert": {
        const slot = slotsById.get(command.slotId);
        if (slot?.track !== track.id) return refused("This insert is no longer on the mixer track.");
        if (command.type === "openInsert") return { kind: "openInsert", target: { ...stamp, track: track.id, insert: slot.insert } };
        if (command.type === "removeInsert") return edit({ kind: "removeInsert", track: track.id, insert: slot.insert });
        if (command.type === "setInsertActive" || command.type === "setDetector") return unavailable();

        let index = track.inserts.length - 1;
        if (command.beforeSlotId !== null) {
          const before = slotsById.get(command.beforeSlotId);
          if (before?.track !== track.id) return refused("The target insert is no longer on the mixer track.");
          index = before.index - Number(slot.index < before.index);
        }
        if (index === slot.index) return { kind: "ignore" };
        return edit({ kind: "moveInsert", track: track.id, insert: slot.insert, index });
      }
    }
  }

  const nativeMaster = tracksByNode.get(trackKey(MASTER_ID));
  if (!nativeMaster) throw new Error("The mixer snapshot has no Master track.");
  const master: MixerMaster = { ...projectTrack(nativeMaster), outputLabel: "Audio device" };
  return {
    props: {
      master,
      tracks: snapshot.mixer.tracks.filter(({ id }) => id !== MASTER_ID).map(projectTrack),
      effects,
      unsupported: snapshot.unsupported,
    },
    stamp,
    resolveTrack: (id) => tracksByNode.get(id)?.id,
    ownsParamTarget: (target) => {
      const track = tracksByNode.get(target.nodeId);
      return track !== undefined && (target.kind !== "send" || sendsById.get(target.sendId)?.from === track.id);
    },
    translate,
    routing: {
      sources,
      destinations: [...trackChoices.values()],
      nextTrackName: `Track ${snapshot.mixer.nextTrackId}`,
      assignmentEdit(sourceId, destinationId) {
        const source = sourceKeys.get(sourceId);
        const destination = tracksByNode.get(destinationId);
        if (source && destination) return { kind: "assignSource", source, destination: destination.id };
      },
    },
  };
}
