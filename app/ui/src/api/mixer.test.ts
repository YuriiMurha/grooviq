import { describe, expect, it } from "vitest";
import {
  decodeMixerEditReply, decodeMixerInsertReply, decodeMixerSnapshot, encodeMixerEdit,
  type MixerInsertDetail, type MixerSnapshot,
} from "./mixer";

const stamp = { generation: 4, revision: 12 };
const insert = {
  id: 8, device: "drum_shaper", params: [2, 6, 1, 1, 0.2, 0.8, -3],
  preset: "Snare Body", seed: "18446744073709551615", bypass: false,
};
const master = {
  id: 0, name: "Master", output: null, inserts: [insert],
  level: { db: 0 }, pan: 0, mute: false, solo: false,
};
const bus = { ...master, id: 2, name: "Bus", output: 0, inserts: [], level: "silence" as const };
const snapshot: MixerSnapshot = {
  ...stamp,
  mixer: {
    mode: "routed", tracks: [master, bus],
    sends: [{ id: 3, from: 2, to: 0, tap: "pre", gain: 4 }],
    sidechains: [{ from: 2, to: 8, tap: "post" }],
    sources: [{ source: { channel: 7 }, destination: 2 }],
    nextTrackId: 3, nextInsertId: 9, nextSendId: 4,
  },
  sources: [
    { source: { channel: 7 }, name: "Synth", instrument: "synth" },
    { source: { audioTrack: 7 }, name: "Audio", instrument: null },
  ],
  effects: ["drum_shaper"],
  unsupported: { mute: "Unavailable", solo: "Unavailable", bypass: "Unavailable", detector: "Unavailable" },
};
const detail: MixerInsertDetail = {
  ...stamp, track: 0, insert: 8, effectId: "drum_shaper", active: true,
  params: [2, 6, 1, 1, 0.2, 0.8, -3], preset: "Snare Body", factoryPresets: ["Kick Punch", "Snare Body"],
};

