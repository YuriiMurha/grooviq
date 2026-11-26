import { useState, type ReactNode, type Ref, type RefObject } from "react";
import type { MixerInsertDetail } from "../api/mixer";
import { ListPicker } from "../layout/ListPicker";
import type { LiveFrame } from "../live/decode";
import { Mixer } from "./Mixer";
import type { MeterSource } from "./mixerModel";
import { useMixerHost } from "./useMixerHost";
import "./mixerPanel.css";

export type MixerPanelInsertEditor = Readonly<{
  detail: MixerInsertDetail;
  pending: boolean;
  setParams: (params: readonly number[]) => Promise<boolean>;
  applyPreset: (preset: string) => Promise<boolean>;
  close: () => void;
}>;

export type MixerPanelProps = Readonly<{
  /** Programmatic focus entry for the Workspace. */
  focusRef?: Ref<HTMLElement>;
  latestLive?: RefObject<LiveFrame | null>;
  meters?: MeterSource;
  clearClip?: (nodeId: string) => void;
  renderInsertEditor?: (editor: MixerPanelInsertEditor) => ReactNode;
}>;

type SourceRouting = NonNullable<ReturnType<typeof useMixerHost>["routing"]>;

function SourceAssignments({ routing, pending }: { routing: SourceRouting; pending: boolean }) {
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const source = routing.sources.find(({ id }) => id === selectedId) ?? routing.sources[0];
  const masterOnly = routing.destinations.length === 1;

  return (
    <details className="mx-panel-sources">
      <summary>Source assignments</summary>
      {source ? (
        <>
          <div className="mx-panel-source-fields">
            <div>
              <span>Arrangement source</span>
              <ListPicker
                label="Arrangement source"
                options={routing.sources}
                value={source.id}
                placeholder="Choose a source"
                onCommit={setSelectedId}
              />
            </div>
            <div>
              <span>Mixer destination</span>
              <ListPicker
                label="Mixer destination"
                options={routing.destinations}
                value={source.destinationId}
                placeholder="Choose a destination"
                disabled={pending || masterOnly}
                reason={pending ? "A mixer edit is pending." : "Add a track to choose another destination."}
                onCommit={(id) => {
                  if (id !== source.destinationId) void routing.assign(source.id, id);
                }}
              />
            </div>
          </div>
          <p className="mx-mini">Patterns share their instrument channel’s destination. Audio tracks route independently.</p>
        </>
      ) : (
        <p className="mx-mini">Add an instrument channel or an audio track to assign a destination.</p>
      )}
    </details>
  );
}

/** Production mixer surface. Mount under ProjectContext. */
export function MixerPanel({ focusRef, latestLive, meters, clearClip, renderInsertEditor }: MixerPanelProps) {
  const { props, routing, error, pending, insert, refresh } = useMixerHost({ latestLive, meters, clearClip });
  const editor = insert?.detail && renderInsertEditor?.({
    detail: insert.detail,
    pending: insert.pending,
    setParams: insert.setParams,
    applyPreset: insert.applyPreset,
    close: insert.close,
  });

  return (
    <section ref={focusRef} tabIndex={-1} aria-label="Mixer" className="mx-panel">
      {props && routing ? (
        <>
          <div className="mx-panel-actions" aria-busy={pending || undefined}>
            {error?.nodeId === null && <button type="button" className="btn mx-panel-button" onClick={() => void refresh()}>Retry refresh</button>}
            <button type="button" className="btn mx-panel-button" disabled={pending} onClick={() => void routing.addTrack()}>Add track</button>
          </div>
          <Mixer {...props} />
          <SourceAssignments routing={routing} pending={pending} />
          {insert && (
            <section className="mx-panel-insert" aria-label="Insert editor">
              <div className="mx-panel-actions">
                <button type="button" className="btn mx-panel-button" aria-label="Close insert editor" onClick={insert.close}>Close</button>
              </div>
              {insert.loading ? <p className="mx-mini" role="status">Loading insert…</p>
                : editor ?? <p className="mx-mini">Insert editor unavailable.</p>}
            </section>
          )}
        </>
      ) : (
        <div className="mx-panel-status">
          {error ? (
            <>
              <p className="mx-notice" role="status">{error.message}</p>
              <button type="button" className="btn mx-panel-button" onClick={() => void refresh()}>Retry</button>
            </>
          ) : <p className="mx-mini" role="status">Loading mixer…</p>}
        </div>
      )}
    </section>
  );
}
