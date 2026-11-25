import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { channelWaveform } from "../../api/notes";
import { useSampleRevision } from "../../rack/sampleRevision";
import type { ChannelView } from "../../api/project";
import { EnvelopeGraph } from "../controls/EnvelopeGraph";
import type { ParamControl } from "../controls/gesture";
import { Knob } from "../controls/Knob";
import { Segmented } from "../controls/Segmented";
import { Toggle } from "../controls/Toggle";
import { DeviceWindow } from "../DeviceWindow";
import { FilterDisplay } from "../synth/SynthDisplays";
import { FILTER_BP, FILTER_HP, FILTER_LP12 } from "../synth/waveShapes";
import { useDeviceParams } from "../useDeviceParams";
import { CHOKE_GROUPS, formatValue, samplerParam, section, type Placement, type SectionId, type Slot } from "./samplerLayout";
import { WaveformView, type MarkerInfo } from "./WaveformView";
import { trimRepairState } from "./trimRepair";
import { useTrimRepair } from "./useTrimRepair";
import "./trimRepair.css";
import { WAVEFORM_POINTS, decodePeaks, type Marker } from "./waveform";

interface Props {
  channel: ChannelView;
  index: number;
  z: number;
  opener: HTMLElement | null;
  onClose: () => void;
  onRaise: () => void;
  onError: (message: string) => void;
  /** Asks for a file for the channel's sample (a missing one, or none yet). */
  onRelink: (channel: ChannelView) => void;
}

/** The sampler's filter types (off, low-, high-, band-pass) as the display draws them. */
const FILTER_SHAPES = [null, FILTER_LP12, FILTER_HP, FILTER_BP];
const CHOKE_OPTIONS = ["Off", ...Array.from({ length: CHOKE_GROUPS }, (_, i) => String(i + 1))];
const MARKERS: Marker[] = ["start", "end", "loop_start"];

/** The channel's waveform peaks: fetched once per sample (not again on a resize or an edit),
 * `null` while on its way, `"none"` when there is no sample to draw. A relink is another sample
 * even under the same file name. */
function usePeaks(channel: number, sample: string | null, missing: boolean): Float32Array | null | "none" {
  const revision = useSampleRevision(channel);
  const has = sample !== null && !missing;
  const key = `${channel}\n${sample ?? ""}\n${missing}\n${revision}`;
  const [got, setGot] = useState<{ key: string; peaks: Float32Array | "none" } | null>(null);
  useEffect(() => {
    if (!has) return;
    let live = true;
    channelWaveform(channel, WAVEFORM_POINTS).then(
      (buf) => live && setGot({ key, peaks: decodePeaks(buf) }),
      // The engine has no sample to draw after all: offer to choose one.
      () => live && setGot({ key, peaks: "none" }),
    );
    return () => {
      live = false;
    };
  }, [channel, key, has]);
  if (!has) return "none";
  return got?.key === key ? got.peaks : null;
}

/** The sampler's editor: the sample's waveform with its region and loop markers, pitch, the two
 * envelopes either side of the filter, the voice mode with glide and the choke group, and
 * velocity; gain and pan in the title bar. Every one of the 29 parameters has a control. */
