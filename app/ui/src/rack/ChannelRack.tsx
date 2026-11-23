import type { PatternStamp } from "../api/patterns";
import { useCallback, useEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type RefObject } from "react";
import { channelAddSynth, onNotesChanged, patternNotes, type WireNote } from "../api/notes";
import { channelAdd, errorText, projectView, type ChannelView } from "../api/project";
import { useSelection } from "../layout/selection";
import type { LiveFrame } from "../live/decode";
import { ConfirmDialog } from "../project/ConfirmDialog";
import { useProject, type ProjectStamp } from "../project/context";
import { AddChannelMenu } from "./AddChannelMenu";
import { pickSounds } from "./audioFiles";
import { ChannelRow } from "./ChannelRow";
import { useFileDrop } from "./fileDrop";
import { CELL, EDGE, GAP } from "./gridModel";
import type { GridRenderer } from "./palette";
import { StepGrid } from "./StepGrid";

/** Pattern length range: one place in the engine, project and here. */
export const STEPS_MIN = 1;
export const STEPS_MAX = 256;

export const EMPTY_RACK = "Drop a sound here to make a channel";

interface Props {
  latest: RefObject<LiveFrame | null>;
  /** The output's rate, for the latency shift of the playhead. */
  sampleRate: number;
  /** Shows a failure that is not a project edit (a preview, a file dialog). */
  onError?: (message: string) => void;
  /** Opens the piano roll on a channel (a click on a roll channel's preview). */
  onOpenRoll?: (channel: number) => void;
  createRenderer?: (canvas: HTMLCanvasElement) => GridRenderer;
}

/** The pattern's notes, fetched again whenever the view's notes revision moves. The engine
 * announces notes changed elsewhere (recording) with `notes-changed`: the view is fetched then,
 * since roll channels may have changed too. */
function usePatternNotes(revision: number | undefined, onError: (message: string) => void): WireNote[] {
  const { run, view } = useProject();
  const patternId = view?.pattern.id;
  const generation = view?.generation;
  const [notes, setNotes] = useState<WireNote[]>([]);
  const report = useRef(onError);
  const shown = useRef(revision);
  useEffect(() => {
    report.current = onError;
    shown.current = revision;
  }, [onError, revision]);

  useEffect(() => {
    if (revision === undefined) return;
    let gone = false;
    patternNotes().then(
      (n) => {
        if (!gone) setNotes(n.notes);
      },
      (e: unknown) => report.current(errorText(e)),
    );
    return () => {
      gone = true;
    };
  }, [revision, patternId, generation]);

  useEffect(() => {
    // Outside the Tauri shell there is nothing to listen to. A revision the view already has
    // (an edit this UI made) needs no fetch.
    const listening = onNotesChanged((r) => {
      if (r !== shown.current) void run(projectView);
    }).catch(() => null);
    return () => {
      void listening.then((stop) => stop?.());
    };
  }, [run]);
  return notes;
}

/** The channel rack: a row per channel beside one step grid, then adding channels (button or
 * files dropped on the window), the pattern length and swing. */
