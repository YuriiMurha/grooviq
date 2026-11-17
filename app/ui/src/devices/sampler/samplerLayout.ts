// The sampler editor's parameters and where each one sits. Ranges, defaults and which ones are
// whole numbers come from the engine's table (tests/fixtures/device-params.json, which a Rust
// test keeps in step with `grooviq_dsp::sampler`); this file adds only how each one is shown and
// changed.

import deviceParams from "../../../../../tests/fixtures/device-params.json";
import { NOTE_NAMES } from "../../roll/scales";
import type { Curve, ParamRange } from "../controls/paramDrag";
import { formatHz, formatMs } from "../synth/synthLayout";

/** How a value reads: its unit or kind. */
export type Unit = "note" | "st" | "ct" | "pos" | "switch" | "ms" | "pct" | "bipct" | "dB" | "pan" | "Hz" | "glide" | "enum";

interface Meta {
  /** The short label on the control. */
  label: string;
  /** The accessible name ("Amp attack"). */
  name: string;
  /** What the parameter does: the first sentence of its hint. */
  does: string;
  unit: Unit;
  curve?: Curve;
  options?: string[];
}

const env = (w: "" | "flt_", name: string): Record<string, Meta> => ({
  [`${w}attack`]: { label: "A", name: `${name} attack`, does: `${name} envelope attack: how long it takes to rise.`, unit: "ms", curve: "sqr" },
  [`${w}decay`]: { label: "D", name: `${name} decay`, does: `${name} envelope decay: how long it takes to fall to the sustain level.`, unit: "ms", curve: "log" },
  [`${w}sustain`]: { label: "S", name: `${name} sustain`, does: `${name} envelope sustain: the level it holds while the key is down.`, unit: "pct" },
  [`${w}release`]: { label: "R", name: `${name} release`, does: `${name} envelope release: how long it fades after the key is let go.`, unit: "ms", curve: "log" },
});

const META: Record<string, Meta> = {
  root: { label: "Root", name: "Root key", does: "The key that plays the sample at its own pitch.", unit: "note" },
  tune: { label: "Tune", name: "Tune", does: "Tunes the sample in semitones.", unit: "st" },
  fine: { label: "Fine", name: "Fine tune", does: "Fine tuning in cents.", unit: "ct" },
  start: { label: "Start", name: "Region start", does: "Where the sample starts playing.", unit: "pos" },
  end: { label: "End", name: "Region end", does: "Where the sample stops playing.", unit: "pos" },
  reverse: { label: "Reverse", name: "Reverse", does: "Plays the region backwards.", unit: "switch" },
  pitch_track: { label: "Follow keys", name: "Follow keys", does: "The key played changes the pitch; off, every key plays the sample at the root's pitch.", unit: "switch" },
  one_shot: { label: "One-shot", name: "One-shot", does: "Plays the whole region on every hit, however short the note; off, letting go of the key releases it.", unit: "switch" },
  ...env("", "Amp"),
  gain: { label: "Gain", name: "Gain", does: "The sampler's output level.", unit: "dB" },
  pan: { label: "Pan", name: "Pan", does: "Where the sampler sits between left and right.", unit: "pan" },
  declick: { label: "Declick", name: "Declick", does: "Short fades at the region's edges remove clicks.", unit: "switch" },
  loop: { label: "Loop", name: "Loop", does: "Repeats the region's tail from the loop marker until the key is let go.", unit: "switch" },
  loop_start: { label: "Loop", name: "Loop start", does: "Where the loop begins inside the played region.", unit: "pct" },
  filter_type: { label: "Filter type", name: "Filter type", does: "The filter: off, low-pass, high-pass or band-pass.", unit: "enum", options: ["Off", "LP", "HP", "BP"] },
  cutoff: { label: "Cutoff", name: "Filter cutoff", does: "The filter's cutoff frequency.", unit: "Hz", curve: "log" },
  res: { label: "Res", name: "Filter resonance", does: "The filter's resonance: a peak at the cutoff.", unit: "pct" },
  filter_env: { label: "Env amt", name: "Filter envelope amount", does: "How far the filter envelope moves the cutoff, up or down.", unit: "bipct" },
  ...env("flt_", "Filter"),
  vel_amp: { label: "To level", name: "Velocity to level", does: "How much softer notes play quieter.", unit: "pct" },
  vel_filter: { label: "To cutoff", name: "Velocity to cutoff", does: "How much softer notes close the filter.", unit: "pct" },
  voice_mode: { label: "Voice mode", name: "Voice mode", does: "Poly plays chords; mono plays one note; legato slides between held notes.", unit: "enum", options: ["Poly", "Mono", "Legato"] },
  glide: { label: "Glide", name: "Glide", does: "How long a note slides from the previous one in mono, legato and on slide notes.", unit: "glide", curve: "sqr" },
};

export interface SamplerParam extends ParamRange {
  id: string;
  label: string;
  name: string;
  does: string;
  unit: Unit;
  options?: string[];
}

