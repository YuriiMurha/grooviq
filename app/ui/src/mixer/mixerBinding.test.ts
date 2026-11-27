import { describe, expect, it } from "vitest";
import type { MixerInsert, MixerRouteInput, MixerSnapshot, MixerTrack as WireTrack } from "../api/mixer";
import { bindMixerSnapshot, type MixerBinding } from "./mixerBinding";
import { LEVEL, moveTarget, outputChoices, type MixerTrack } from "./mixerModel";

const stamp = { generation: 11, revision: 21 };
const unsupported = { mute: "Mute unavailable", solo: "Solo unavailable", bypass: "Bypass unavailable", detector: "Detector unavailable" };
const track = (id: number, output: number | null = 0): WireTrack => ({
  id, name: id === 0 ? "Master" : `Track ${id}`, output, inserts: [], level: { db: -4.5 }, pan: 0, mute: false, solo: false,
});
const insert = (id: number, device: string): MixerInsert => ({
  id, device, params: [], preset: null, seed: "18446744073709551615", bypass: id === 2,
});

function fixture(): MixerSnapshot {
  return {
    ...stamp, effects: ["eq", "comp", "lofi_colour", "future_effect"], unsupported,
    sources: [
      { source: { channel: 7 }, name: "Synth", instrument: "synth" },
      { source: { audioTrack: 7 }, name: "Audio", instrument: null },
    ],
    mixer: {
      mode: "routed",
      tracks: [
        track(0, null), { ...track(1, 2), inserts: [insert(1, "eq"), insert(2, "comp"), insert(3, "future_effect")] },
        track(2, 4), track(3), { ...track(4, 5), inserts: [insert(4, "comp")] },
        track(5, 6), { ...track(6), level: "silence", pan: -0.75 }, track(9),
      ],
      sends: [
        { id: 1, from: 1, to: 3, gain: 0, tap: "post" },
        { id: 2, from: 6, to: 9, gain: 3.5, tap: "pre" },
        { id: 3, from: 6, to: 0, gain: 1, tap: "post" },
        { id: 4, from: 6, to: 9, gain: 4, tap: "post" },
      ],
      sidechains: [{ from: 3, to: 4, tap: "pre" }],
      sources: [
        { source: { channel: 7 }, destination: 1 },
        { source: { audioTrack: 7 }, destination: 3 },
      ],
      nextTrackId: 10, nextInsertId: 5, nextSendId: 5,
    },
    routeInputs: [
      { track: 0, from: [3, 6, 9] }, { track: 1, from: [] }, { track: 2, from: [1] },
      { track: 3, from: [1] }, { track: 4, from: [2, 3] }, { track: 5, from: [4] },
      { track: 6, from: [5] }, { track: 9, from: [6] },
    ],
  };
}

function node(binding: MixerBinding, id: number): MixerTrack {
  const track = [binding.props.master, ...binding.props.tracks].find((candidate) => binding.resolveTrack(candidate.id) === id);
  if (!track) throw new Error(`Missing track ${id}`);
  return track;
}

const outputIds = (binding: MixerBinding, track: number) =>
  node(binding, track).outputs.map((option) => binding.resolveTrack(option.id));

