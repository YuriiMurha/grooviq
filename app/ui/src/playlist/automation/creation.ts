import type { ArrangementOperation, AutomationContent, AutomationTarget, LoopRegion } from "../../api/arrangement";
import type { ChannelView, TimeSignature } from "../../api/project";
import { SAMPLER_SECTIONS } from "../../devices/sampler/samplerLayout";
import { SYNTH_SECTIONS } from "../../devices/synth/synthLayout";
import { MAX_SONG_TICKS } from "../geometry";
import { pointTarget } from "./target";

export type ControlTarget = Extract<AutomationTarget, { type: "instrumentParam" }>;
type ControlChannel = Pick<ChannelView, "id" | "kind" | "sampler" | "synth">;
export type StartChoice = "playhead" | "loop" | "song";
export type TrackChoice = number | "new";
type Placement = { startTick: number; lengthTicks: number };
type Track = { id: number };

const controlIds = (sections: readonly { controls: readonly { id: string; kind: string }[] }[]) =>
  new Set(sections.flatMap(section => section.controls.filter(control => control.kind === "knob" || control.kind === "field").map(control => control.id)));
const eligibleIds = { sampler: controlIds(SAMPLER_SECTIONS), synth: controlIds(SYNTH_SECTIONS) };

export function currentControlValue(channels: readonly ControlChannel[], target: ControlTarget, shown: number): number | null {
  if (!eligibleIds[target.instrument].has(target.param)) return null;
  const channel = channels.find(item => item.id === target.channel);
  if (channel?.kind !== target.instrument) return null;
  const spec = pointTarget(target);
  if (!spec) return null;
  const value = Math.fround(channel[target.instrument][target.param]);
  // Device editors can temporarily show an uncommitted drag or wheel preview.
  return value === Math.fround(shown) && value >= Math.fround(spec.min) && value <= Math.fround(spec.max) ? value : null;
}

const usableTick = (tick: number | null): tick is number =>
  tick !== null && Number.isSafeInteger(tick) && tick >= 0 && tick < MAX_SONG_TICKS;

export function automationStartOptions(playhead: number | null, loop: LoopRegion | null): StartChoice[] {
  const choices: StartChoice[] = [];
  if (usableTick(playhead)) choices.push("playhead");
  if (loop?.enabled && usableTick(loop.startTick)) choices.push("loop");
  if (!usableTick(playhead)) choices.push("song");
  return choices;
}

export function automationPlacement(choice: StartChoice, playhead: number | null, loop: LoopRegion | null, meter: TimeSignature): Placement | null {
  const tick = choice === "playhead" ? playhead : choice === "loop" ? (loop?.enabled ? loop.startTick : null) : 0;
  if (!usableTick(tick)) return null;
  const barTicks = Math.max(1, Math.floor(3840 / Math.max(1, meter.unit))) * Math.max(1, meter.beats);
  return { startTick: tick, lengthTicks: Math.min(barTicks, MAX_SONG_TICKS - tick) };
}

export function selectedAutomationTrack(clips: readonly { track: number }[], tracks: readonly Track[]): number | null {
  const track = clips[0]?.track;
  return track !== undefined && clips.every(clip => clip.track === track) && tracks.some(candidate => candidate.id === track) ? track : null;
}

export function buildAutomationCreation(target: ControlTarget, value: number, name: string, placement: Placement, tracks: readonly Track[], track: TrackChoice): readonly ArrangementOperation[] | null {
  const content: AutomationContent = {
    type: "automation", target, priority: 0, // Native creation allocates the layer in its entity ID space.
    offsetTick: 0, lengthTicks: placement.lengthTicks, points: [{ tick: 0, value, mode: "linear" }],
  };
  const clip = { name, startTick: placement.startTick, muted: false, content };
  if (track === "new") return tracks.length === 0 ? [
    { kind: "createTrack", name: "Track 1", color: 0 },
    { kind: "createClipOnCreatedTrack", trackOperation: 0, ...clip },
  ] : null;
  return tracks.some(candidate => candidate.id === track) ? [{ kind: "createClip", track, ...clip }] : null;
}
