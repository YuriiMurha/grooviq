// A labelled offline fixture for the mixer components: generated tracks, a synthetic meter
// stream, and a stand-in host that applies or refuses commands. No engine, IPC or audio; never
// part of the app's build (a non-production Vite input).
//
// Query: ?tracks=<n> generated tracks (0 for none), ?long=1 long names and chains,
// ?offscreen-send=1 makes the last generated track a send destination.

import "@fontsource-variable/manrope";
import "../theme/styles";
import "../app.css";
import "../devices/devices.css";
import { Profiler, StrictMode, useLayoutEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/profiling";
import { initializeTheme } from "../theme/bootstrap";
import { waitForStartupStyles } from "../theme/waitForStartupStyles";
import { Mixer } from "./Mixer";
import { StripProfiler } from "./stripProfiler";
import type { MeterSource, MixerCommand, MixerError, MixerInsert, MixerMaster, MixerOption, MixerProps, MixerTrack } from "./mixerModel";

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

/** Stand-in host route rules. Even a silent send participates in cycle detection. */
function wire(tracks: MixerTrack[], master: MixerMaster): MixerTrack[] {
  const nodes = new Map(tracks.map((track) => [track.id, track]));
  const buses = tracks.filter((track) => track.kind === "Bus");
  const returns = tracks.filter((track) => track.kind === "Return");
  const reaches = (from: string, to: string): boolean => {
    const seen = new Set<string>();
    const visit = (id: string): boolean => {
      if (id === to) return true;
      const track = nodes.get(id);
      if (!track || seen.has(id)) return false;
      seen.add(id);
      return (track.outputId !== null && visit(track.outputId)) || track.sends.some((send) => visit(send.destinationId));
    };
    return visit(from);
  };
  const option = ({ id, name }: MixerTrack) => ({ id, name });

  return tracks.map((track) => {
    const candidates = track.kind === "Return" ? [master] : [...buses, master];
    const outputs = candidates.filter((target) => !reaches(target.id, track.id)).map(option);
    const sendDestinations = returns.filter((target) => !reaches(target.id, track.id)).map(option);
    const inserts = track.inserts.map((slot) => DETECTOR.has(slot.name) && !slot.detector
      ? { ...slot, detector: { options: detectorSources(tracks, track.id), sourceId: null } }
      : slot);
    return { ...track, outputs, sendDestinations, inserts };
  });
}

const detectorSources = (tracks: MixerTrack[], self: string) => tracks.filter((t) => t.id !== self && t.kind === "Instrument").map(({ id, name }) => ({ id, name }));

const q = new URLSearchParams(location.search);
const UNSUPPORTED: MixerProps["unsupported"] = q.has("unsupported") ? {
  mute: "Mute is unavailable in this mixer host.",
  solo: "Solo is unavailable in this mixer host.",
  bypass: "Bypass is unavailable in this mixer host.",
  detector: "Detector selection is unavailable in this mixer host.",
} : undefined;

function initial(): State {
  const count = q.get("tracks");
  const long = q.get("long") === "1";
  const master: MixerMaster = { ...base("n-master", "Master", "Output", 1, 0, ["Limiter"]), outputId: null, outputLabel: "Stereo out" };
  let tracks: MixerTrack[];
  if (count !== null) {
    const n = Math.max(0, Math.floor(Number(count)) || 0);
    tracks = Array.from({ length: n }, (_, i) => base(`n-${i + 1}`, `Track ${i + 1}`, "Instrument", (i % 6) + 1, -6 - (i % 7), i % 3 ? ["EQ"] : []));
    if (tracks.length && q.has("offscreen-send")) tracks[tracks.length - 1].kind = "Return";
  } else {
    tracks = [
      base("n-kick", "Kick", "Instrument", 1, -4, ["EQ", "Drum Shaper"]),
      base("n-snare", "Snare", "Instrument", 4, -8, UNSUPPORTED ? ["EQ", "Compressor"] : ["EQ"]),
      base("n-drums", "Kit bus", "Bus", 4, -2, ["Multiband Compressor"]),
      base("n-bass", "Bass", "Instrument", 5, -5, ["Fattener", "Compressor"]),
      base("n-keys", "Keys", "Instrument", 2, -9, ["EQ", "Lo-fi Colour", "Character Delay"]),
      base("n-vocal", "Vocal take", "Audio", 6, -6, []),
      base("n-music", "Music bus", "Bus", 3, -1, ["Utility"]),
      base("n-air", "Room", "Return", 2, -10, ["Algorithmic Reverb"]),
      base("n-echo", "Echo", "Return", 6, -12, ["Character Delay"]),
    ];
    tracks[0].outputId = tracks[1].outputId = "n-drums";
    tracks[2].outputId = tracks[3].outputId = tracks[4].outputId = "n-music";
    tracks[1].sends = [{ id: uid("send"), destinationId: "n-air", amount: 0.16, pre: false }];
    tracks[2].sends = [{ id: uid("send"), destinationId: "n-air", amount: 0.12, pre: false }];
    tracks[4].sends = [{ id: uid("send"), destinationId: "n-air", amount: 0.24, pre: false }];
    tracks[5].sends = [{ id: uid("send"), destinationId: "n-echo", amount: 0.19, pre: false }];
    tracks[1].clipped = true;
    if (long) {
      tracks[0].name = "Kick drum layered with a long sub tail, take three (final)";
      tracks[0].inserts = ["EQ", "Drum Shaper", "Saturation", "Compressor", "Clipper", "Utility", "Character Delay"].map((n) => insert(n));
      tracks[7].name = "Air return with a very long plate-style reverb name";
    }
  }
  return { tracks: wire(tracks, master), master, selectedId: tracks.find((t) => t.name === "Snare")?.id ?? tracks[0]?.id ?? master.id, error: null, refuse: null, opened: null };
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

const measure = q.has("measure");
let meterFrames = 0;
let mixerCommits = 0;

/** Synthetic stereo peaks sampled on demand. No signal or audio device is created. */
function meterSource(get: () => State | null): MeterSource {
  return {
    read(id, out) {
      const state = get();
      // Master is registered for every frame, regardless of the windowed strip count.
      if (measure && state?.master.id === id) meterFrames++;
      const track = state && node(state, id);
      if (!track || track.mute) return false;
      const time = q.has("still") ? 0 : performance.now() / 450;
      const peak = Math.pow(10, (track.level - 5) / 20);
      out[0] = peak * (0.75 + 0.17 * Math.sin(time + track.level)) * (1 - Math.max(0, track.pan) * 0.5);
      out[1] = peak * (0.65 + 0.18 * Math.cos(time + track.level)) * (1 + Math.min(0, track.pan) * 0.5);
      return true;
    },
  };
}

declare global {
  interface Window {
    /** The fixture's host, for the Playwright spec. */
    mixerFixture: {
      log: MixerCommand[];
      stats(): { meterFrames: number; mixerCommits: number; stripCommits: Record<string, number> };
      refuseNext(message: string): void;
      deferNextCommit(): void;
      settleCommit(accepted: boolean): void;
      setPending(id: string, pending: boolean): void;
      offerSend(id: string, destination: MixerOption): void;
      remove(id: string): void;
      replace(id: string, newId: string): void;
      removeSend(id: string, sendId: string): void;
      rename(id: string, name: string): void;
    };
  }
}

/** Every command the mixer emitted, in order. */
const LOG: MixerCommand[] = [];
const stripCommits = new Map<string, number>();
const countStripCommit = (id: string) => stripCommits.set(id, (stripCommits.get(id) ?? 0) + 1);

/** The state the meter source reads. */
let shownState: State | null = null;
const METERS = meterSource(() => shownState);

export function Fixture() {
  const [state, setState] = useState(initial);
  const deferNext = useRef(false);
  const deferred = useRef<{ command: Extract<MixerCommand, { type: "param" | "addSend" }>; resolve: () => void } | null>(null);
  const afterRender = useRef<(() => void) | null>(null);
  useLayoutEffect(() => {
    shownState = state;
    // Settle only after the controlled child received the host's authoritative props.
    const resolve = afterRender.current;
    afterRender.current = null;
    resolve?.();
  });

  useLayoutEffect(() => {
    window.mixerFixture = {
      log: LOG,
      stats: () => ({ meterFrames, mixerCommits, stripCommits: Object.fromEntries(stripCommits) }),
      refuseNext: (message) => setState((s) => ({ ...s, refuse: message })),
      deferNextCommit: () => { deferNext.current = true; },
      settleCommit: (accepted) => {
        const pending = deferred.current;
        if (!pending) return;
        deferred.current = null;
        afterRender.current = pending.resolve;
        const { command } = pending;
        setState((s) => {
          if (!accepted) {
            const nodeId = command.type === "param" ? command.target.nodeId : command.nodeId;
            return { ...s, error: { nodeId, message: "Refused delayed fixture commit." } };
          }
          const next = apply(s, command);
          return { ...next, tracks: command.type === "addSend" ? wire(next.tracks, next.master) : next.tracks, error: null };
        });
      },
      setPending: (id, pending) => setState((s) => update(s, id, (t) => ({ ...t, pending }))),
      offerSend: (id, destination) => setState((s) => update(s, id, (t) => ({ ...t, sendDestinations: [...t.sendDestinations, destination] }))),
      remove: (id) => setState((s) => ({ ...s, tracks: s.tracks.filter((t) => t.id !== id) })),
      replace: (id, newId) => setState((s) => ({ ...s, tracks: s.tracks.map((t) => (t.id === id ? { ...t, id: newId, sends: t.sends.map((x) => ({ ...x, id: uid("send") })) } : t)) })),
      removeSend: (id, sendId) => setState((s) => update(s, id, (t) => ({ ...t, sends: t.sends.filter((x) => x.id !== sendId) }))),
      rename: (id, name) => setState((s) => update(s, id, (t) => ({ ...t, name }))),
    };
  }, []);

  const onCommand = (c: MixerCommand) => {
    window.mixerFixture.log.push(c);
    if (deferNext.current && (c.type === "addSend" || (c.type === "param" && c.phase === "commit"))) {
      deferNext.current = false;
      return new Promise<void>((resolve) => { deferred.current = { command: c, resolve }; });
    }
    setState((s) => {
      const live = c.type === "param" && c.phase !== "commit";
      if (s.refuse && !live && c.type !== "select") {
        return { ...s, refuse: null, error: { nodeId: "nodeId" in c ? c.nodeId : c.target.nodeId, message: s.refuse } };
      }
      const next = apply(s, c);
      if (live || next === s) return next;
      const routesChanged = c.type === "setOutput" || c.type === "addSend" || c.type === "removeSend";
      return { ...next, tracks: routesChanged ? wire(next.tracks, next.master) : next.tracks, error: null };
    });
  };

  const mixer = <Mixer tracks={state.tracks} master={state.master} effects={EFFECTS} selectedId={state.selectedId} error={state.error} meters={METERS} unsupported={UNSUPPORTED} onCommand={onCommand} />;
  return (
    <main className="fx">
      <header className="mx-fixture-head">
        <h1>Mixer</h1>
        <span>Night sketch · 140 BPM</span>
        {state.opened && <span role="status">Opened {state.opened}</span>}
        <p>Silent fixture · simulated meters</p>
      </header>
      {measure ? (
        <Profiler id="mixer" onRender={() => { mixerCommits++; }}>
          <StripProfiler.Provider value={countStripCommit}>{mixer}</StripProfiler.Provider>
        </Profiler>
      ) : mixer}
    </main>
  );
}

void waitForStartupStyles(document, window).then(() => {
  initializeTheme();
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <Fixture />
    </StrictMode>,
  );
});