export function SamplerEditor({ channel, index, z, opener, onClose, onRaise, onError, onRelink }: Props) {
  const fileDetails = useRef<HTMLElement>(null);
  const recoveryFocus = useCallback((button: HTMLButtonElement | null) => {
    if (!button) return;
    return () => {
      // The recovery control disappears when peaks arrive. Move only its own focus,
      // never focus held elsewhere while the picker or waveform request was pending.
      if (document.activeElement === button && fileDetails.current?.isConnected) {
        fileDetails.current.focus({ preventScroll: true });
      }
    };
  }, []);
  const params = useDeviceParams(channel.id, channel.sampler, (id) => samplerParam(id).default, onError);
  const v = params.value;
  const revision = useSampleRevision(channel.id);
  const repair = useTrimRepair(`${channel.id}:${channel.sampleName}:${channel.missing}:${revision}`, channel.sampler.start ?? 0, channel.sampler.end ?? 1, channel.sampler, params.trim, onError);
  const diagnosis = trimRepairState({ start:v("start"), end:v("end"), frames:channel.missing || !channel.sampleName ? null : channel.sampleFrames, loopStart:v("loop_start"), reverse:v("reverse") >= 0.5 });
  const ctl = (id: string): ParamControl => ({ begin: () => params.begin(id), set: (x) => params.set(id, x) });
  const peaks = usePeaks(channel.id, channel.sampleName, channel.missing);
  // A picked choke group shows at once, until the project has it.
  const [choke, setChoke] = useState<number | null>(null);

  const control = ({ id, kind }: Placement, opts: { row?: boolean; size?: number } = {}) => {
    const p = samplerParam(id);
    if (kind === "segmented") {
      return (
        <Segmented
          key={id}
          id={id}
          name={p.name}
          options={p.options ?? []}
          min={p.min}
          value={v(id)}
          does={p.does}
          onChange={(x) => params.set(id, x)}
          className={id === "voice_mode" ? "full" : undefined}
        />
      );
    }
    if (kind === "toggle") {
      return <Toggle key={id} id={id} label={p.label} does={p.does} on={v(id) >= 0.5} onChange={(on) => params.set(id, on ? 1 : 0)} />;
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
      />
    );
  };
  const slot = (id: SectionId, where: Slot, opts?: { row?: boolean; size?: number }) =>
    section(id)
      .controls.filter((c) => c.slot === where && c.kind !== "marker")
      .map((c) => control(c, opts));

  const card = (id: SectionId, className: string, body: ReactNode, head?: ReactNode, lead?: ReactNode) => (
    <section className={`dcard ${className}`} aria-label={section(id).title}>
      <div className="dcard-head">
        <b className="dcard-title">{section(id).title}</b>
        {lead}
        <span className="dspacer" />
        {head ?? slot(id, "head")}
      </div>
      {body}
    </section>
  );

  const env = (w: "amp" | "flt") => {
    const pre = w === "amp" ? "" : "flt_";
    const at = (k: string) => ({ id: `${pre}${k}`, param: samplerParam(`${pre}${k}`), value: v(`${pre}${k}`) });
    return card(
      w,
      `env ${w}`,
      <>
        <EnvelopeGraph
          name={w === "amp" ? "Amp envelope" : "Filter envelope"}
          attack={at("attack")}
          decay={at("decay")}
          sustain={at("sustain")}
          release={at("release")}
          group={params.group}
        />
        <div className="env-fields">{slot(w, "foot")}</div>
      </>,
      <span />,
    );
  };

  const markers = Object.fromEntries(
    MARKERS.map((id) => {
      const p = samplerParam(id);
      const info: MarkerInfo = { id, name: p.name, does: p.does, format: (x) => formatValue(p, x), default: p.default };
      return [id, info];
    }),
  ) as Record<Marker, MarkerInfo>;

  const missing = peaks === "none";
  const wave = (
    <WaveformView
      name={channel.name}
      peaks={missing ? null : (peaks ?? EMPTY)}
      region={{ start: v("start"), end: v("end"), loopStart: v("loop_start"), reverse: v("reverse") >= 0.5, frames: channel.sampleFrames }}
      loop={v("loop") >= 0.5}
      markers={markers}
      busy={repair.busy}
      onTrimIntent={repair.invalidate}
      begin={(id) => params.begin(id)}
      set={(id, x) => params.set(id, x)}
    >
      {missing && (
        <div className="sample-missing">
          <span>{channel.missing ? `${channel.sampleName ?? "The sample"} is missing.` : "No sample yet."}</span>
          <button
            type="button"
            className="dbutton"
            ref={recoveryFocus}
            aria-label={channel.missing ? `Relink ${channel.name}` : `Choose a sample for ${channel.name}`}
            data-hint={`Choose the file ${channel.name} should play.`}
            onClick={(event) => {
              // WebKit pointer activation does not focus buttons. Keep Escape in this window
              // when the picker is cancelled, without taking focus on an async reply.
              event.currentTarget.focus({ preventScroll: true });
              onRelink(channel);
            }}
          >
            {channel.missing ? "Relink…" : "Choose a sample…"}
          </button>
        </div>
      )}
    </WaveformView>
  );

  const shownChoke = choke ?? channel.choke;
  const pickChoke = (group: number) => {
    setChoke(group);
    void params.send({ kind: "setChoke", channel: channel.id, group }).then(() => setChoke((c) => (c === group ? null : c)));
  };

  return (
    <DeviceWindow
      title={channel.name}
      device="Sampler"
      color={channel.color}
      index={index}
      z={z}
      opener={opener}
      onClose={onClose}
      onRaise={onRaise}
      toolbar={
        <>
          <span className="dspacer" />
          <div className="sampler-out">{slot("header", "head", { row: true, size: 26 })}</div>
        </>
      }
    >
      <div className="sampler">
        <div className="sampler-r1">
          {card(
            "sample",
            "sample",
            <>
              {wave}
              {(diagnosis.damaged || diagnosis.wholeFrame || repair.offer || repair.busy) && <div className="sampler-trim-repair">
                <span role="status">{repair.busy ? "Updating trim…" : diagnosis.stuck ? "This saved trim needs repair. Neither marker can leave a full frame." : diagnosis.damaged ? "This saved trim is shorter than a frame or crossed. Move a legal marker or use the whole sample." : diagnosis.wholeFrame ? "Whole sample selected. A one-frame sample cannot be trimmed smaller." : "Whole sample selected."}</span>
                {diagnosis.damaged && <button type="button" className="dbutton" disabled={repair.busy || !repair.ready} onClick={repair.reset}>Use whole sample</button>}
                {repair.offer && <button type="button" className="dbutton" disabled={repair.busy} onClick={repair.undo}>Undo reset</button>}
              </div>}
              <details className="sample-file-details">
                <summary ref={fileDetails} tabIndex={0}>File details</summary>
                <dl>
                  <dt>{channel.missing ? "Stored reference" : "File"}</dt><dd>{channel.sampleName ?? "No sample"}</dd>
                  <dt>Status</dt><dd>{channel.missing ? "Missing — locate the file to restore playback." : channel.sampleName ? "Loaded" : "No sample yet"}</dd>
                  {channel.sampleFrames !== null && <><dt>Length</dt><dd>{channel.sampleFrames.toLocaleString()} frames</dd></>}
                </dl>
              </details>
            </>,
            <div className="sample-toggles">{slot("sample", "head")}</div>,
            <span className="sample-name">{channel.sampleName ?? ""}</span>,
          )}
          {card(
            "pitch",
            "pitch",
            <>
              <div className="pitch-body">{slot("pitch", "side", { size: 32 })}</div>
              {slot("pitch", "foot")}
            </>,
          )}
        </div>
        <div className="sampler-r2">
          {env("amp")}
          {card(
            "filter",
            "filter",
            <>
              <FilterDisplay
                type={FILTER_SHAPES[Math.round(v("filter_type"))] ?? null}
                cutoff={{ id: "cutoff", param: samplerParam("cutoff"), value: v("cutoff") }}
                res={{ id: "res", param: samplerParam("res"), value: v("res") }}
                drive={0}
                envAmount={v("filter_env")}
                group={params.group}
              />
              <div className="filter-knobs">{slot("filter", "foot", { size: 30 })}</div>
            </>,
          )}
          {env("flt")}
          <div className="sampler-col">
            {card(
              "voice",
              "voice",
              <>
                {slot("voice", "side")}
                <div className="voice-glide">{slot("voice", "foot", { row: true, size: 28 })}</div>
                <Segmented
                  name="Choke group"
                  options={CHOKE_OPTIONS}
                  min={0}
                  value={shownChoke}
                  does="Channels in the same choke group cut each other off, like an open and a closed hat."
                  onChange={pickChoke}
                  className="choke"
                />
              </>,
            )}
            {card("velocity", "velocity", <div className="vel-knobs">{slot("velocity", "side", { size: 30 })}</div>)}
          </div>
        </div>
      </div>
    </DeviceWindow>
  );
}

const EMPTY = new Float32Array(0);
