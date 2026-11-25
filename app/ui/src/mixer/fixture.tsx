// A labelled offline fixture for the mixer components: generated tracks, a synthetic meter
// snapshot, and a stand-in host that applies or refuses commands. No engine, IPC or audio; never
// part of the app's build (a non-production Vite input).
//
// Query: ?tracks=<n> generated tracks (0 for none), ?long=1 long names and chains.

import "@fontsource-variable/manrope";
import "../theme/riso.css";
import "../app.css";
import "../devices/devices.css";
import { StrictMode, useLayoutEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { Mixer } from "./Mixer";
import type { MeterSource, MixerCommand, MixerError, MixerInsert, MixerMaster, MixerOption, MixerTrack } from "./mixerModel";

const EFFECTS: MixerOption[] = [
  "EQ",
  "Compressor",
  "Limiter",
  "Clipper",
  "Utility",
  "Saturation",
  "Drum Shaper",
  "Fattener",
  "Bitcrusher",
  "Lo-fi Colour",
  "Character Delay",
  "Algorithmic Reverb",
  "Shimmer Reverb",
  "Multiband Compressor",
].map((name, i) => ({ id: `fx-${i}`, name }));
const DETECTOR = new Set(["Compressor", "Multiband Compressor"]);

interface State {
  tracks: MixerTrack[];
  master: MixerMaster;
  selectedId: string | null;
  error: MixerError | null;
  /** The next command is refused with this message. */
  refuse: string | null;
  opened: string | null;
}

let nextId = 1;
const uid = (p: string) => `${p}-${nextId++}`;

function insert(name: string, active = true): MixerInsert {
  return { id: uid("slot"), name, active };
}

function base(id: string, name: string, kind: string, ink: number, level: number, fx: string[]): MixerTrack {
  return {
    id,
    name,
    kind,
    ink,
    level,
    pan: 0,
    mute: false,
    solo: false,
    clipped: false,
    outputId: "n-master",
    outputs: [],
    inserts: fx.map((f) => insert(f)),
    sends: [],
    sendDestinations: [],
  };
}

/** Outputs and send destinations: the stand-in host's own rule (buses and returns), not a
 * route validator of the mixer's. */
function wire(tracks: MixerTrack[], master: MixerMaster): MixerTrack[] {
  const buses = tracks.filter((t) => t.kind === "Bus" || t.kind === "Return");
  const returns = tracks.filter((t) => t.kind === "Return");
  return tracks.map((t) => {
    const outputs = [...buses.filter((b) => b.id !== t.id && t.kind !== "Bus" && t.kind !== "Return"), master].map(({ id, name }) => ({ id, name }));
    const sendDestinations = t.kind === "Return" ? [] : returns.map(({ id, name }) => ({ id, name }));
    const inserts = t.inserts.map((s) => (DETECTOR.has(s.name) ? { ...s, detector: s.detector ?? { options: detectorSources(tracks, t.id), sourceId: null } } : s));
    return { ...t, outputs, sendDestinations, inserts };
  });
}

const detectorSources = (tracks: MixerTrack[], self: string) => tracks.filter((t) => t.id !== self && t.kind === "Instrument").map(({ id, name }) => ({ id, name }));

function initial(): State {
  const q = new URLSearchParams(location.search);
  const count = q.get("tracks");
  const long = q.get("long") === "1";
  const master: MixerMaster = { ...base("n-master", "Master", "Output", 1, 0, ["Limiter"]), outputId: null, outputLabel: "Audio device" };
  let tracks: MixerTrack[];
  if (count !== null) {
    const n = Math.max(0, Math.floor(Number(count)) || 0);
    tracks = Array.from({ length: n }, (_, i) => base(`n-${i + 1}`, `Track ${i + 1}`, "Instrument", (i % 6) + 1, -6 - (i % 7), i % 3 ? ["EQ"] : []));
  } else {
    tracks = [
      base("n-kick", "Kick", "Instrument", 1, -4, ["EQ", "Drum Shaper"]),
      base("n-snare", "Snare", "Instrument", 4, -7, ["EQ", "Saturation"]),
      base("n-hats", "Hats", "Instrument", 3, -12, ["EQ"]),
      base("n-bass", "Bass", "Instrument", 5, -5, ["Fattener", "Compressor"]),
      base("n-keys", "Keys", "Instrument", 2, -8, ["EQ", "Lo-fi Colour", "Character Delay"]),
      base("n-vocal", "Vocal take", "Audio track", 6, -6, []),
      base("n-drums", "Drum bus", "Bus", 4, 0, ["Multiband Compressor"]),
      base("n-air", "Air return", "Return", 2, -10, ["Algorithmic Reverb"]),
      base("n-echo", "Echo return", "Return", 6, -13, ["Character Delay"]),
    ];
    for (const id of ["n-kick", "n-snare", "n-hats"]) tracks.find((t) => t.id === id)!.outputId = "n-drums";
    const kick = tracks[0];
    kick.sends = [{ id: uid("send"), destinationId: "n-air", amount: 0.05, pre: false }];
    tracks[1].sends = [{ id: uid("send"), destinationId: "n-air", amount: 0.23, pre: false }];
    tracks[4].sends = [{ id: uid("send"), destinationId: "n-echo", amount: 0.3, pre: true }];
    tracks[1].clipped = true;
    tracks[4].inserts = tracks[4].inserts.map((x, i) => (i === 1 ? { ...x, active: false } : x));
    if (long) {
      tracks[0].name = "Kick drum layered with a long sub tail, take three (final)";
      tracks[0].inserts = ["EQ", "Drum Shaper", "Saturation", "Compressor", "Clipper", "Utility", "Character Delay"].map((n) => insert(n));
      tracks[7].name = "Air return with a very long plate-style reverb name";
    }
  }
  return { tracks: wire(tracks, master), master, selectedId: tracks.find((t) => t.kind === "Bus")?.id ?? tracks[0]?.id ?? master.id, error: null, refuse: null, opened: null };
}

function update(state: State, id: string, f: (t: MixerTrack) => MixerTrack): State {
  if (id === state.master.id) return { ...state, master: f(state.master) as MixerMaster };
  return { ...state, tracks: state.tracks.map((t) => (t.id === id ? f(t) : t)) };
}

const node = (state: State, id: string) => (id === state.master.id ? state.master : state.tracks.find((t) => t.id === id));

/** The stand-in host: applies one command, or ignores one for a target that is gone. */
function apply(state: State, c: MixerCommand): State {
  if (c.type === "select") return { ...state, selectedId: c.nodeId };
  const n = "nodeId" in c ? node(state, c.nodeId) : undefined;
  if (c.type === "param") {
    const target = node(state, c.target.nodeId);
    if (!target || c.phase !== "commit") return state; // live values would go to the engine
    const t = c.target;
    if (t.kind === "send") return update(state, t.nodeId, (x) => ({ ...x, sends: x.sends.map((s) => (s.id === t.sendId ? { ...s, amount: c.value } : s)) }));
    return update(state, t.nodeId, (x) => ({ ...x, [t.kind]: c.value, clipped: x.clipped || (t.kind === "level" && c.value > 0) }));
  }
  if (!n) return state;
  const id = n.id;
  switch (c.type) {
    case "mute":
      return update(state, id, (x) => ({ ...x, mute: c.on }));
    case "solo":
      return update(state, id, (x) => ({ ...x, solo: c.on }));
    case "clearClip":
      return update(state, id, (x) => ({ ...x, clipped: false }));
    case "setOutput":
      return update(state, id, (x) => ({ ...x, outputId: c.outputId }));
    case "addInsert": {
      const fx = EFFECTS.find((e) => e.id === c.effectId);
      if (!fx) return state;
      const slot: MixerInsert = DETECTOR.has(fx.name) ? { ...insert(fx.name), detector: { options: detectorSources(state.tracks, id), sourceId: null } } : insert(fx.name);
      return update(state, id, (x) => ({ ...x, inserts: [...x.inserts, slot] }));
    }
    case "openInsert": {
      const slot = n.inserts.find((s) => s.id === c.slotId);
      return slot ? { ...state, opened: `${slot.name} on ${n.name}` } : state;
    }
    case "setInsertActive":
      return update(state, id, (x) => ({ ...x, inserts: x.inserts.map((s) => (s.id === c.slotId ? { ...s, active: c.active } : s)) }));
    case "moveInsert":
      return update(state, id, (x) => {
        const slot = x.inserts.find((s) => s.id === c.slotId);
        if (!slot) return x;
        const rest = x.inserts.filter((s) => s.id !== c.slotId);
        const at = c.beforeSlotId === null ? rest.length : rest.findIndex((s) => s.id === c.beforeSlotId);
        if (at < 0) return x;
        return { ...x, inserts: [...rest.slice(0, at), slot, ...rest.slice(at)] };
      });
    case "removeInsert":
      return update(state, id, (x) => ({ ...x, inserts: x.inserts.filter((s) => s.id !== c.slotId) }));
    case "setDetector":
      return update(state, id, (x) => ({ ...x, inserts: x.inserts.map((s) => (s.id === c.slotId && s.detector ? { ...s, detector: { ...s.detector, sourceId: c.sourceId } } : s)) }));
    case "addSend":
      return update(state, id, (x) => ({ ...x, sends: [...x.sends, { id: uid("send"), destinationId: c.destinationId, amount: c.amount, pre: c.pre }] }));
    case "setSendPre":
      return update(state, id, (x) => ({ ...x, sends: x.sends.map((s) => (s.id === c.sendId ? { ...s, pre: c.pre } : s)) }));
    case "removeSend":
      return update(state, id, (x) => ({ ...x, sends: x.sends.filter((s) => s.id !== c.sendId) }));
  }
}

/** A still, synthetic meter snapshot derived from each generated level: not a signal. */
function snapshot(get: () => State | null): MeterSource {
  return {
    read(id, out) {
      const s = get();
      const t = s && node(s, id);
      if (!t || t.mute) return false;
      const peak = Math.pow(10, (t.level - 4) / 20);
      out[0] = t.clipped ? 1 : peak * (1 - Math.max(0, t.pan) * 0.5);
      out[1] = t.clipped ? 1 : peak * (1 + Math.min(0, t.pan) * 0.5);
      return true;
    },
  };
}

declare global {
  interface Window {
    /** The fixture's host, for the Playwright spec. */
    mixerFixture: {
      log: MixerCommand[];
      refuseNext(message: string): void;
      setPending(id: string, pending: boolean): void;
      remove(id: string): void;
      replace(id: string, newId: string): void;
      removeSend(id: string, sendId: string): void;
      rename(id: string, name: string): void;
    };
  }
}

/** Every command the mixer emitted, in order. */
const LOG: MixerCommand[] = [];

/** The state the meter snapshot reads. */
let shownState: State | null = null;
const METERS = snapshot(() => shownState);

export function Fixture() {
  const [state, setState] = useState(initial);
  useLayoutEffect(() => {
    shownState = state;
  });

  useLayoutEffect(() => {
    window.mixerFixture = {
      log: LOG,
      refuseNext: (message) => setState((s) => ({ ...s, refuse: message })),
      setPending: (id, pending) => setState((s) => update(s, id, (t) => ({ ...t, pending }))),
      remove: (id) => setState((s) => ({ ...s, tracks: s.tracks.filter((t) => t.id !== id) })),
      replace: (id, newId) => setState((s) => ({ ...s, tracks: s.tracks.map((t) => (t.id === id ? { ...t, id: newId, sends: t.sends.map((x) => ({ ...x, id: uid("send") })) } : t)) })),
      removeSend: (id, sendId) => setState((s) => update(s, id, (t) => ({ ...t, sends: t.sends.filter((x) => x.id !== sendId) }))),
      rename: (id, name) => setState((s) => update(s, id, (t) => ({ ...t, name }))),
    };
  }, []);

  const onCommand = (c: MixerCommand) => {
    window.mixerFixture.log.push(c);
    setState((s) => {
      const live = c.type === "param" && c.phase !== "commit";
      if (s.refuse && !live && c.type !== "select") {
        return { ...s, refuse: null, error: { nodeId: "nodeId" in c ? c.nodeId : c.target.nodeId, message: s.refuse } };
      }
      const next = apply(s, c);
      return live || next === s ? next : { ...next, error: null };
    });
  };

  return (
    <main className="fx">
      <header className="fx-head">
        <div>
          <h1>Mixer components</h1>
          <p>Synthetic fixture: generated tracks, a still synthetic meter snapshot, no audio and no engine. A stand-in host applies each command.</p>
        </div>
        <p className="fx-count">
          {state.tracks.length} tracks{state.opened ? ` · Opened ${state.opened}` : ""}
        </p>
      </header>
      <Mixer tracks={state.tracks} master={state.master} effects={EFFECTS} selectedId={state.selectedId} error={state.error} meters={METERS} onCommand={onCommand} />
    </main>
  );
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Fixture />
  </StrictMode>,
);
