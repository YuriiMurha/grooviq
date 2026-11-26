import { ListPicker } from "../layout/ListPicker";
import { Knob } from "../devices/controls/Knob";
import { useFeelPopover } from "./useFeelPopover";
import "./feelPopover.css";
import { useEffect, useRef } from "react";
import { AMOUNT_RANGES, type TransformAmount } from "./transformAmounts";
import type { useTransformAmounts } from "./useTransformAmounts";
import type { ProjectKey } from "../api/project";
import { CHORDS, CHORD_LABELS } from "./chords";
import { NOTE_NAMES, SCALES, scaleLabel } from "./scales";
import { GRIDS } from "./snap";
import { TOOL_BUTTONS, type Tool } from "./tools";

const GRID_OPTIONS = GRIDS.map(({ name }, index) => ({ id: String(index), name }));
const KEY_OPTIONS = [{ id: "", name: "None" }, ...NOTE_NAMES.map((name, root) => ({ id: String(root), name }))];
const SCALE_OPTIONS = Object.keys(SCALES).map((id) => ({ id, name: scaleLabel(id) }));
const CHORD_OPTIONS = [{ id: "", name: "No chord" }, ...Object.keys(CHORDS).map((id) => ({ id, name: CHORD_LABELS[id] ?? id }))];
const INVERSION_OPTIONS = ["Root position", "1st inversion", "2nd inversion", "3rd inversion"]
  .map((name, index) => ({ id: String(index), name }));

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
  const { projectKey } = p;
  const feel = useRef<HTMLDetailsElement>(null);
  const cancelAmounts = p.amounts.cancel;
  const feelPanel = useRef<HTMLDivElement>(null);
  const { synchronize } = useFeelPopover({ detailsRef: feel, panelRef: feelPanel, cancelAmounts });
  useEffect(() => {
    const close = (returnFocus: boolean) => {
      if (!feel.current?.open) return;
      cancelAmounts();
      feel.current.open = false;
      if (returnFocus) feel.current.querySelector("summary")?.focus();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape" && feel.current?.open) { e.preventDefault(); close(true); }
    };
    const pointer = (e: PointerEvent) => {
      if (e.target instanceof Node && !feel.current?.contains(e.target)) close(false);
    };
    window.addEventListener("keydown", key);
    window.addEventListener("pointerdown", pointer, true);
    return () => {
      window.removeEventListener("keydown", key);
      window.removeEventListener("pointerdown", pointer, true);
    };
  }, [cancelAmounts]);
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
        <div className="roll-snap" data-hint="The grid notes snap to when they are drawn, moved, resized or sliced.">
          <span>Snap</span>
          <ListPicker label="Snap" options={GRID_OPTIONS} value={String(p.gridIndex)} placeholder="" onCommit={(id) => p.onGrid(Number(id))} />
        </div>
      </div>
      <div className="roll-group">
        <div className="roll-snap" data-hint="The project's key: rows outside its scale are dimmed.">
          <span>Key</span>
          <ListPicker label="Key" options={KEY_OPTIONS} value={projectKey ? String(projectKey.root) : ""} placeholder="None"
            onCommit={(id) => p.onKey(id === "" ? null : { root: Number(id), scale: projectKey?.scale ?? "major" })} />
        </div>
        <div className="roll-snap" data-hint="The scale of the project's key.">
          <ListPicker label="Scale" options={SCALE_OPTIONS} value={projectKey?.scale ?? "major"} placeholder="Major"
            disabled={!projectKey} reason="Choose a key first."
            onCommit={(scale) => { if (projectKey) p.onKey({ root: projectKey.root, scale }); }} />
        </div>
        {toggle("Snap to scale", "Drawn, painted and moved notes jump to the nearest key of the scale.", p.snapScale, p.onSnapScale, !p.projectKey)}
      </div>
      <div className="roll-group">
        {toggle("Slide", "New notes glide into the next note (an 808 slide); this also marks the selected notes.", p.slide, p.onSlide)}
        {toggle("Ghost notes", "Show the other channels' notes faintly behind this channel's. They cannot be edited here.", p.ghosts, p.onGhosts)}
        {toggle("Velocity", "Show the velocity lane: drag to set the velocity of the selected notes.", p.lane, p.onLane)}
      </div>
      <div className="roll-group">
        <div className="roll-snap" data-hint="Stamp a chord instead of a single note when you draw.">
          <ListPicker label="Chord" options={CHORD_OPTIONS} value={p.chord} placeholder="No chord" onCommit={p.onChord} />
        </div>
        <div className="roll-snap" data-hint="Which inversion of the chord is stamped.">
          <ListPicker label="Inversion" options={INVERSION_OPTIONS} value={String(p.inversion)} placeholder="Root position"
            disabled={!p.chord} reason="Choose a chord first." onCommit={(id) => p.onInversion(Number(id))} />
        </div>
      </div>
      <div className="roll-group">
        {act("Quantize", "Move the starts of the selected notes (all notes with none selected) onto the snap grid (Alt+Q).", p.onQuantize)}
        <details ref={feel} className="roll-feel" onToggle={() => { if (!feel.current?.open) cancelAmounts(); synchronize(); }}>
          <summary className="roll-tool">Feel</summary>
          <div ref={feelPanel} className="roll-feel-panel" role="group" aria-label="Transform amounts">
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
