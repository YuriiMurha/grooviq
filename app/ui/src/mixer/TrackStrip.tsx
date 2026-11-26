import { useCallback, useMemo, type CSSProperties, type KeyboardEvent } from "react";
import { Knob } from "../devices/controls/Knob";
import type { ParamControl } from "../devices/controls/gesture";
import { Fader } from "./Fader";
import { Icon } from "./icons";
import { Options, OutputChoice } from "./OutputChoice";
import {
  availableSends, formatDb, formatPan, formatPercent, LEVEL, PAN, placeholderValue, SEND, unsupportedReason,
  type MixerCommand, type MixerGestures, type MixerProps, type MixerSend, type MixerTrack, type ParamTarget,
} from "./mixerModel";

/** What every strip shares; one stable object for the mixer's lifetime. */
export interface StripActions {
  command: (c: MixerCommand) => void;
  gestures: MixerGestures;
  /** A navigation key on a strip's name (arrows, Home, End). */
  navigate: (fromId: string, key: string) => boolean;
  focused: (id: string) => void;
  /** Registers a strip's meter element for the shared animation loop (null: gone). */
  meter: (id: string, el: HTMLElement | null) => void;
}

interface StripProps {
  track: MixerTrack;
  actions: StripActions;
  nodes: ReadonlyMap<string, MixerTrack>;
  unsupported: MixerProps["unsupported"];
  masterOutput?: string;
  index?: number;
  level: number;
  pan: number;
  selected: boolean;
  /** The one strip name in the tab order. */
  tabStop: boolean;
  /** Absolute position in a windowed row; omitted for the pinned master. */
  left?: number;
}

function control(gestures: MixerGestures, target: ParamTarget): ParamControl {
  return { begin: () => gestures.begin(target), set: (v) => gestures.set(target, v) };
}

const TICKS = [0, -15, -30, -45, -60];

