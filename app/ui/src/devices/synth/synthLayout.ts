// The synth editor's parameters and where each one sits. Ranges, defaults and which ones are
// whole numbers come from the engine's table (tests/fixtures/device-params.json, which a Rust test keeps in step with
// `grooviq_dsp::synth`); this file adds only how each one is shown and changed.

import deviceParams from "../../../../../tests/fixtures/device-params.json";
import type { Curve, ParamRange } from "../controls/paramDrag";

/** How a value reads: its unit or kind. */
export type Unit = "shape" | "oct" | "st" | "ct" | "voices" | "pct" | "bipct" | "Hz" | "rate" | "ms" | "glide" | "dB" | "enum";

interface Meta {
  /** The short label on the control. */
  label: string;
  /** The accessible name ("Osc 1 octave"). */
  name: string;
  /** What the parameter does: the first sentence of its hint. */
  does: string;
  unit: Unit;
  curve?: Curve;
  options?: string[];
}

const SHAPE_DOES = "Morphs the waveform from sine through triangle and saw to square.";
const osc = (n: 1 | 2): Record<string, Meta> => ({
  [`osc${n}_shape`]: { label: "Shape", name: `Osc ${n} shape`, does: `Osc ${n}: ${SHAPE_DOES}`, unit: "shape" },
  [`osc${n}_octave`]: { label: "Oct", name: `Osc ${n} octave`, does: `Osc ${n}: the octave it plays in.`, unit: "oct" },
  [`osc${n}_semi`]: { label: "Semi", name: `Osc ${n} semi`, does: `Osc ${n}: tunes it in semitones.`, unit: "st" },
  [`osc${n}_fine`]: { label: "Fine", name: `Osc ${n} fine`, does: `Osc ${n}: fine tuning in cents.`, unit: "ct" },
  [`osc${n}_level`]: { label: "Level", name: `Osc ${n} level`, does: `Osc ${n}: how loud it is in the mix. 0 turns it off.`, unit: "pct" },
  [`osc${n}_unison`]: { label: "Uni", name: `Osc ${n} unison`, does: `Osc ${n}: how many detuned copies play together.`, unit: "voices" },
  [`osc${n}_detune`]: { label: "Detune", name: `Osc ${n} detune`, does: `Osc ${n}: how far the unison copies drift apart in pitch.`, unit: "pct" },
  [`osc${n}_spread`]: { label: "Spread", name: `Osc ${n} spread`, does: `Osc ${n}: how wide the unison copies sit between left and right.`, unit: "pct" },
});
const env = (w: "amp" | "flt", name: string): Record<string, Meta> => ({
  [`${w}_attack`]: { label: "A", name: `${name} attack`, does: `${name} envelope attack: how long it takes to rise.`, unit: "ms", curve: "log" },
  [`${w}_decay`]: { label: "D", name: `${name} decay`, does: `${name} envelope decay: how long it takes to fall to the sustain level.`, unit: "ms", curve: "log" },
  [`${w}_sustain`]: { label: "S", name: `${name} sustain`, does: `${name} envelope sustain: the level it holds while the key is down.`, unit: "pct" },
  [`${w}_release`]: { label: "R", name: `${name} release`, does: `${name} envelope release: how long it fades after the key is let go.`, unit: "ms", curve: "log" },
});

