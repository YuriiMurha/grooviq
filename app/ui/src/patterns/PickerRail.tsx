import { useRef, useState } from "react";
import type { PatternPicker } from "./usePatterns";
import "./rail.css";

/** T10 may consume this MIME payload; this rail never places a clip. */
export const PATTERN_DRAG_TYPE = "application/x-grooviq-pattern";
export function PickerRail({ picker }: { picker: PatternPicker }) {
  const { state, loading, message, act } = picker;
  const [editing, setEditing] = useState<{ id: number; name: string } | null>(null);
  const list = useRef<HTMLUListElement>(null);
  const busy = Boolean(state.pending) || picker.modePending || loading;
  const selected = state.canonical?.selected;
  const focusSelected = () => list.current?.querySelector<HTMLButtonElement>('[aria-current="true"]')?.focus();
  return <aside className="pattern-rail" aria-label="Patterns" aria-busy={busy}>
    <header className="pattern-rail-head"><h2>Patterns</h2>
      <button type="button" disabled={busy || !state.canonical} onClick={() => { void act({ kind: "create", name: "New pattern" }); }}
        data-hint="Creates a new pattern without starting playback.">New</button></header>
    {loading && <p role="status">Loading patterns…</p>}
    <ul ref={list} className="pattern-rail-list" onKeyDown={(event) => {
      if (!(event.target instanceof HTMLButtonElement) || event.altKey || event.metaKey || event.ctrlKey || event.shiftKey) return;
      const buttons = Array.from(list.current?.querySelectorAll<HTMLButtonElement>('button[data-pattern]') ?? []);
      const index = buttons.indexOf(event.target);
      const next = event.key === "ArrowDown" ? Math.min(index + 1, buttons.length - 1) : event.key === "ArrowUp" ? Math.max(0, index - 1) : event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1 : null;
      if (next !== null) { event.preventDefault(); buttons[next]?.focus(); }
    }}>
      {state.canonical?.patterns.map((pattern) => <li key={pattern.id}>
        <button type="button" data-pattern={pattern.id} aria-current={pattern.id === selected ? "true" : undefined}
          disabled={busy} draggable={!busy} onDragStart={(event) => {
            if (busy || !state.canonical) { event.preventDefault(); return; }
            event.dataTransfer.effectAllowed = "copy";
            event.dataTransfer.setData(PATTERN_DRAG_TYPE, JSON.stringify({ generation: state.canonical.generation, pattern: pattern.id }));
          }} onClick={() => { void act({ kind: "select", pattern: pattern.id }); }}
          data-hint="Selects this pattern for the rack and piano roll.">{pattern.name}</button>
      </li>)}
    </ul>
    <div className="pattern-rail-actions">
      <button type="button" disabled={busy || selected === undefined} onClick={() => { if (selected !== undefined) void act({ kind: "duplicate", pattern: selected }); }}>Duplicate</button>
      <button type="button" disabled={busy || selected === undefined} onClick={() => {
        const pattern = state.canonical?.patterns.find((item) => item.id === selected);
        if (pattern) setEditing({ id: pattern.id, name: pattern.name });
      }}>Rename</button>
    </div>
    {editing && <form className="pattern-rail-rename" onSubmit={(event) => {
      event.preventDefault(); const captured = editing;
      if (!captured.name.trim()) return;
      void act({ kind: "rename", pattern: captured.id, name: captured.name.trim() }).then((accepted) => {
        if (accepted) { setEditing(null); focusSelected(); }
      });
    }}>
      <label>Pattern name<input autoFocus value={editing.name} disabled={busy} onChange={(event) => setEditing({ ...editing, name: event.target.value })}
        onKeyDown={(event) => { if (event.key === "Escape") { event.preventDefault(); setEditing(null); focusSelected(); } }} /></label>
      <button type="submit" disabled={busy || !editing.name.trim()}>Save name</button>
      <button type="button" onClick={() => { setEditing(null); focusSelected(); }}>Cancel</button>
    </form>}
    {state.pending && <p role="status">Updating patterns…</p>}
    {message && <p role="alert">{message}</p>}
    <button type="button" disabled={busy} onClick={() => { void picker.reload(); }}>Refresh patterns</button>
  </aside>;
}

export function PatternModeControls({ picker }: { picker: PatternPicker }) {
  const pattern = picker.state.canonical?.patterns.find((item) => item.id === picker.state.canonical?.selected);
  return <div className="pattern-mode-controls">
    <label>Playback<select aria-label="Playback mode" value={picker.mode ?? ""}
      disabled={picker.modePending || Boolean(picker.state.pending) || picker.mode === null}
      onChange={(event) => { if (event.target.value === "pattern" || event.target.value === "song") void picker.changeMode(event.target.value); }}>
      {picker.mode === null && <option value="">Loading…</option>}
      <option value="pattern">Pattern</option><option value="song">Song</option>
    </select></label>
    <span className="pattern-current" title={pattern?.name} role={picker.modePending ? "status" : undefined}>
      {picker.modePending ? "Changing mode…" : pattern?.name ?? "Loading patterns…"}
    </span>
  </div>;
}
