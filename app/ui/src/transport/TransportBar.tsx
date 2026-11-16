import type { RefObject } from "react";
import type { ProjectView } from "../api/project";
import type { LiveFrame } from "../live/decode";
import { BpmField } from "./BpmField";
import { PositionReadout } from "./PositionReadout";

const UNITS = [2, 4, 8, 16];
const BEATS_MAX = 16;

interface Props {
  view: ProjectView;
  playing: boolean;
  latest: RefObject<LiveFrame | null>;
  sampleRate: number;
  onPlay: () => void;
  onStop: () => void;
  onTempo: (bpm: number) => void;
  onSignature: (beats: number, unit: number) => void;
}

/** Play and stop, the tempo with its time signature under it, and the position. */
export function TransportBar({ view, playing, latest, sampleRate, onPlay, onStop, onTempo, onSignature }: Props) {
  const { beats, unit } = view.timeSignature;
  const beatChoices = Array.from({ length: Math.max(BEATS_MAX, beats) }, (_, i) => i + 1);
  const unitChoices = UNITS.includes(unit) ? UNITS : [...UNITS, unit].sort((a, b) => a - b);

  return (
    <div className="transport">
      <div className="transport-buttons">
        <button
          type="button"
          className="transport-btn play"
          aria-label="Play"
          aria-pressed={playing}
          data-hint="Starts playing the pattern. Space also plays and stops."
          onClick={onPlay}
        >
          <svg viewBox="0 0 16 16" aria-hidden>
            <path d="M4 2.5v11l9-5.5z" fill="currentColor" />
          </svg>
        </button>
        <button
          type="button"
          className="transport-btn"
          aria-label="Stop"
          data-hint="Stops playing. Space also plays and stops."
          onClick={onStop}
        >
          <svg viewBox="0 0 16 16" aria-hidden>
            <rect x="3.5" y="3.5" width="9" height="9" rx="1" fill="currentColor" />
          </svg>
        </button>
      </div>
      <div className="tempo">
        <BpmField bpm={view.bpm} onCommit={onTempo} />
        <span className="field-caption tempo-caption">
          BPM
          <select
            aria-label="Beats per bar"
            data-hint="How many beats make a bar."
            value={beats}
            onChange={(e) => onSignature(Number(e.target.value), unit)}
          >
            {beatChoices.map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </select>
          /
          <select
            aria-label="Beat note value"
            data-hint="Which note value is one beat: 4 is a quarter note, 8 an eighth."
            value={unit}
            onChange={(e) => onSignature(beats, Number(e.target.value))}
          >
            {unitChoices.map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </select>
        </span>
      </div>
      <PositionReadout latest={latest} bpm={view.bpm} signature={view.timeSignature} sampleRate={sampleRate} />
    </div>
  );
}
