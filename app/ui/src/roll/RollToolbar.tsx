import { Knob } from "../devices/controls/Knob";
import { AMOUNT_RANGES, type TransformAmount } from "./transformAmounts";
import type { useTransformAmounts } from "./useTransformAmounts";
import type { ProjectKey } from "../api/project";
import { CHORDS, CHORD_LABELS } from "./chords";
import { NOTE_NAMES, SCALES, scaleLabel } from "./scales";
import { GRIDS } from "./snap";
import { TOOL_BUTTONS, type Tool } from "./tools";

const INVERSIONS = ["Root position", "1st inversion", "2nd inversion", "3rd inversion"];

interface Props {
  amounts: ReturnType<typeof useTransformAmounts>;
  tool: Tool;
  onTool: (tool: Tool) => void;
  gridIndex: number;
  onGrid: (index: number) => void;
  /** The project's key; `null` for none. */
  projectKey: ProjectKey | null;
  onKey: (key: ProjectKey | null) => void;
  snapScale: boolean;
  onSnapScale: (on: boolean) => void;
  slide: boolean;
  onSlide: (on: boolean) => void;
  ghosts: boolean;
  onGhosts: (on: boolean) => void;
  lane: boolean;
  onLane: (on: boolean) => void;
  /** The chord a click stamps, or `""` for single notes. */
  chord: string;
  onChord: (chord: string) => void;
  inversion: number;
  onInversion: (inversion: number) => void;
  onQuantize: () => void;
  onStrum: (direction: "up" | "down") => void;
  onHumanize: () => void;
}

/** The piano roll's controls: tools, snap grid, key and scale, slide, ghosts, velocity lane,
 * chord stamps and the whole-selection operations. */
export function RollToolbar(p: Props) {
  const toggle = (label: string, hint: string, on: boolean, set: (on: boolean) => void, disabled = false) => (
    <button type="button" className="roll-tool" aria-pressed={on} disabled={disabled} data-hint={hint} onClick={() => set(!on)}>
      {label}
    </button>
  );
  const act = (label: string, hint: string, run: () => void) => (
    <button type="button" className="roll-tool" data-hint={hint} onClick={run}>
      {label}
    </button>
  );
  return (
    <div className="roll-tools" role="toolbar" aria-label="Piano roll tools">
      <div className="roll-group">
        {TOOL_BUTTONS.map((t) => (
          <button key={t.tool} type="button" className="roll-tool" aria-pressed={p.tool === t.tool} data-hint={t.hint} onClick={() => p.onTool(t.tool)}>
            {t.label}
          </button>
        ))}
      </div>
      <div className="roll-group">
        <label className="roll-snap" data-hint="The grid notes snap to when they are drawn, moved, resized or sliced.">
          Snap
          <select value={p.gridIndex} onChange={(e) => p.onGrid(Number(e.target.value))}>
            {GRIDS.map((g, i) => (
              <option key={g.name} value={i}>
                {g.name}
              </option>
            ))}
          </select>
        </label>
      </div>
      <div className="roll-group">
        <label className="roll-snap" data-hint="The project's key: rows outside its scale are dimmed.">
          Key
          <select
            aria-label="Key"
            value={p.projectKey ? p.projectKey.root : ""}
            onChange={(e) => p.onKey(e.target.value === "" ? null : { root: Number(e.target.value), scale: p.projectKey?.scale ?? "major" })}
          >
            <option value="">None</option>
            {NOTE_NAMES.map((n, i) => (
              <option key={n} value={i}>
                {n}
              </option>
            ))}
          </select>
        </label>
        <label className="roll-snap" data-hint="The scale of the project's key.">
          <span className="visually-hidden">Scale</span>
          <select
            aria-label="Scale"
            disabled={!p.projectKey}
            value={p.projectKey?.scale ?? "major"}
            onChange={(e) => p.projectKey && p.onKey({ root: p.projectKey.root, scale: e.target.value })}
          >
            {Object.keys(SCALES).map((id) => (
              <option key={id} value={id}>
                {scaleLabel(id)}
              </option>
            ))}
          </select>
        </label>
        {toggle("Snap to scale", "Drawn, painted and moved notes jump to the nearest key of the scale.", p.snapScale, p.onSnapScale, !p.projectKey)}
      </div>
      <div className="roll-group">
        {toggle("Slide", "New notes glide into the next note (an 808 slide); this also marks the selected notes.", p.slide, p.onSlide)}
        {toggle("Ghost notes", "Show the other channels' notes faintly behind this channel's. They cannot be edited here.", p.ghosts, p.onGhosts)}
        {toggle("Velocity", "Show the velocity lane: drag to set the velocity of the selected notes.", p.lane, p.onLane)}
      </div>
      <div className="roll-group">
        <label className="roll-snap" data-hint="Stamp a chord instead of a single note when you draw.">
          <span className="visually-hidden">Chord</span>
          <select aria-label="Chord" value={p.chord} onChange={(e) => p.onChord(e.target.value)}>
            <option value="">No chord</option>
            {Object.keys(CHORDS).map((id) => (
              <option key={id} value={id}>
                {CHORD_LABELS[id] ?? id}
              </option>
            ))}
          </select>
        </label>
        <label className="roll-snap" data-hint="Which inversion of the chord is stamped.">
          <span className="visually-hidden">Inversion</span>
          <select aria-label="Inversion" disabled={!p.chord} value={p.inversion} onChange={(e) => p.onInversion(Number(e.target.value))}>
            {INVERSIONS.map((n, i) => (
              <option key={n} value={i}>
                {n}
              </option>
            ))}
          </select>
        </label>
      </div>
      <div className="roll-group">
        {act("Quantize", "Move the starts of the selected notes (all notes with none selected) onto the snap grid (Alt+Q).", p.onQuantize)}
        <details className="roll-feel">
          <summary className="roll-tool">Feel</summary>
          <div className="roll-feel-panel" role="group" aria-label="Transform amounts">
            {([ ["spread", "Strum spread"], ["timing", "Humanize timing"], ["velocity", "Humanize velocity"] ] as [TransformAmount, string][]).map(([id, label]) => (
              <Knob key={id} id={id} param={AMOUNT_RANGES[id]} value={p.amounts.values[id]}
                control={p.amounts.control(id)} label={label} name={label} size={36}
                format={v => v === 0 ? "Off" : `${Math.round(v / AMOUNT_RANGES[id].max * 100)}%`}
                does="Adjust the feel of the next operation from low to high." />
            ))}
          </div>
        </details>
        {act("Strum up", "Spread the notes of each chord in time, lowest note first.", () => p.onStrum("up"))}
        {act("Strum down", "Spread the notes of each chord in time, highest note first.", () => p.onStrum("down"))}
        {act("Humanize", "Nudge the timing and velocity of the selected notes (all notes with none selected) a little, at random.", p.onHumanize)}
      </div>
    </div>
  );
}