const META: Record<string, Meta> = {
  ...osc(1),
  ...osc(2),
  sub_level: { label: "Level", name: "Sub level", does: "A sub oscillator an octave below osc 1: its level.", unit: "pct" },
  sub_shape: { label: "Sub shape", name: "Sub shape", does: "The sub oscillator's wave.", unit: "enum", options: ["Sine", "Square"] },
  noise_level: { label: "Level", name: "Noise level", does: "White noise mixed in: its level.", unit: "pct" },
  filter_type: { label: "Filter type", name: "Filter type", does: "The filter: low-pass (12 or 24 dB per octave), high-pass or band-pass.", unit: "enum", options: ["LP 12", "LP 24", "HP", "BP"] },
  filter_cutoff: { label: "Cutoff", name: "Filter cutoff", does: "The filter's cutoff frequency.", unit: "Hz", curve: "log" },
  filter_res: { label: "Res", name: "Filter resonance", does: "The filter's resonance: a peak at the cutoff.", unit: "pct" },
  filter_drive: { label: "Drive", name: "Filter drive", does: "Saturates the sound before the filter.", unit: "pct" },
  filter_env_amt: { label: "Env amt", name: "Filter envelope amount", does: "How far the filter envelope moves the cutoff, up or down.", unit: "bipct" },
  filter_keytrack: { label: "Key trk", name: "Filter key tracking", does: "How much the cutoff follows the key played.", unit: "pct" },
  ...env("amp", "Amp"),
  ...env("flt", "Filter"),
  lfo_shape: { label: "LFO shape", name: "LFO shape", does: "The LFO's wave.", unit: "enum", options: ["Sine", "Triangle", "Saw", "Square", "Random"] },
  lfo_rate: { label: "Rate", name: "LFO rate", does: "How fast the LFO moves.", unit: "rate", curve: "log" },
  lfo_amount: { label: "Amount", name: "LFO amount", does: "How strongly the LFO moves its target.", unit: "pct" },
  lfo_target: { label: "LFO target", name: "LFO target", does: "What the LFO moves.", unit: "enum", options: ["Cutoff", "Pitch", "Shape", "Volume"] },
  glide: { label: "Glide", name: "Glide", does: "How long a note slides from the previous one in mono and legato.", unit: "glide", curve: "sqr" },
  voice_mode: { label: "Voice mode", name: "Voice mode", does: "Poly plays chords; mono plays one note; legato slides between held notes.", unit: "enum", options: ["Poly", "Mono", "Legato"] },
  master: { label: "Volume", name: "Volume", does: "The synth's output level.", unit: "dB" },
};

export interface SynthParam extends ParamRange {
  id: string;
  label: string;
  name: string;
  does: string;
  unit: Unit;
  options?: string[];
}

/** Every synth parameter in the engine's order, with its range and how it is shown. */
export const SYNTH_PARAMS: SynthParam[] = deviceParams.synth.map((p) => {
  const m = META[p.id];
  if (!m) throw new Error(`The synth editor has no place for "${p.id}".`);
  const { curve = "lin", ...rest } = m;
  // `stepped` is the engine's: it rounds these on set (`grooviq_dsp::synth::is_stepped`).
  return { id: p.id, min: p.min, max: p.max, default: p.default, curve, stepped: p.stepped, ...rest };
});

const BY_ID = new Map(SYNTH_PARAMS.map((p) => [p.id, p]));

export function synthParam(id: string): SynthParam {
  const p = BY_ID.get(id);
  if (!p) throw new Error(`The synth has no parameter "${id}".`);
  return p;
}

/** A knob, a small drag field or a row of buttons (an enum). */
export type ControlKind = "knob" | "field" | "segmented";

/** Where a control sits on its card: the card's header, beside its display, or under it. */
export type Slot = "head" | "side" | "foot";

export interface Placement {
  id: string;
  kind: ControlKind;
  slot: Slot;
}

export type SectionId = "header" | "osc1" | "osc2" | "filter" | "voice" | "sub" | "noise" | "amp" | "flt" | "lfo";

export interface Section {
  id: SectionId;
  title: string;
  controls: Placement[];
}

const k = (id: string, slot: Slot = "side"): Placement => ({ id, kind: "knob", slot });
const f = (id: string, slot: Slot = "head"): Placement => ({ id, kind: "field", slot });
const s = (id: string, slot: Slot = "head"): Placement => ({ id, kind: "segmented", slot });

