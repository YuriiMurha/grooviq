import { OutputScope } from "./OutputScope";
import type { ReactNode } from "react";
import { SYNTH_PRESETS } from "../../api/notes";
import type { ChannelView } from "../../api/project";
import { EnvelopeGraph } from "../controls/EnvelopeGraph";
import type { ParamControl } from "../controls/gesture";
import { Knob } from "../controls/Knob";
import { useAutomationCreation, useAutomationGestureGuard } from "../../playlist/automation/creationContext";
import { Segmented } from "../controls/Segmented";
import { DeviceWindow } from "../DeviceWindow";
import { useDeviceParams } from "../useDeviceParams";
import { OscDisplay } from "./OscDisplay";
import { FilterDisplay, GlideDisplay, LfoDisplay } from "./SynthDisplays";
import { formatValue, section, synthParam, type Placement, type SectionId, type Slot } from "./synthLayout";
import { lfoWave, randomSteps } from "./waveShapes";

interface Props {
  channel: ChannelView;
  active: boolean;
  generation: number;
  index: number;
  z: number;
  opener: HTMLElement | null;
  onClose: () => void;
  onRaise: () => void;
  onError: (message: string) => void;
}

/** A small drawing of a wave for an enum button (LFO and sub shapes). */
function glyph(kind: number | "random"): ReactNode {
  const pts: string[] = [];
  if (kind === "random") {
    const steps = randomSteps(4);
    steps.forEach((v, i) => pts.push(`${i ? "L" : "M"}${1 + i * 3} ${8 - v * 5}L${1 + (i + 1) * 3} ${8 - v * 5}`));
  } else {
    for (let i = 0; i <= 24; i++) pts.push(`${i ? "L" : "M"}${1 + i / 2} ${(8 - lfoWave(kind, i / 24) * 5).toFixed(2)}`);
  }
  return (
    <svg className="dseg-glyph" viewBox="0 0 14 16" aria-hidden="true">
      <path d={pts.join("")} />
    </svg>
  );
}
const LFO_GLYPHS = [glyph(0), glyph(1), glyph(2), glyph(3), glyph("random")];
const SUB_GLYPHS = [glyph(0), glyph(3)];

/** The basic synth's editor, "Wave lab": two oscillators drawn large that morph with their
 * shape, the filter curve, two envelopes with points to drag, the LFO and the voice section;
 * the presets and the volume in the title bar. Every one of the 40 parameters has a control. */