export function ChannelRack({ latest, sampleRate, onError = () => {}, onOpenRoll = () => {}, createRenderer }: Props) {
  const project = useProject();
  const { view, apply, runFor, stamp } = project;
  const { channel: selected, select } = useSelection();
  const [dropping, setDropping] = useState(false);
  const [removing, setRemoving] = useState<ChannelView | null>(null);
  const rowsRef = useRef<HTMLDivElement>(null);
  const notes = usePatternNotes(view?.notesRevision, onError);

  // Each file becomes a channel, in order, in the project shown when the files were dropped or
  // picked; one that cannot load shows the loader's sentence. Each file is checked at its turn:
  // after a New, Open or Restore (or while one is on its way) the rest are dropped.
  const addFiles = useCallback(
    async (paths: string[], at: ProjectStamp) => {
      for (const path of paths) await runFor(at, (g) => channelAdd(path, g));
    },
    [runFor],
  );
  const dropFiles = useCallback((paths: string[]) => void addFiles(paths, stamp()), [addFiles, stamp]);
  useFileDrop(dropFiles, setDropping);

  if (!view) return null;
  const { channels, pattern } = view;

  const mix = (ch: ChannelView, change: Partial<Pick<ChannelView, "volume" | "pan" | "mute" | "solo">>) =>
    apply({
      kind: "setChannelMix",
      channel: ch.id,
      volume: change.volume ?? ch.volume,
      pan: change.pan ?? ch.pan,
      mute: change.mute ?? ch.mute,
      solo: change.solo ?? ch.solo,
    });

  const addSamples = () => {
    const at = stamp();
    pickSounds().then((paths) => addFiles(paths, at), (e: unknown) => onError(errorText(e)));
  };
  // The new synth becomes the selected channel: the one the typing keyboard plays.
  const addSynth = (preset: string) => {
    const before = new Set(channels.map((c) => c.id));
    let made: number | null = null;
    void runFor(stamp(), async (g) => {
      const next = await channelAddSynth(preset, g);
      made = next.channels.find((c) => !before.has(c.id))?.id ?? null;
      return next;
    }).then((added) => {
      if (added && made !== null) select(made);
    });
  };

  const remove = async (ch: ChannelView) => {
    setRemoving(null);
    const index = channels.findIndex((c) => c.id === ch.id);
    if (!(await apply({ kind: "removeChannel", channel: ch.id }))) return;
    // Keep the keyboard in the rack: the row that took its place, else the one above.
    const names = rowsRef.current?.querySelectorAll<HTMLElement>(".channel-name");
    if (names && names.length > 0) names[Math.min(index, names.length - 1)].focus();
  };

  return (
    <section
      className={`rack${dropping ? " drop-over" : ""}`}
      aria-label="Channel rack"
      style={{ "--cell": `${CELL}px`, "--gap": `${GAP}px` } as CSSProperties}
    >
      <header className="rack-head">
        <h2>Channel rack</h2>
        <span className="meta-chip" data-hint="The pattern the rack shows and edits.">
          Pattern <b>{pattern.name}</b>
        </span>
        <PatternLength key={`${view.generation}:${pattern.id}`} steps={pattern.steps} onCommit={(steps, origin) => void apply({ kind: "setPatternLength", steps }, undefined, origin)} />
      </header>
      {channels.length > 0 && (
        <div className="rack-body">
          <div className="rack-rows" ref={rowsRef}>
            {channels.map((ch) => (
              <ChannelRow
                key={ch.id}
                channel={ch}
                onMix={(change) => mix(ch, change)}
                onError={onError}
                selected={ch.id === selected}
                onSelect={() => select(ch.id)}
                onRemove={() => setRemoving(ch)}
              />
            ))}
          </div>
          <div className="rack-grid" style={{ margin: -EDGE }}>
            <StepGrid
              channels={channels}
              pattern={pattern}
              bpm={view.bpm}
              latest={latest}
              sampleRate={sampleRate}
              notes={notes}
              onOpenRoll={onOpenRoll}
              createRenderer={createRenderer}
            />
          </div>
        </div>
      )}
      <footer className="rack-foot">
        <AddChannelMenu onSample={addSamples} onSynth={addSynth} />
        <span className="rack-drop">{EMPTY_RACK}</span>
        <Swing key={`${view.generation}:${pattern.id}`} swing={pattern.swing} onCommit={(swing, origin) => apply({ kind: "setSwing", swing }, undefined, origin)} />
      </footer>
      {removing && (
        <ConfirmDialog
          title={`Remove ${removing.name}?`}
          message={`${removing.name} goes, with its steps and notes.`}
          onDismiss={() => setRemoving(null)}
          actions={[
            { label: "Cancel", hint: "Keeps the channel.", onSelect: () => setRemoving(null) },
            {
              label: "Remove",
              hint: `Removes ${removing.name} from the project.`,
              primary: true,
              onSelect: () => void remove(removing),
            },
          ]}
        />
      )}
    </section>
  );
}

