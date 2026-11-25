import { useMemo } from "react";
import { Knob } from "../devices/controls/Knob";
import { Segmented } from "../devices/controls/Segmented";
import { Toggle } from "../devices/controls/Toggle";
import type { ParamControl } from "../devices/controls/gesture";
import { Icon } from "./icons";
import { formatPercent, moveTarget, SEND, type MixerError, type MixerInsert, type MixerOption, type MixerSend, type MixerTrack } from "./mixerModel";
import type { StripActions } from "./TrackStrip";

interface InspectorProps {
  /** The selected track or master; null for no selection or a removed one. */
  track: MixerTrack | null;
  master: boolean;
  /** The master's device label. */
  masterOutput: string;
  /** A selection was supplied but its track is gone. */
  removed: boolean;
  effects: readonly MixerOption[];
  error: MixerError | null;
  actions: StripActions;
}

/** The selected track's output, insert chain and sends. */
export function Inspector({ track, master, masterOutput, removed, effects, error, actions }: InspectorProps) {
  const notice = error && (error.nodeId === null || error.nodeId === track?.id) ? error.message : "";

  if (!track) {
    return (
      <aside className="mx-inspector empty" aria-label="Selected mixer track">
        <p className="mx-empty">{removed ? "The selected track is no longer in the mixer. Select another strip." : "Select a strip to see its inserts and sends."}</p>
        <p className="mx-notice" role="status">
          {notice}
        </p>
      </aside>
    );
  }

  const { id, pending } = track;
  const { command } = actions;
  const sentTo = new Set(track.sends.map((s) => s.destinationId));
  const freeDestinations = track.sendDestinations.filter((d) => !sentTo.has(d.id) && d.id !== id);

  return (
    <aside className={`mx-inspector${pending ? " pending" : ""}`} aria-label="Selected mixer track" aria-busy={pending || undefined}>
      <header className="mx-ihead">
        <h2>{track.name}</h2>
        <span className="mx-kind">{track.kind}</span>
      </header>

      <div className="mx-routefields">
        <label className="mx-field">
          Main output
          {master ? (
            <select disabled>
              <option>{masterOutput}</option>
            </select>
          ) : (
            <select
              value={track.outputId ?? ""}
              disabled={pending || track.outputs.length === 0}
              onChange={(e) => {
                if (e.target.value) command({ type: "setOutput", nodeId: id, outputId: e.target.value });
              }}
            >
              {track.outputId === null && <option value="">{track.outputs.length ? "Choose an output…" : "No outputs available"}</option>}
              {track.outputs.map((o) => (
                <option key={o.id} value={o.id}>
                  {o.name}
                </option>
              ))}
            </select>
          )}
        </label>

        <label className="mx-field">
          Insert chain
          <select
            value=""
            disabled={pending || effects.length === 0}
            onChange={(e) => {
              if (e.target.value) command({ type: "addInsert", nodeId: id, effectId: e.target.value });
            }}
          >
            <option value="">{effects.length ? "Add an effect…" : "No effects available"}</option>
            {effects.map((fx) => (
              <option key={fx.id} value={fx.id}>
                {fx.name}
              </option>
            ))}
          </select>
        </label>
        {track.inserts.length === 0 ? (
          <p className="mx-mini">No effects. Add one to begin a chain.</p>
        ) : (
          <ol className="mx-inserts">
            {track.inserts.map((slot, i) => (
              <InsertRow key={slot.id} nodeId={id} slot={slot} slots={track.inserts} first={i === 0} last={i === track.inserts.length - 1} pending={!!pending} command={command} />
            ))}
          </ol>
        )}
      </div>

      <section className="mx-sends" aria-label="Sends">
        <h3>Sends</h3>
        {master ? (
          <p className="mx-mini">The master has no sends.</p>
        ) : (
          <>
            {track.sends.length === 0 && <p className="mx-mini">No sends.</p>}
            {track.sends.map((s) => (
              <SendRow key={s.id} nodeId={id} send={s} name={track.sendDestinations.find((d) => d.id === s.destinationId)?.name ?? "Unavailable destination"} pending={!!pending} actions={actions} />
            ))}
            <label className="mx-field">
              Add a send
              <select
                value=""
                disabled={pending || freeDestinations.length === 0}
                onChange={(e) => {
                  if (e.target.value) command({ type: "addSend", nodeId: id, destinationId: e.target.value, pre: false, amount: 0 });
                }}
              >
                <option value="">{freeDestinations.length ? "Send to…" : "No destinations available"}</option>
                {freeDestinations.map((d) => (
                  <option key={d.id} value={d.id}>
                    {d.name}
                  </option>
                ))}
              </select>
            </label>
          </>
        )}
      </section>

      <p className="mx-notice" role="status">
        {notice}
      </p>
    </aside>
  );
}