/** Every sampler parameter in the engine's order, with its range and how it is shown. */
export const SAMPLER_PARAMS: SamplerParam[] = deviceParams.sampler.map((p) => {
  const m = META[p.id];
  if (!m) throw new Error(`The sampler editor has no place for "${p.id}".`);
  const { curve = "lin", ...rest } = m;
  // `stepped` is the engine's: it rounds these on set (`grooviq_dsp::sampler::is_stepped`).
  return { id: p.id, min: p.min, max: p.max, default: p.default, curve, stepped: p.stepped, ...rest };
});

const BY_ID = new Map(SAMPLER_PARAMS.map((p) => [p.id, p]));

export function samplerParam(id: string): SamplerParam {
  const p = BY_ID.get(id);
  if (!p) throw new Error(`The sampler has no parameter "${id}".`);
  return p;
}

/** A knob, a small drag field, a row of buttons (an enum), an on/off switch, or a handle on the
 * waveform. */
export type ControlKind = "knob" | "field" | "segmented" | "toggle" | "marker";

/** Where a control sits on its card: the card's header, beside its display, or under it. */
export type Slot = "head" | "side" | "foot";

export interface Placement {
  id: string;
  kind: ControlKind;
  slot: Slot;
}

export type SectionId = "header" | "sample" | "pitch" | "amp" | "filter" | "flt" | "voice" | "velocity";

export interface Section {
  id: SectionId;
  title: string;
  controls: Placement[];
}

const k = (id: string, slot: Slot = "foot"): Placement => ({ id, kind: "knob", slot });
const f = (id: string, slot: Slot = "foot"): Placement => ({ id, kind: "field", slot });
const s = (id: string, slot: Slot = "head"): Placement => ({ id, kind: "segmented", slot });
const t = (id: string, slot: Slot = "head"): Placement => ({ id, kind: "toggle", slot });
const m = (id: string): Placement => ({ id, kind: "marker", slot: "side" });

/** The sample with its region on top, the pitch beside it; then the two envelopes either side
 * of the filter, and the voice and velocity sections. Gain and pan sit in the title bar. */
export const SAMPLER_SECTIONS: Section[] = [
  { id: "header", title: "Sampler", controls: [k("gain", "head"), k("pan", "head")] },
  { id: "sample", title: "Sample", controls: [m("start"), m("end"), m("loop_start"), t("reverse"), t("one_shot"), t("loop"), t("declick")] },
  { id: "pitch", title: "Pitch", controls: [f("root", "side"), k("tune", "side"), k("fine", "side"), t("pitch_track", "foot")] },
  { id: "amp", title: "Amp env", controls: [f("attack"), f("decay"), f("sustain"), f("release")] },
  { id: "filter", title: "Filter", controls: [s("filter_type"), k("cutoff"), k("res"), k("filter_env")] },
  { id: "flt", title: "Filter env", controls: [f("flt_attack"), f("flt_decay"), f("flt_sustain"), f("flt_release")] },
  { id: "voice", title: "Voice", controls: [s("voice_mode", "side"), k("glide", "foot")] },
  { id: "velocity", title: "Velocity", controls: [k("vel_amp", "side"), k("vel_filter", "side")] },
];

export function section(id: SectionId): Section {
  return SAMPLER_SECTIONS.find((x) => x.id === id)!;
}

/** Choke groups: 0 (none) to 16, one place in the engine, the project and here. */
export const CHOKE_GROUPS = 16;

const MINUS = "−";
const signed = (v: number, digits = 0) => (v > 0 ? "+" : v < 0 ? MINUS : "") + Math.abs(v).toFixed(digits);

/** A MIDI key as a note name, C4 = 60. */
export function noteName(key: number): string {
  const k = Math.round(key);
  return `${NOTE_NAMES[((k % 12) + 12) % 12]}${Math.floor(k / 12) - 1}`;
}

/** A value as the editor shows it, with its unit. */
export function formatValue(p: SamplerParam, v: number): string {
  switch (p.unit) {
    case "note":
      return noteName(v);
    case "enum":
      return p.options?.[Math.round(v) - p.min] ?? String(v);
    case "switch":
      return v >= 0.5 ? "On" : "Off";
    case "st": {
      const r = Math.round(v * 10) / 10;
      return `${r === 0 ? "0" : signed(r, Number.isInteger(r) ? 0 : 1)} st`;
    }
    case "ct":
      return `${Math.round(v) === 0 ? "0" : signed(Math.round(v))} ct`;
    case "pos":
      return `${(v * 100).toFixed(1)}%`;
    case "bipct":
      return `${Math.round(v * 100) === 0 ? "0" : signed(Math.round(v * 100))}%`;
    case "Hz":
      return formatHz(v);
    case "ms":
      return formatMs(v);
    case "glide":
      return v < 0.5 ? "Off" : formatMs(v);
    case "dB":
      return `${Math.abs(v) < 0.05 ? "0.0" : signed(v, 1)} dB`;
    case "pan": {
      const r = Math.round(v * 100);
      return r === 0 ? "C" : `${r < 0 ? "L" : "R"}${Math.abs(r)}`;
    }
    default:
      return `${Math.round(v * 100)}%`;
  }
}
