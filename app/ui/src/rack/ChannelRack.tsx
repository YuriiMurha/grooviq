import { useCallback, useState, type CSSProperties, type KeyboardEvent, type RefObject } from "react";
import { channelAdd, channelSetSample, errorText, type ChannelView } from "../api/project";
import type { LiveFrame } from "../live/decode";
import { useProject } from "../project/context";
import { pickSound, pickSounds } from "./audioFiles";
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
  createRenderer?: (canvas: HTMLCanvasElement) => GridRenderer;
}

/** The channel rack: a row per channel beside one step grid, then adding channels (button or
 * files dropped on the window), the pattern length and swing. */
export function ChannelRack({ latest, sampleRate, onError = () => {}, createRenderer }: Props) {
  const { view, apply, run } = useProject();
  const [dropping, setDropping] = useState(false);

  // Each file becomes a channel, in order; one that cannot load shows the loader's sentence.
  const addFiles = useCallback(
    async (paths: string[]) => {
      for (const path of paths) await run(() => channelAdd(path));
    },
    [run],
  );
  useFileDrop(addFiles, setDropping);

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

  const relink = (ch: ChannelView) => {
    pickSound(`Find the sample for ${ch.name}`).then(
      (path) => {
        if (path !== null) void run(() => channelSetSample(ch.id, path));
      },
      (e: unknown) => onError(errorText(e)),
    );
  };

  const addChannel = () => {
    pickSounds().then(addFiles, (e: unknown) => onError(errorText(e)));
  };

  return (
    <section
      className={`rack${dropping ? " drop-over" : ""}`}
      aria-label="Channel rack"
      style={{ "--cell": `${CELL}px`, "--gap": `${GAP}px` } as CSSProperties}
    >
      <header className="rack-head">
        <h2>Channel rack</h2>
        <span className="chip" data-hint="The pattern the rack shows and edits.">
          Pattern <b>{pattern.name}</b>
        </span>
        <PatternLength steps={pattern.steps} onCommit={(steps) => void apply({ kind: "setPatternLength", steps })} />
      </header>
      {channels.length > 0 && (
        <div className="rack-body">
          <div className="rack-rows">
            {channels.map((ch) => (
              <ChannelRow
                key={ch.id}
                channel={ch}
                onMix={(change) => mix(ch, change)}
                onRelink={() => relink(ch)}
                onError={onError}
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
              createRenderer={createRenderer}
            />
          </div>
        </div>
      )}
      <footer className="rack-foot">
        <button
          type="button"
          className="add-channel"
          data-hint="Adds a channel for each sound file you choose: WAV, AIFF, FLAC, MP3 or Ogg."
          onClick={addChannel}
        >
          + Add channel
        </button>
        <span className="rack-drop">{EMPTY_RACK}</span>
        <Swing swing={pattern.swing} onCommit={(swing) => apply({ kind: "setSwing", swing })} />
      </footer>
    </section>
  );
}

/** The pattern's length in steps: type it, Enter or leaving the field sets it (clamped). */
function PatternLength({ steps, onCommit }: { steps: number; onCommit: (steps: number) => void }) {
  const [draft, setDraft] = useState<string | null>(null);
  const commit = (text: string) => {
    setDraft(null);
    const n = Math.round(Number(text.trim()));
    if (text.trim() === "" || !Number.isFinite(n)) return;
    const next = Math.min(STEPS_MAX, Math.max(STEPS_MIN, n));
    if (next !== steps) onCommit(next);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") commit(e.currentTarget.value);
    else if (e.key === "Escape") setDraft(null);
  };
  return (
    <label className="chip pattern-length" data-hint="How many steps the pattern has, 1 to 256. A step is a sixteenth.">
      <input
        type="number"
        min={STEPS_MIN}
        max={STEPS_MAX}
        aria-label="Pattern length"
        value={draft ?? String(steps)}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={onKeyDown}
        onBlur={(e) => {
          if (draft !== null) commit(e.currentTarget.value);
        }}
      />
      <span>steps</span>
    </label>
  );
}

/** Swing, 0..100 %: moved locally while dragging, sent on release. */
function Swing({ swing, onCommit }: { swing: number; onCommit: (swing: number) => Promise<unknown> }) {
  const percent = Math.round(swing * 100);
  // The value being moved, and the swing it started from: a new swing from the engine wins.
  const [moving, setMoving] = useState<{ value: number; from: number } | null>(null);
  const shown = moving && moving.from === percent ? moving.value : percent;
  const commit = () => {
    if (moving && moving.from === percent && moving.value !== percent) {
      // Shown until the edit settles, so the slider does not jump back for a round trip.
      void onCommit(moving.value / 100).finally(() => setMoving(null));
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
        onChange={(e) => setMoving({ value: Number(e.target.value), from: percent })}
        onPointerUp={commit}
        onKeyUp={commit}
        onBlur={commit}
      />
      <span className="swing-value">{shown}%</span>
    </label>
  );
}
