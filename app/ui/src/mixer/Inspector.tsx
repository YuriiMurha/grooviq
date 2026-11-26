import { ListPicker } from "../layout/ListPicker";
import { Icon } from "./icons";
import { OutputChoice } from "./OutputChoice";
import { UnsupportedButton } from "./UnsupportedButton";
import { moveTarget, unusedOptionId, unsupportedReason, type MixerError, type MixerInsert, type MixerOption, type MixerProps, type MixerTrack } from "./mixerModel";
import type { StripActions } from "./TrackStrip";

interface InspectorProps {
  track: MixerTrack | null;
  master: boolean;
  masterOutput: string;
  removed: boolean;
  effects: readonly MixerOption[];
  error: MixerError | null;
  actions: StripActions;
  nodes: ReadonlyMap<string, MixerTrack>;
  unsupported: MixerProps["unsupported"];
}

/** Main output and insert slots for the host's selected strip. */
export function Inspector({ track, master, masterOutput, removed, effects, error, actions, nodes, unsupported }: InspectorProps) {
  const errorTrack = error?.nodeId != null && error.nodeId !== track?.id ? nodes.get(error.nodeId) : undefined;
  return (
    <aside className="mx-inspector" aria-label="Selected mixer track" aria-busy={track?.pending || undefined}>
      {track ? (
        <>
          <header className="mx-ihead">
            <h2>{track.name}</h2>
            <span className="mx-kind">{track.kind}</span>
          </header>
          <div className="mx-rack-fields">
            <div className="mx-field">
              <span>Main output</span>
              {master ? <select aria-label="Main output" disabled><option>{masterOutput}</option></select> : <OutputChoice track={track} nodes={nodes} command={actions.command} />}
            </div>
            <div className="mx-field">
              <span>Insert chain</span>
              <ListPicker
                label="Insert chain"
                options={effects}
                value={null}
                placeholder={effects.length ? "Add an effect…" : "No effects available"}
                disabled={track.pending || effects.length === 0}
                onCommit={(effectId) => actions.command({ type: "addInsert", nodeId: track.id, effectId })}
              />
            </div>
          </div>
          <div className="mx-rack-slots">
            <h3>Slots</h3>
            {track.inserts.length === 0 ? <p className="mx-mini">No effects</p> : (
              <ol className="mx-inserts">
                {track.inserts.map((slot, i) => (
                  <InsertRow key={slot.id} track={track} slot={slot} index={i} unsupported={unsupported} command={actions.command} />
                ))}
              </ol>
            )}
          </div>
        </>
      ) : (
        <p className="mx-empty">{removed ? "The selected track is no longer in the mixer. Select another strip." : "Select a strip to see its insert rack."}</p>
      )}
      <p className="mx-notice" role="status">
        {errorTrack && `${errorTrack.name}: `}{error?.message}
      </p>
    </aside>
  );
}

interface InsertRowProps {
  track: MixerTrack;
  slot: MixerInsert;
  index: number;
  unsupported: MixerProps["unsupported"];
  command: StripActions["command"];
}

function InsertRow({ track, slot, index, unsupported, command }: InsertRowProps) {
  const { id: nodeId, inserts, pending } = track;
  const { id: slotId, detector } = slot;
  const bypassReason = unsupportedReason(unsupported, "setInsertActive");
  const detectorReason = unsupportedReason(unsupported, "setDetector");
  const noSource = unusedOptionId(detector?.options ?? []);
  const move = (dir: -1 | 1) => {
    const to = moveTarget(inserts, slotId, dir);
    if (to) command({ type: "moveInsert", nodeId, slotId, ...to });
  };
  return (
    <li className="mx-insert" aria-label={slot.name}>
      <div className="mx-insert-row">
        <span className="mx-slot-index" aria-hidden="true">{index + 1}</span>
        <button type="button" className="mx-open" title={slot.name} aria-label={`Open ${slot.name}`}
          onClick={() => command({ type: "openInsert", nodeId, slotId })}>{slot.name}</button>
        <UnsupportedButton type="button" className="mx-tool mx-bypass" role="switch" aria-label="On" aria-checked={slot.active}
          disabled={pending} title={`Enable or bypass ${slot.name}`} reason={bypassReason}
          onClick={() => command({ type: "setInsertActive", nodeId, slotId, active: !slot.active })}>
          {slot.active ? "On" : "Off"}
        </UnsupportedButton>
        <button type="button" className="mx-icon-btn" aria-label={`Move ${slot.name} earlier`} title="Move earlier" disabled={pending || index === 0} onClick={() => move(-1)}>
          <Icon name="up" />
        </button>
        <button type="button" className="mx-icon-btn" aria-label={`Move ${slot.name} later`} title="Move later" disabled={pending || index === inserts.length - 1} onClick={() => move(1)}>
          <Icon name="down" />
        </button>
        <button type="button" className="mx-icon-btn" aria-label={`Remove ${slot.name}`} title="Remove insert" disabled={pending} onClick={() => command({ type: "removeInsert", nodeId, slotId })}>
          <Icon name="remove" />
        </button>
      </div>
      {detector && (
        <div className="mx-field mx-detector">
          <span>Sidechain input</span>
          <ListPicker
            label="Sidechain input"
            options={[{ id: noSource, name: "None" }, ...detector.options]}
            value={detector.options.some(({ id }) => id === detector.sourceId) ? detector.sourceId : noSource}
            placeholder="None"
            disabled={pending || detectorReason !== undefined}
            reason={detectorReason}
            onCommit={(id) => command({ type: "setDetector", nodeId, slotId, sourceId: id === noSource ? null : id })}
          />
          {detectorReason && <span className="mx-mini">{detectorReason}</span>}
        </div>
      )}
    </li>
  );
}