const oscSection = (n: 1 | 2): Section => ({
  id: `osc${n}`,
  title: `Osc ${n}`,
  controls: [
    f(`osc${n}_octave`),
    f(`osc${n}_semi`),
    f(`osc${n}_fine`),
    f(`osc${n}_unison`),
    k(`osc${n}_shape`),
    k(`osc${n}_level`),
    k(`osc${n}_detune`),
    k(`osc${n}_spread`),
  ],
});
const envSection = (w: "amp" | "flt", title: string): Section => ({
  id: w,
  title,
  controls: [f(`${w}_attack`, "foot"), f(`${w}_decay`, "foot"), f(`${w}_sustain`, "foot"), f(`${w}_release`, "foot")],
});

/** The Wave lab layout: header, two oscillators beside the filter and the voice section, then
 * sub and noise, the two envelopes and the LFO. */
export const SYNTH_SECTIONS: Section[] = [
  { id: "header", title: "Synth", controls: [k("master", "head")] },
  oscSection(1),
  oscSection(2),
  {
    id: "filter",
    title: "Filter",
    controls: [
      s("filter_type"),
      k("filter_cutoff", "foot"),
      k("filter_res", "foot"),
      k("filter_drive", "foot"),
      k("filter_env_amt", "foot"),
      k("filter_keytrack", "foot"),
    ],
  },
  { id: "voice", title: "Voice", controls: [s("voice_mode", "side"), k("glide", "foot")] },
  { id: "sub", title: "Sub", controls: [s("sub_shape"), k("sub_level")] },
  { id: "noise", title: "Noise", controls: [k("noise_level")] },
  envSection("amp", "Amp env"),
  envSection("flt", "Filter env"),
  { id: "lfo", title: "LFO", controls: [s("lfo_shape"), k("lfo_rate"), k("lfo_amount"), s("lfo_target", "foot")] },
];

export function section(id: SectionId): Section {
  return SYNTH_SECTIONS.find((x) => x.id === id)!;
}

const MINUS = "−";
const signed = (v: number, digits = 0) => (v > 0 ? "+" : v < 0 ? MINUS : "") + Math.abs(v).toFixed(digits);
const SHAPES = ["Sine", "Tri", "Saw", "Sqr"];

export function formatHz(f: number): string {
  if (f >= 10000) return `${(f / 1000).toFixed(1)} kHz`;
  if (f >= 1000) return `${(f / 1000).toFixed(2)} kHz`;
  return `${Math.round(f)} Hz`;
}

export function formatMs(ms: number): string {
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)} s`;
  return ms >= 100 ? `${Math.round(ms)} ms` : `${ms.toFixed(1)} ms`;
}

/** The oscillator shape as words: "Saw", or "Tri › Saw" between two. */
export function shapeName(v: number): string {
  const i = Math.min(2, Math.floor(v));
  const t = v - i;
  if (t < 0.03) return SHAPES[i];
  if (t > 0.97) return SHAPES[i + 1];
  return `${SHAPES[i]} › ${SHAPES[i + 1]}`;
}

/** A value as the editor shows it, with its unit. */
export function formatValue(p: SynthParam, v: number): string {
  switch (p.unit) {
    case "shape":
      return shapeName(v);
    case "enum":
      return p.options?.[Math.round(v) - p.min] ?? String(v);
    case "oct":
      return v === 0 ? "0" : signed(v);
    case "st":
      return `${v === 0 ? "0" : signed(v)} st`;
    case "ct":
      return `${Math.round(v) === 0 ? "0" : signed(Math.round(v))} ct`;
    case "voices":
      return String(Math.round(v));
    case "bipct":
      return `${Math.round(v * 100) === 0 ? "0" : signed(Math.round(v * 100))}%`;
    case "Hz":
      return formatHz(v);
    case "rate":
      return `${v < 10 ? v.toFixed(2) : v.toFixed(1)} Hz`;
    case "ms":
      return formatMs(v);
    case "glide":
      return v < 0.5 ? "Off" : formatMs(v);
    case "dB":
      return `${Math.abs(v) < 0.05 ? "0.0" : signed(v, 1)} dB`;
    default:
      return `${Math.round(v * 100)}%`;
  }
}