/** One vertical channel strip. Values come from the host (or a gesture in flight). */
export function TrackStrip({ track, actions, nodes, unsupported, masterOutput, index, level, pan, selected, tabStop, left }: StripProps) {
  const { id, name, pending } = track;
  const master = masterOutput !== undefined;
  const levelControl = useMemo(() => control(actions.gestures, { kind: "level", nodeId: id }), [actions, id]);
  const panControl = useMemo(() => control(actions.gestures, { kind: "pan", nodeId: id }), [actions, id]);
  const meterRef = useCallback((el: HTMLElement | null) => actions.meter(id, el), [actions, id]);
  const muteReason = unsupportedReason(unsupported, "mute");
  const soloReason = unsupportedReason(unsupported, "solo");
  const destinations = availableSends(track.sendDestinations, track.sends);
  const addPlaceholder = placeholderValue(destinations);

  const onNameKey = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (actions.navigate(id, e.key)) e.preventDefault();
  };

  return (
    <section
      className={`mx-strip${selected ? " selected" : ""}${master ? " master" : ""}${pending ? " pending" : ""}`}
      style={{ "--ink": `var(--ink-${track.ink})`, left } as CSSProperties}
      aria-label={name}
      aria-busy={pending || undefined}
      data-node={id}
      onFocus={() => actions.focused(id)}
    >
      <button
        type="button"
        className="mx-pick"
        aria-pressed={selected}
        aria-label={name}
        aria-disabled={pending || undefined}
        tabIndex={tabStop ? 0 : -1}
        title={name}
        onClick={() => actions.command({ type: "select", nodeId: id })}
        onKeyDown={onNameKey}
      >
        <span className="mx-kind" title={track.kind}>
          <span className="mx-index">{master ? "LR" : String(index).padStart(2, "0")}</span>
          {track.kind === "Instrument" ? "Inst." : track.kind}
        </span>
        <strong className="mx-name">{name}</strong>
      </button>
      <div className="mx-tools">
        <button type="button" className="mx-tool" aria-pressed={track.mute} aria-label={`Mute ${name}`}
          title={muteReason ?? `Mute ${name}`} aria-description={muteReason} disabled={pending || muteReason !== undefined}
          onClick={() => actions.command({ type: "mute", nodeId: id, on: !track.mute })}>
          M
        </button>
        <button type="button" className="mx-tool solo" aria-pressed={track.solo} aria-label={`Solo ${name}`}
          title={soloReason ?? `Solo ${name}`} aria-description={soloReason} disabled={pending || soloReason !== undefined}
          onClick={() => actions.command({ type: "solo", nodeId: id, on: !track.solo })}>
          S
        </button>
      </div>
      <div className="mx-bank">
        <Fader control={levelControl} param={LEVEL} value={level} name={`${name} level`} format={formatDb} disabled={pending} />
        <div className="mx-scale" aria-hidden="true">
          {TICKS.map((tick) => <span key={tick} style={{ bottom: `${((tick - LEVEL.min) / (LEVEL.max - LEVEL.min)) * 100}%` }}>{tick}</span>)}
        </div>
        <div className="mx-meter" ref={meterRef} role="img" aria-label={`${name} meter`}>
          <span className="mx-meter-bar" />
          <span className="mx-meter-bar" />
        </div>
        <button
          type="button"
          className={`mx-clip${track.clipped ? " on" : ""}`}
          aria-label={track.clipped ? `Clear ${name} clip` : `${name} has not clipped`}
          title={track.clipped ? `Clear ${name} clip` : undefined}
          disabled={pending || !track.clipped}
          onClick={() => actions.command({ type: "clearClip", nodeId: id })}
        />
      </div>
      <output className="mx-db" aria-hidden="true">{formatDb(level)}</output>
      <div className="mx-pan" style={{ "--pan": (pan + 1) / 2 } as CSSProperties} inert={pending || undefined}>
        <Knob control={panControl} id={`pan-${id}`} param={PAN} value={pan} label="Pan" name={`${name} pan`}
          format={formatPan} does="Places the track between left and right." variant="field" row />
      </div>
      <div className="mx-out">
        {master ? <><Icon name="arrow" /><span title={masterOutput}>{masterOutput}</span></> : <OutputChoice track={track} nodes={nodes} command={actions.command} compact />}
      </div>
      {!master && (
        <section className="mx-sends" aria-label={`${name} sends`}>
          <h3>Sends</h3>
          <div className="mx-send-list">
            {track.sends.map((send) => (
              <SendRow key={send.id} nodeId={id} send={send}
                name={track.sendDestinations.find((d) => d.id === send.destinationId)?.name ?? nodes.get(send.destinationId)?.name ?? "Unavailable destination"}
                pending={pending} actions={actions} />
            ))}
            {track.sends.length === 0 && <p className="mx-mini">No sends</p>}
          </div>
          <select
            className="mx-add-send"
            aria-label={`Add send from ${name}`}
            title={destinations.length ? "Add send" : "No destinations available"}
            value={addPlaceholder}
            disabled={pending || destinations.length === 0}
            onChange={(e) => {
              const destination = destinations.find((option) => option.id === e.currentTarget.value);
              if (destination) actions.command({ type: "addSend", nodeId: id, destinationId: destination.id, pre: false, amount: 0 });
            }}
          >
            <option value={addPlaceholder} disabled>Add send</option>
            <Options options={destinations} />
          </select>
        </section>
      )}
    </section>
  );
}

function SendRow({ nodeId, send, name, pending, actions }: { nodeId: string; send: MixerSend; name: string; pending?: boolean; actions: StripActions }) {
  const { id: sendId } = send;
  const { gestures, command } = actions;
  const ctl = useMemo(() => control(gestures, { kind: "send", nodeId, sendId }), [gestures, nodeId, sendId]);
  const amount = gestures.shown({ kind: "send", nodeId, sendId }) ?? send.amount;
  return (
    <div className="mx-send" role="group" aria-label={`Send to ${name}`}>
      <div inert={pending || undefined}>
        <Knob control={ctl} id={sendId} param={SEND} value={amount} label="" name={`Send to ${name} amount`}
          format={formatPercent} does="Sets the level of this send." size={29} />
      </div>
      <strong title={name}>{name}</strong>
      <div className="mx-send-tools">
        <button type="button" className="mx-tool" aria-label={`Pre-fader send to ${name}`}
          aria-pressed={send.pre} title={send.pre ? "Switch to post-fader" : "Switch to pre-fader"} disabled={pending}
          onClick={() => command({ type: "setSendPre", nodeId, sendId, pre: !send.pre })}>
          {send.pre ? "Pre" : "Post"}
        </button>
        <button type="button" className="mx-icon-btn" aria-label={`Remove send to ${name}`} title="Remove send" disabled={pending}
          onClick={() => command({ type: "removeSend", nodeId, sendId })}>
          <Icon name="remove" />
        </button>
      </div>
    </div>
  );
}
