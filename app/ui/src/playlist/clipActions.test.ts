import { describe, expect, it } from "vitest";
import type { ClipSummary } from "../api/arrangement";
import { buildClipAction, selectedEditableClips } from "./clipActions";
import { MAX_SONG_TICKS } from "./geometry";

const first: ClipSummary = {
  id: 20, track: 1, name: "First", muted: false, startTick: 960,
  content: { type: "pattern", pattern: 0, offsetTick: 480, lengthTicks: 1920 },
};
const second: ClipSummary = {
  ...first, id: 10, track: 2, name: "Second", muted: true, startTick: 1920,
};
const audio: ClipSummary = {
  ...first, id: 3, name: "Audio", startTick: 2880,
  content: { type: "audio", asset: 2, sourceStart: 0, sourceEnd: 48000, gain: 1, reverse: false, fadeIn: 0, fadeOut: 0 },
};
const automation: ClipSummary = {
  ...first, id: 4,
  content: { type: "automation", target: { type: "channelVolume", channel: 1 }, priority: 0, offsetTick: 0, lengthTicks: 960, pointCount: 0 },
};
const clips = [first, audio, second, automation, { ...second, id: 30 }];
const selection = new Set([10, 4, 3, 20, 999]);
const audioAssets = new Map([[2, { sourceRate: 48000 }]]);
const batch = (action: Parameters<typeof buildClipAction>[0], members: ReturnType<typeof selectedEditableClips>) =>
  buildClipAction(action, members, audioAssets, 120);
const members = () => selectedEditableClips(clips, selection);

describe("playlist clip actions", () => {
  it("deletes every editable clip in playlist order, skipping read-only and unselected clips", () => {
    expect(batch("delete", members())).toEqual([
      { kind: "deleteClip", clip: 20 },
      { kind: "deleteClip", clip: 3 },
      { kind: "deleteClip", clip: 10 },
    ]);
  });

  it("mutes a mixed selection and unmutes a fully muted selection", () => {
    expect(batch("mute", members())).toEqual([
      { kind: "setClip", clip: 20, name: "First", muted: true },
      { kind: "setClip", clip: 3, name: "Audio", muted: true },
      { kind: "setClip", clip: 10, name: "Second", muted: true },
    ]);
    expect(batch("mute", selectedEditableClips([{ ...first, muted: true }, second, { ...audio, muted: true }], selection))).toEqual([
      { kind: "setClip", clip: 20, name: "First", muted: false },
      { kind: "setClip", clip: 10, name: "Second", muted: false },
      { kind: "setClip", clip: 3, name: "Audio", muted: false },
    ]);
  });

  it("duplicates in playlist order with one offset and keeps each clip on its track", () => {
    expect(batch("duplicate", members())).toEqual([
      { kind: "copy", clip: 20, track: 1, startTick: 4800 },
      { kind: "copy", clip: 3, track: 1, startTick: 6720 },
      { kind: "copy", clip: 10, track: 2, startTick: 5760 },
    ]);
  });

  it("limits the common copy offset at the song horizon", () => {
    const nearEnd = [
      { ...first, startTick: MAX_SONG_TICKS - 3500 },
      { ...second, startTick: MAX_SONG_TICKS - 2000 },
    ];
    expect(batch("duplicate", selectedEditableClips(nearEnd, selection))).toEqual([
      { kind: "copy", clip: 20, track: 1, startTick: MAX_SONG_TICKS - 3420 },
      { kind: "copy", clip: 10, track: 2, startTick: MAX_SONG_TICKS - 1920 },
    ]);
    const atEnd = [first, { ...second, startTick: MAX_SONG_TICKS - 1920 }];
    expect(batch("duplicate", selectedEditableClips(atEnd, selection))).toEqual([]);
  });

  it.each(["delete", "mute", "duplicate"] as const)("builds no %s batch for empty or read-only selections", action => {
    expect(batch(action, selectedEditableClips(clips, new Set()))).toEqual([]);
    expect(batch(action, selectedEditableClips(clips, new Set([4])))).toEqual([]);
  });
});