describe("mixer binding", () => {
  it("walks a mixed diamond backward and admits routes only in the safe direction", () => {
    const binding = bindMixerSnapshot(fixture());
    expect(outputIds(binding, 6)).toEqual([0, 9]);
    expect(outputIds(binding, 3)).toEqual([0, 2, 4, 5, 6, 9]); // The existing zero-gain send blocks 3 -> 1.
    expect(outputIds(binding, 4)).toEqual([0, 5, 6, 9]); // Detector track 3 is also a predecessor.
    expect(outputIds(binding, 0)).toEqual([]);
    expect(binding.translate({ type: "setOutput", nodeId: node(binding, 6).id, outputId: node(binding, 1).id }).kind).toBe("refused");
    expect(binding.translate({ type: "setOutput", nodeId: node(binding, 1).id, outputId: node(binding, 6).id })).toEqual({
      kind: "edit", stamp, edit: { kind: "setOutput", track: 1, output: 6 },
    });
    expect(binding.translate({ type: "setOutput", nodeId: node(binding, 6).id, outputId: node(binding, 6).id }).kind).toBe("refused");
    expect(binding.translate({ type: "setOutput", nodeId: node(binding, 0).id, outputId: node(binding, 6).id }).kind).toBe("refused");
  });

  it("fails closed without topology while keeping stored route labels and sends", () => {
    const binding = bindMixerSnapshot({ ...fixture(), routeInputs: null });
    const source = node(binding, 6);
    const nodes = new Map([binding.props.master, ...binding.props.tracks].map((track) => [track.id, track]));
    expect(source.outputs).toEqual([]);
    expect(source.sendDestinations).toEqual([]);
    expect(outputChoices(source.outputs, source.outputId, nodes).label).toBe("Master");
    expect(source.sends.map((send) => nodes.get(send.destinationId)?.name)).toEqual(["Track 9", "Master", "Track 9"]);
    expect(binding.translate({ type: "addSend", nodeId: source.id, destinationId: node(binding, 3).id, pre: false, amount: 0 }).kind).toBe("refused");
  });

  it("keeps view units and stored sends while removing occupied new-send choices", () => {
    const binding = bindMixerSnapshot(fixture());
    const source = node(binding, 6);
    const root = node(binding, 1);
    expect(source).toMatchObject({ level: LEVEL.min, pan: -0.75, clipped: false, kind: "Track" });
    expect(root).toMatchObject({ level: -4.5, kind: "Instrument" });
    expect(node(binding, 3).kind).toBe("Audio");
    expect(source.sends.map(({ amount, pre }) => ({ amount, pre }))).toEqual([
      { amount: 3.5, pre: true }, { amount: 1, pre: false }, { amount: 4, pre: false },
    ]);
    expect(source.sendDestinations).toEqual([]);
    expect(root.sendDestinations.map((choice) => binding.resolveTrack(choice.id))).toEqual([0, 4, 5, 6, 9]);
    expect(root.inserts.map(({ name, active }) => [name, active])).toEqual([
      ["EQ", true], ["Compressor", false], ["Future Effect", true],
    ]);
    expect(binding.props.effects.find((effect) => effect.id === "lofi_colour")?.name).toBe("Lo-fi Colour");
    expect(binding.props.unsupported).toEqual(unsupported);
  });

  it("previews and cancels without edits and emits one stamped edit for each committed control", () => {
    const binding = bindMixerSnapshot(fixture());
    const source = node(binding, 6);
    const target = { kind: "send", nodeId: source.id, sendId: source.sends[0].id } as const;
    for (const phase of ["live", "cancel"] as const) {
      expect(binding.translate({ type: "param", target, value: 2.75, phase })).toEqual({ kind: "ignore" });
    }
    expect(binding.translate({ type: "param", target, value: 2.75, phase: "commit" })).toEqual({
      kind: "edit", stamp, edit: { kind: "setSend", from: 6, send: 2, gain: 2.75 },
    });
    expect(binding.translate({ type: "param", target: { kind: "pan", nodeId: source.id }, value: -0.625, phase: "commit" })).toEqual({
      kind: "edit", stamp, edit: { kind: "setFader", track: 6, pan: -0.625 },
    });
    for (const value of [LEVEL.min, -11.25]) {
      expect(binding.translate({ type: "param", target: { kind: "level", nodeId: source.id }, value, phase: "commit" })).toEqual({
        kind: "edit", stamp, edit: { kind: "setFader", track: 6, level: value === LEVEL.min ? "silence" : { db: value } },
      });
    }
    expect(binding.translate({ type: "setSendPre", nodeId: source.id, sendId: target.sendId, pre: false })).toEqual({
      kind: "edit", stamp, edit: { kind: "setSend", from: 6, send: 2, tap: "post" },
    });
    expect(binding.translate({ type: "removeSend", nodeId: source.id, sendId: target.sendId })).toEqual({
      kind: "edit", stamp, edit: { kind: "removeSend", from: 6, send: 2 },
    });
    const send = (id: number) => binding.translate({ type: "addSend", nodeId: node(binding, 1).id, destinationId: node(binding, id).id, pre: false, amount: 0 });
    expect(send(2).kind).toBe("refused");
    expect(send(3).kind).toBe("refused");
    expect(send(4)).toEqual({ kind: "edit", stamp, edit: { kind: "addSend", from: 1, to: 4, gain: 0, tap: "post" } });
  });

  it("translates rack moves in either direction using the index after removal", () => {
    const binding = bindMixerSnapshot(fixture());
    const rack = node(binding, 1);
    for (const [slot, direction, index] of [[0, 1, 1], [2, -1, 1], [1, 1, 2]] as const) {
      const slotId = rack.inserts[slot].id;
      const move = moveTarget(rack.inserts, slotId, direction);
      if (!move) throw new Error("Expected a move");
      expect(binding.translate({ type: "moveInsert", nodeId: rack.id, slotId, ...move })).toEqual({
        kind: "edit", stamp, edit: { kind: "moveInsert", track: 1, insert: slot + 1, index },
      });
    }
    expect(binding.translate({ type: "addInsert", nodeId: rack.id, effectId: "eq" })).toEqual({
      kind: "edit", stamp, edit: { kind: "addInsert", track: 1, index: 3, effectId: "eq" },
    });
    expect(binding.translate({ type: "addInsert", nodeId: rack.id, effectId: "missing" }).kind).toBe("refused");
    expect(binding.translate({ type: "removeInsert", nodeId: rack.id, slotId: rack.inserts[0].id })).toEqual({
      kind: "edit", stamp, edit: { kind: "removeInsert", track: 1, insert: 1 },
    });
  });

  it("separates local intents and refuses cross-owner or generation-stale commands", () => {
    const snapshot = fixture();
    const binding = bindMixerSnapshot(snapshot);
    const rack = node(binding, 1);
    const slotId = rack.inserts[0].id;
    const wrongSlot = node(binding, 4).inserts[0].id;
    const source = node(binding, 6);
    const sendId = source.sends[0].id;
    expect(binding.translate({ type: "select", nodeId: rack.id })).toEqual({ kind: "select", nodeId: rack.id });
    expect(binding.translate({ type: "clearClip", nodeId: rack.id })).toEqual({ kind: "clearClip", nodeId: rack.id });
    expect(binding.translate({ type: "mute", nodeId: rack.id, on: true })).toEqual({ kind: "unsupported", nodeId: rack.id, message: unsupported.mute });
    expect(binding.translate({ type: "setDetector", nodeId: rack.id, slotId, sourceId: null })).toEqual({
      kind: "unsupported", nodeId: rack.id, message: unsupported.detector,
    });
    expect(binding.translate({ type: "openInsert", nodeId: rack.id, slotId })).toEqual({ kind: "openInsert", target: { ...stamp, track: 1, insert: 1 } });
    expect(binding.translate({ type: "openInsert", nodeId: rack.id, slotId: wrongSlot }).kind).toBe("refused");
    expect(binding.translate({ type: "moveInsert", nodeId: rack.id, slotId, beforeSlotId: wrongSlot }).kind).toBe("refused");
    expect(binding.translate({ type: "removeSend", nodeId: rack.id, sendId }).kind).toBe("refused");

    const revised = bindMixerSnapshot({ ...snapshot, revision: 22 });
    expect(node(revised, 1).id).toBe(rack.id);
    expect(node(revised, 1).inserts[0].id).toBe(slotId);
    const replacement = bindMixerSnapshot({ ...snapshot, generation: 12, revision: 21 });
    const freshRack = node(replacement, 1);
    expect(freshRack.id).not.toBe(rack.id);
    expect(replacement.resolveTrack(rack.id)).toBeUndefined();
    expect(replacement.translate({ type: "select", nodeId: rack.id }).kind).toBe("refused");
    expect(replacement.translate({ type: "openInsert", nodeId: freshRack.id, slotId }).kind).toBe("refused");
    expect(replacement.translate({ type: "removeSend", nodeId: node(replacement, 6).id, sendId }).kind).toBe("refused");
  });

  it("keeps route work lazy, shares option objects, and evicts rows while scrolling", () => {
    let reads = 0;
    const count = 90;
    const snapshot = fixture();
    const tracks = [track(0, null), ...Array.from({ length: count }, (_, at) => track(at + 1))];
    const routeInputs = tracks.map(({ id }): MixerRouteInput => ({
      track: id,
      get from() { reads++; return id === 0 ? tracks.slice(1).map(({ id }) => id) : []; },
    }));
    const binding = bindMixerSnapshot({ ...snapshot, routeInputs, mixer: { ...snapshot.mixer, tracks, sends: [], sidechains: [], sources: [], nextTrackId: count + 1 } });
    const [first, second] = binding.props.tracks;
    expect(first.name).toBe("Track 1");
    expect(reads).toBe(0);
    const firstOutputs = first.outputs;
    expect(reads).toBe(1);
    expect(first.sendDestinations).toHaveLength(count - 1);
    expect(first.outputs).toBe(firstOutputs);
    expect(reads).toBe(1);
    expect(second.outputs[0]).toBe(firstOutputs[0]);
    for (const track of binding.props.tracks.slice(2)) void track.outputs;
    expect(reads).toBe(count);
    expect(first.outputs).toEqual(firstOutputs);
    expect(reads).toBe(count + 1);
  });

  it("handles a long predecessor chain iteratively", () => {
    const count = 12_000;
    const snapshot = fixture();
    const tracks = [track(0, null), ...Array.from({ length: count }, (_, at) => track(at + 1, at + 1 === count ? 0 : at + 2))];
    const routeInputs = tracks.map(({ id }) => ({ track: id, from: id === 0 ? [count] : id === 1 ? [] : [id - 1] }));
    const binding = bindMixerSnapshot({ ...snapshot, routeInputs, mixer: { ...snapshot.mixer, tracks, sends: [], sidechains: [], sources: [], nextTrackId: count + 1 } });
    expect(binding.props.tracks.at(-1)?.outputs).toEqual([{ id: binding.props.master.id, name: "Master" }]);
  });
});
