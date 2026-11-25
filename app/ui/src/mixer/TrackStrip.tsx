import { memo, useCallback, useMemo, type CSSProperties, type KeyboardEvent } from "react";
import { Knob } from "../devices/controls/Knob";
import type { ParamControl } from "../devices/controls/gesture";
import { Fader } from "./Fader";
import { Icon } from "./icons";
import { formatDb, formatPan, LEVEL, PAN, type MixerCommand, type MixerGestures, type MixerTrack, type ParamTarget } from "./mixerModel";

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
  master?: boolean;
  /** The output's name or the master's device label. */
  outputLabel: string;
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

/** One vertical channel strip. Values come from the host (or a gesture in flight); every change
 * goes out as a command on the strip's id. */
export const TrackStrip = memo(function TrackStrip({ track, actions, master, outputLabel, level, pan, selected, tabStop, left }: StripProps) {
  const { id, name, pending } = track;
  const levelControl = useMemo(() => control(actions.gestures, { kind: "level", nodeId: id }), [actions, id]);
  const panControl = useMemo(() => control(actions.gestures, { kind: "pan", nodeId: id }), [actions, id]);
  const meterRef = useCallback((el: HTMLElement | null) => actions.meter(id, el), [actions, id]);
  const shownInserts = track.inserts.slice(0, 2);
  const more = track.inserts.length - shownInserts.length;

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
      <span className="mx-ink" aria-hidden="true" />
      <button
        type="button"
        className="mx-pick"
        aria-pressed={selected}
        aria-label={name}
        tabIndex={tabStop ? 0 : -1}
        title={name}
        data-hint="Selects this track for its inserts and sends. Left and Right move between tracks."
        onClick={() => actions.command({ type: "select", nodeId: id })}
        onKeyDown={onNameKey}
      >
        <span className="mx-kind">{track.kind}</span>
        <strong className="mx-name">{name}</strong>
      </button>
      <ul className="mx-slots" aria-label={`${name} inserts`}>
        {shownInserts.map((s) => (
          <li key={s.id} className={s.active ? undefined : "bypassed"} title={s.name}>
            {s.name}
          </li>
        ))}
        {more > 0 && <li className="mx-more">+{more} more</li>}
        {track.inserts.length === 0 && <li className="mx-none">No inserts</li>}
      </ul>
      <div className="mx-tools">
        <button
          type="button"
          className="mx-tool"
          aria-pressed={track.mute}
          aria-label={`Mute ${name}`}
          disabled={pending}
          onClick={() => actions.command({ type: "mute", nodeId: id, on: !track.mute })}
        >
          M
        </button>
        <button
          type="button"
          className="mx-tool solo"
          aria-pressed={track.solo}
          aria-label={`Solo ${name}`}
          disabled={pending}
          onClick={() => actions.command({ type: "solo", nodeId: id, on: !track.solo })}
        >
          S
        </button>
        <button
          type="button"
          className={`mx-tool clip${track.clipped ? " on" : ""}`}
          aria-label={track.clipped ? `Clear ${name} clip` : `${name} has not clipped`}
          disabled={pending || !track.clipped}
          onClick={() => actions.command({ type: "clearClip", nodeId: id })}
        >
          Clip
        </button>
      </div>
      <div className="mx-bank">
        <Fader control={levelControl} param={LEVEL} value={level} name={`${name} level`} format={formatDb} disabled={pending} />
        <div className="mx-meter" ref={meterRef} role="img" aria-label={`${name} meter`}>
          <span className="mx-meter-bar" />
          <span className="mx-meter-bar" />
        </div>
      </div>
      <output className="mx-db" aria-hidden="true">
        {formatDb(level)}
      </output>
      <div className="mx-pan" aria-disabled={pending || undefined}>
        <Knob
          control={panControl}
          id={`pan-${id}`}
          param={PAN}
          value={pan}
          label="Pan"
          name={`${name} pan`}
          format={formatPan}
          does="Places the track between left and right."
          row
          size={26}
        />
      </div>
      <p className="mx-out" title={outputLabel}>
        {!master && <Icon name="arrow" />}
        <span>{outputLabel}</span>
      </p>
    </section>
  );
});
