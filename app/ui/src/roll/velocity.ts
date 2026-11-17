import type { LocalEdit, NoteStore } from "./noteStore";

// The velocity lane under the roll: one bar per note, as tall as its velocity. A drag with notes
// selected sets all of them to the pointer's height; with none selected it paints the bars the
// pointer passes over. The values gather in a map while the pointer is down and go out as one edit.

/** The velocity at `y` pixels from the top of a lane `height` pixels high: full at the top,
 * silent at the bottom, clamped. */
export function velocityAt(y: number, height: number): number {
  return Math.min(1, Math.max(0, 1 - y / Math.max(1, height)));
}

/** Records `value` (clamped to 0..1) in `acc` for what the pointer at `tick` acts on: every
 * selected note of `channel`, or without a selection the notes of `channel` starting within
 * `reach` ticks of it. */
export function laneSet(store: NoteStore, channel: number, tick: number, value: number, reach: number, acc: Map<number, number>) {
  const v = Math.min(1, Math.max(0, value));
  let anySelected = false;
  for (let i = 0; i < store.count; i++) {
    if (store.channels[i] === channel && store.selected[i]) {
      anySelected = true;
      acc.set(store.ids[i], v);
    }
  }
  if (anySelected) return;
  for (let i = 0; i < store.count; i++) {
    if (store.channels[i] === channel && Math.abs(store.ticks[i] - tick) <= reach) acc.set(store.ids[i], v);
  }
}

/** The edit setting the velocities in `acc`; `null` when none of them changes anything. */
export function laneEdit(store: NoteStore, acc: ReadonlyMap<number, number>): LocalEdit | null {
  const update = [];
  for (const [id, velocity] of acc) {
    const n = store.get(id);
    if (n && Math.abs(n.velocity - velocity) > 1e-6) update.push({ ...n, velocity });
  }
  return update.length ? { add: [], update, delete: [] } : null;
}