export function SynthEditor({ generation, active, channel, index, z, opener, onClose, onRaise, onError }: Props) {
  const params = useDeviceParams(channel.id, channel.synth, (id) => synthParam(id).default, onError);
  const v = params.value;
  const automation = useAutomationCreation();
  useAutomationGestureGuard(params.hasActiveChanges);
  const ctl = (id: string): ParamControl => ({ begin: () => params.begin(id), set: (x) => params.set(id, x) });

  const control = ({ id, kind }: Placement, opts: { row?: boolean; size?: number } = {}) => {
    const p = synthParam(id);
    if (kind === "segmented") {
      return (
        <Segmented
          key={id}
          id={id}
          name={p.name}
          options={p.options ?? []}
          icons={id === "lfo_shape" ? LFO_GLYPHS : id === "sub_shape" ? SUB_GLYPHS : undefined}
          min={p.min}
          value={v(id)}
          does={p.does}
          onChange={(x) => params.set(id, x)}
          className={id === "voice_mode" || id === "lfo_target" ? "full" : undefined}
        />
      );
    }
    return (
      <Knob
        key={id}
        id={id}
        param={p}
        value={v(id)}
        label={p.label}
        name={p.name}
        format={(x) => formatValue(p, x)}
        does={p.does}
        variant={kind === "field" ? "field" : "knob"}
        row={opts.row}
        size={opts.size}
        control={ctl(id)}
        onContextAction={automation ? (anchor, at) =>
          automation.open({ type: "instrumentParam", channel: channel.id, instrument: "synth", param: id }, v(id), anchor, at) : undefined}
      />
    );
  };
  const slot = (id: SectionId, where: Slot, opts?: { row?: boolean; size?: number }) =>
    section(id)
      .controls.filter((c) => c.slot === where)
      .map((c) => control(c, opts));

  const card = (id: SectionId, className: string, body: ReactNode, head?: ReactNode) => (
    <section className={`dcard ${className}`} aria-label={section(id).title}>
      <div className="dcard-head">
        <b className="dcard-title">{section(id).title}</b>
        <span className="dspacer" />
        {head ?? slot(id, "head")}
      </div>
      {body}
    </section>
  );

  const osc = (n: 1 | 2) => {
    const pre = `osc${n}_`;
    return card(
      `osc${n}`,
      `osc o${n}`,
      <div className="osc-body">
        <OscDisplay
          n={n}
          shapeParam={synthParam(`${pre}shape`)}
          shape={v(`${pre}shape`)}
          level={v(`${pre}level`)}
          unison={Math.round(v(`${pre}unison`))}
          detune={v(`${pre}detune`)}
          spread={v(`${pre}spread`)}
          control={ctl(`${pre}shape`)}
        />
        <div className="osc-knobs">{slot(`osc${n}`, "side", { row: true, size: 27 })}</div>
      </div>,
    );
  };

  const env = (w: "amp" | "flt") =>
    card(
      w,
      `env ${w}`,
      <>
        <EnvelopeGraph
          meter={w}
          name={w === "amp" ? "Amp envelope" : "Filter envelope"}
          attack={{ id: `${w}_attack`, param: synthParam(`${w}_attack`), value: v(`${w}_attack`) }}
          decay={{ id: `${w}_decay`, param: synthParam(`${w}_decay`), value: v(`${w}_decay`) }}
          sustain={{ id: `${w}_sustain`, param: synthParam(`${w}_sustain`), value: v(`${w}_sustain`) }}
          release={{ id: `${w}_release`, param: synthParam(`${w}_release`), value: v(`${w}_release`) }}
          group={params.group}
        />
        <div className="env-fields">{slot(w, "foot")}</div>
      </>,
      <span />,
    );

  const presets = (
    <nav className="synth-presets" aria-label="Presets">
      {SYNTH_PRESETS.map((name) => (
        <button
          key={name}
          type="button"
          className="preset"
          aria-pressed={channel.preset === name}
          data-autofocus={channel.preset === name ? true : undefined}
          data-hint={`Loads the ${name} preset: every setting of this synth changes.`}
          onClick={() => {
            // A preset replaces every value: `send` ends running gestures first.
            void params.send({ kind: "loadSynthPreset", channel: channel.id, preset: name });
          }}
        >
          {name}
        </button>
      ))}
    </nav>
  );

  return (
    <DeviceWindow
      title={channel.name}
      device="Synth"
      color={channel.color}
      index={index}
      z={z}
      opener={opener}
      onClose={onClose}
      onRaise={onRaise}
      toolbar={
        <>
          {presets}
          <span className="dspacer" />
          <div className="synth-volume">{slot("header", "head", { row: true, size: 26 })}</div>
        </>
      }
    >
      <div className="synth">
        <div className="synth-r1">
          <div className="synth-col">
            {osc(1)}
            {osc(2)}
          </div>
          {card(
            "filter",
            "filter",
            <>
              <FilterDisplay
                type={Math.round(v("filter_type"))}
                cutoff={{ id: "filter_cutoff", param: synthParam("filter_cutoff"), value: v("filter_cutoff") }}
                res={{ id: "filter_res", param: synthParam("filter_res"), value: v("filter_res") }}
                drive={v("filter_drive")}
                envAmount={v("filter_env_amt")}
                group={params.group}
              />
              <div className="filter-knobs">{slot("filter", "foot", { size: 30 })}</div>
            </>,
          )}
          {card(
            "voice",
            "voice",
            <>
              {slot("voice", "side")}
              <OutputScope channel={channel.id} active={active} generation={generation} />
              <div className="voice-glide">
                {slot("voice", "foot", { size: 34 })}
                <GlideDisplay glide={v("glide")} max={synthParam("glide").max} />
              </div>
            </>,
          )}
        </div>
        <div className="synth-r2">
          <div className="synth-col sub-noise">
            {card("sub", "sub", <div className="sn-knob">{slot("sub", "side", { row: true, size: 28 })}</div>)}
            {card("noise", "noise", <div className="sn-knob">{slot("noise", "side", { row: true, size: 28 })}</div>)}
          </div>
          {env("amp")}
          {env("flt")}
          {card(
            "lfo",
            "lfo",
            <>
              <div className="lfo-body">
                <LfoDisplay
                  shape={Math.round(v("lfo_shape"))}
                  rate={v("lfo_rate")}
                  amountParam={synthParam("lfo_amount")}
                  amount={v("lfo_amount")}
                  control={ctl("lfo_amount")}
                />
                <div className="lfo-knobs">{slot("lfo", "side", { row: true, size: 26 })}</div>
              </div>
              {slot("lfo", "foot")}
            </>,
          )}
        </div>
      </div>
    </DeviceWindow>
  );
}