/** The pattern's length in steps: type it, Enter or leaving the field sets it (clamped), and
 * Enter leaves the field so Space is play/stop again. */
function PatternLength({ steps, onCommit }: { steps: number; onCommit: (steps: number, origin: PatternStamp) => void }) {
  const { view } = useProject();
  const origin = useRef<PatternStamp | null>(null);
  const capture = (): PatternStamp => ({ generation: view?.generation ?? 0, patternId: view?.pattern.id ?? 0, revision: view?.notesRevision ?? 1 });
  const [draft, setDraft] = useState<string | null>(null);
  // Set when Enter already committed: the blur that follows must not commit again.
  const committed = useRef(false);
  const commit = (text: string) => {
    setDraft(null);
    const n = Math.round(Number(text.trim()));
    if (text.trim() === "" || !Number.isFinite(n)) return;
    const next = Math.min(STEPS_MAX, Math.max(STEPS_MIN, n));
    if (next !== steps && origin.current) onCommit(next, origin.current);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      commit(e.currentTarget.value);
      committed.current = true;
      e.currentTarget.blur();
    } else if (e.key === "Escape") setDraft(null);
  };
  return (
    <label className="meta-chip pattern-length" data-hint="How many steps the pattern has, 1 to 256. A step is a sixteenth.">
      <input
        type="number"
        min={STEPS_MIN}
        max={STEPS_MAX}
        aria-label="Pattern length"
        value={draft ?? String(steps)}
        onFocus={() => { origin.current = capture(); }}
        onChange={(e) => { origin.current ??= capture(); setDraft(e.target.value); }}
        onKeyDown={onKeyDown}
        onBlur={(e) => {
          if (committed.current) committed.current = false;
          else if (draft !== null) commit(e.currentTarget.value);
        }}
      />
      <span>steps</span>
    </label>
  );
}

/** Swing, 0..100 %: moved locally while dragging, sent on release. */
function Swing({ swing, onCommit }: { swing: number; onCommit: (swing: number, origin: PatternStamp) => Promise<unknown> }) {
  const { view } = useProject();
  const origin = useRef<PatternStamp | null>(null);
  const capture = (): PatternStamp => ({ generation: view?.generation ?? 0, patternId: view?.pattern.id ?? 0, revision: view?.notesRevision ?? 1 });
  const percent = Math.round(swing * 100);
  // The value being moved, and the swing it started from: a new swing from the engine wins.
  const [moving, setMoving] = useState<{ value: number; from: number } | null>(null);
  const shown = moving && moving.from === percent ? moving.value : percent;
  const commit = () => {
    if (moving && moving.from === percent && moving.value !== percent) {
      // Shown until the edit settles, so the slider does not jump back for a round trip.
      const sent = moving;
      if (origin.current) void onCommit(moving.value / 100, origin.current).finally(() => setMoving((current) => current === sent ? null : current));
    } else {
      setMoving(null);
    }
  };
  return (
    <label className="swing" data-hint="Swing pushes every second sixteenth later, for a looser groove.">
      <span>Swing</span>
      <input
        type="range"
        min={0}
        max={100}
        step={1}
        aria-label="Swing"
        value={shown}
        style={{ "--fill": `${shown}%` } as CSSProperties}
        onPointerDown={() => { origin.current = capture(); }}
        onKeyDown={() => { origin.current = capture(); }}
        onChange={(e) => { origin.current ??= capture(); setMoving({ value: Number(e.target.value), from: percent }); }}
        onPointerUp={commit}
        onKeyUp={commit}
        onBlur={commit}
      />
      <span className="swing-value">{shown}%</span>
    </label>
  );
}