interface InsertRowProps {
  nodeId: string;
  slot: MixerInsert;
  slots: readonly MixerInsert[];
  first: boolean;
  last: boolean;
  pending: boolean;
  command: StripActions["command"];
}

function InsertRow({ nodeId, slot, slots, first, last, pending, command }: InsertRowProps) {
  const slotId = slot.id;
  const move = (dir: -1 | 1) => {
    const to = moveTarget(slots, slotId, dir);
    if (to) command({ type: "moveInsert", nodeId, slotId, ...to });
  };
  return (
    <li className={`mx-insert${slot.active ? "" : " bypassed"}`} aria-label={slot.name}>
      <div className="mx-insert-row">
        <fieldset className="mx-bare" disabled={pending}>
          <Toggle id={`insert-${slotId}`} label="On" does={`Switches ${slot.name} on or bypasses it.`} on={slot.active} onChange={(active) => command({ type: "setInsertActive", nodeId, slotId, active })} />
        </fieldset>
        <button type="button" className="mx-open" title={slot.name} aria-label={`Open ${slot.name}`} onClick={() => command({ type: "openInsert", nodeId, slotId })}>
          <span>{slot.name}</span>
        </button>
        <button type="button" className="mx-icon-btn" aria-label={`Move ${slot.name} earlier`} disabled={pending || first} onClick={() => move(-1)}>
          <Icon name="up" />
        </button>
        <button type="button" className="mx-icon-btn" aria-label={`Move ${slot.name} later`} disabled={pending || last} onClick={() => move(1)}>
          <Icon name="down" />
        </button>
        <button type="button" className="mx-icon-btn" aria-label={`Remove ${slot.name}`} disabled={pending} onClick={() => command({ type: "removeInsert", nodeId, slotId })}>
          <Icon name="remove" />
        </button>
      </div>
      {slot.detector && (
        <label className="mx-field mx-detector">
          Sidechain input
          <select
            value={slot.detector.sourceId ?? ""}
            disabled={pending}
            onChange={(e) => command({ type: "setDetector", nodeId, slotId, sourceId: e.target.value || null })}
          >
            <option value="">None</option>
            {slot.detector.options.map((o) => (
              <option key={o.id} value={o.id}>
                {o.name}
              </option>
            ))}
          </select>
          <span className="mx-note">{slot.detector.sourceId ? "Feeds this effect's detector only; adds nothing audible." : "Separate detector signal; adds nothing audible."}</span>
        </label>
      )}
    </li>
  );
}

interface SendRowProps {
  nodeId: string;
  send: MixerSend;
  name: string;
  pending: boolean;
  actions: StripActions;
}

function SendRow({ nodeId, send, name, pending, actions }: SendRowProps) {
  const sendId = send.id;
  const { gestures, command } = actions;
  const ctl = useMemo<ParamControl>(() => {
    const target = { kind: "send", nodeId, sendId } as const;
    return { begin: () => gestures.begin(target), set: (v) => gestures.set(target, v) };
  }, [gestures, nodeId, sendId]);
  const amount = gestures.shown({ kind: "send", nodeId, sendId }) ?? send.amount;
  return (
    <div className="mx-send" role="group" aria-label={`Send to ${name}`}>
      <div className="mx-send-top">
        <strong title={name}>{name}</strong>
        <fieldset className="mx-bare" disabled={pending}>
          <Segmented
            name={`Send to ${name}: before or after the fader`}
            options={["Pre", "Post"]}
            min={0}
            value={send.pre ? 0 : 1}
            does="Takes the send before the track fader (Pre) or after it (Post)."
            onChange={(v) => command({ type: "setSendPre", nodeId, sendId, pre: v === 0 })}
          />
        </fieldset>
        <button type="button" className="mx-icon-btn" aria-label={`Remove send to ${name}`} disabled={pending} onClick={() => command({ type: "removeSend", nodeId, sendId })}>
          <Icon name="remove" />
        </button>
      </div>
      <div className="mx-send-amount" aria-disabled={pending || undefined}>
        <Knob
          control={ctl}
          id={`send-${sendId}`}
          param={SEND}
          value={amount}
          label="Amount"
          name={`Send to ${name} amount`}
          format={formatPercent}
          does="Sets how much of the track goes to this destination."
          row
          size={26}
        />
        <span className="mx-mini">{send.pre ? "Before" : "After"} the track fader</span>
      </div>
    </div>
  );
}