describe("mixer wire", () => {
  it("keeps Rust controls and equal IDs in distinct source namespaces", () => {
    expect(decodeMixerSnapshot(snapshot)).toEqual(snapshot);
  });

  it("rejects malformed controls, seeds, and identities instead of normalizing them", () => {
    const mixerWith = (changes: object) => ({ ...snapshot, mixer: { ...snapshot.mixer, ...changes } });
    const trackWith = (changes: object) => mixerWith({ tracks: [{ ...master, ...changes }] });
    const slotWith = (changes: object) => trackWith({ inserts: [{ ...insert, ...changes }] });
    for (const malformed of [
      { ...snapshot, revision: Number.MAX_SAFE_INTEGER + 1 },
      trackWith({ id: 2 ** 32 }),
      trackWith({ level: -60 }),
      trackWith({ level: { db: null } }),
      slotWith({ id: 0 }),
      slotWith({ seed: Number(insert.seed) }),
      slotWith({ seed: "18446744073709551616" }),
      slotWith({ seed: "01" }),
      slotWith({ params: [1, null] }),
      mixerWith({ sends: [{ ...snapshot.mixer.sends[0], gain: 4.01 }] }),
      { ...snapshot, sources: [{ source: { channel: 7, audioTrack: 7 }, name: "Both", instrument: "synth" }] },
      { ...snapshot, sources: [{ source: { audioTrack: 7 }, name: "Audio", instrument: "sampler" }] },
    ]) {
      expect(() => decodeMixerSnapshot(malformed)).toThrow(/mixer snapshot/i);
    }
  });

  it("encodes one action with only the request stamp and preserves omitted fields", () => {
    expect(JSON.parse(encodeMixerEdit(snapshot, { kind: "setFader", track: 2, pan: -0.25 }))).toEqual({
      ...stamp, edit: { kind: "setFader", track: 2, pan: -0.25 },
    });
    expect(JSON.parse(encodeMixerEdit(stamp, {
      kind: "createTrack", name: "Audio bus", output: 0, source: { audioTrack: 7 },
    }))).toEqual({ ...stamp, edit: { kind: "createTrack", name: "Audio bus", output: 0, source: { audioTrack: 7 } } });

    const keepGain = { kind: "setSend", from: 2, send: 3, tap: "post", gain: null } as const;
    expect(JSON.parse(encodeMixerEdit(stamp, keepGain))).toEqual({ ...stamp, edit: keepGain });
  });

  it("rejects unsafe outbound identities, lossy JSON numbers, and oversized UTF-8 requests", () => {
    expect(() => encodeMixerEdit({ ...stamp, generation: -1 }, { kind: "setOutput", track: 2, output: 0 })).toThrow();
    expect(() => encodeMixerEdit(stamp, { kind: "setOutput", track: 2 ** 32, output: 0 })).toThrow();
    expect(() => encodeMixerEdit(stamp, { kind: "setSend", from: 2, send: 3, gain: NaN })).toThrow();
    expect(() => encodeMixerEdit(stamp, { kind: "setInsert", track: 0, insert: 8, params: [Infinity] })).toThrow();
    expect(() => encodeMixerEdit(stamp, { kind: "createTrack", name: "🎛".repeat(4200), output: 0 })).toThrow(/too large/i);
  });

  it("accepts a consecutive edit and a no-op, and rejects a miscorrelated success", () => {
    const changed = { status: "applied", state: { ...snapshot, revision: 13 }, changed: true, createdIds: [{ kind: "send", id: 3 }], message: null };
    const noOp = { ...changed, state: snapshot, changed: false, createdIds: [] };
    expect(decodeMixerEditReply(changed, stamp)).toEqual(changed);
    expect(decodeMixerEditReply(noOp, stamp)).toEqual(noOp);
    for (const state of [
      { ...snapshot, revision: 11 }, snapshot, { ...snapshot, revision: 14 },
      { ...snapshot, generation: 5, revision: 13 },
    ]) {
      expect(() => decodeMixerEditReply({ ...changed, state }, stamp)).toThrow(/mixer edit reply/i);
    }
    expect(() => decodeMixerEditReply({ ...noOp, createdIds: changed.createdIds }, stamp)).toThrow();
  });

  it("returns the replacement state supplied by either refusal", () => {
    const state = { ...snapshot, generation: 5, revision: 0 };
    const refused = { status: "refused", state, message: "The project was replaced." };
    const edit = { ...refused, changed: false, createdIds: [] };
    expect(decodeMixerEditReply(edit, stamp)).toEqual(edit);
    expect(decodeMixerInsertReply(refused, detail)).toEqual(refused);
    const newerRevision = { ...edit, state: { ...snapshot, revision: 15 } };
    expect(decodeMixerEditReply(newerRevision, stamp)).toEqual(newerRevision);
    expect(() => decodeMixerEditReply({ ...edit, changed: true }, stamp)).toThrow();
    expect(() => decodeMixerEditReply({ ...edit, createdIds: [{ kind: "track", id: 2 }] }, stamp)).toThrow();
    expect(() => decodeMixerEditReply({ ...edit, state: { ...state, generation: Infinity } }, stamp)).toThrow();
    expect(() => decodeMixerInsertReply({ ...refused, message: null }, detail)).toThrow();
  });

  it("accepts full insert detail only for the requested generation, revision, track, and slot", () => {
    expect(decodeMixerInsertReply({ status: "applied", detail }, detail)).toEqual({ status: "applied", detail });
    for (const changes of [
      { generation: 5 }, { revision: 11 }, { track: 2 }, { insert: 9 },
      { active: "true" }, { factoryPresets: [null] }, { params: [0, null] },
    ]) {
      expect(() => decodeMixerInsertReply({ status: "applied", detail: { ...detail, ...changes } }, detail)).toThrow(/mixer insert reply/i);
    }
  });
});
