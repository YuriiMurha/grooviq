import type { WireNote } from "../api/notes";

/** An edit made in the piano roll, applied to the store at once and sent to the engine after.
 * Added notes carry temporary negative ids until the engine gives them real ones. */
export interface LocalEdit {
  add: WireNote[];
  update: WireNote[];
  delete: number[];
}

const KEYS = 128;
/** Bucket sort keys are `tick * INDEX_SPAN + index`: exact in a double for ticks below 2^32
 * and up to 131,072 notes (the pattern holds at most 100,000). */
const INDEX_SPAN = 131_072;

/** The pattern's notes as the piano roll mirrors them: a struct of arrays (one slot per note, in
 * no particular order), an id → slot map, and per key the slots sorted by start tick, for hit
 * tests and box selection without touching every note. */
export class NoteStore {
  /** The engine's notes revision the store was last in step with. Local edits keep it. */
  revision = 0;
  /** Bumped by every change to the notes or the selection: what the renderer rebuilds on. */
  version = 0;
  count = 0;

  ids = new Float64Array(0);
  channels = new Uint32Array(0);
  ticks = new Float64Array(0);
  lengths = new Float64Array(0);
  velocities = new Float64Array(0);
  keys = new Uint8Array(0);
  slides = new Uint8Array(0);
  selected = new Uint8Array(0);

  private slotOf = new Map<number, number>();
  /** Slots, grouped by key and sorted by tick inside each key: key k is order[start[k]..start[k + 1]). */
  private order = new Int32Array(0);
  private sortKeys = new Float64Array(0);
  private start = new Int32Array(KEYS + 1);
  /** The longest note in each key: how far back a hit test has to look. */
  private longest = new Float64Array(KEYS);

  /** Replaces every note (a fetch from the engine). Selected ids that are still there stay selected. */
  load(revision: number, notes: readonly WireNote[]) {
    const keep = new Set(this.selectedIds());
    this.count = 0;
    this.slotOf.clear();
    this.reserve(notes.length);
    for (const n of notes) this.push(n);
    for (let i = 0; i < this.count; i++) this.selected[i] = keep.has(this.ids[i]) ? 1 : 0;
    this.revision = revision;
    this.changed();
  }

  /** Applies an edit: deletes, then updates, then adds, as the engine does. Unknown ids are skipped. */
  apply(edit: LocalEdit) {
    for (const id of edit.delete) this.remove(id);
    for (const n of edit.update) {
      const slot = this.slotOf.get(n.id);
      if (slot !== undefined) this.write(slot, n);
    }
    this.reserve(this.count + edit.add.length);
    for (const n of edit.add) {
      if (this.slotOf.has(n.id)) this.write(this.slotOf.get(n.id)!, n);
      else this.push(n);
    }
    this.changed();
  }

  /** A temporary id became the engine's id. */
  remap(from: number, to: number) {
    const slot = this.slotOf.get(from);
    if (slot === undefined) return;
    this.slotOf.delete(from);
    this.slotOf.set(to, slot);
    this.ids[slot] = to;
  }

  get(id: number): WireNote | undefined {
    const slot = this.slotOf.get(id);
    return slot === undefined ? undefined : this.note(slot);
  }

  has(id: number) {
    return this.slotOf.has(id);
  }

  note(slot: number): WireNote {
    return {
      id: this.ids[slot],
      channel: this.channels[slot],
      tick: this.ticks[slot],
      length: this.lengths[slot],
      velocity: this.velocities[slot],
      key: this.keys[slot],
      slide: this.slides[slot] === 1,
    };
  }

  /** The ids of a channel's notes. */
  channelNotes(channel: number): number[] {
    const out: number[] = [];
    for (let i = 0; i < this.count; i++) if (this.channels[i] === channel) out.push(this.ids[i]);
    return out;
  }

  /** The note of `channel` under (tick, key row): the one that starts latest, which is the one
   * drawn on top. */
  hitTest(tick: number, key: number, channel: number): number | null {
    const k = Math.floor(key);
    if (k < 0 || k >= KEYS || tick < 0) return null;
    const from = this.start[k];
    // The last note in the key that starts at or before the tick.
    let lo = from;
    let hi = this.start[k + 1];
    const limit = (Math.floor(tick) + 1) * INDEX_SPAN;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (this.sortKeys[mid] < limit) lo = mid + 1;
      else hi = mid;
    }
    const reach = tick - this.longest[k];
    for (let p = lo - 1; p >= from; p--) {
      const slot = this.order[p];
      const s = this.ticks[slot];
      if (s < reach) break;
      if (this.channels[slot] === channel && tick < s + this.lengths[slot]) return this.ids[slot];
    }
    return null;
  }

  /** The ids of `channel`'s notes that overlap ticks [t0, t1) in key rows k0..=k1 (corners in
   * any order). */
  boxSelect(t0: number, t1: number, k0: number, k1: number, channel: number): number[] {
    const [ta, tb] = t0 <= t1 ? [t0, t1] : [t1, t0];
    const ka = Math.max(0, Math.floor(Math.min(k0, k1)));
    const kb = Math.min(KEYS - 1, Math.floor(Math.max(k0, k1)));
    const out: number[] = [];
    for (let k = ka; k <= kb; k++) {
      const end = this.start[k + 1];
      for (let p = this.start[k]; p < end; p++) {
        const slot = this.order[p];
        const s = this.ticks[slot];
        if (s >= tb) break;
        if (this.channels[slot] === channel && s + this.lengths[slot] > ta) out.push(this.ids[slot]);
      }
    }
    return out;
  }

  setSelection(ids: Iterable<number>) {
    this.selected.fill(0);
    for (const id of ids) {
      const slot = this.slotOf.get(id);
      if (slot !== undefined) this.selected[slot] = 1;
    }
    this.version++;
  }

  isSelected(id: number) {
    const slot = this.slotOf.get(id);
    return slot !== undefined && this.selected[slot] === 1;
  }

  selectedIds(): number[] {
    const out: number[] = [];
    for (let i = 0; i < this.count; i++) if (this.selected[i]) out.push(this.ids[i]);
    return out;
  }

  /** The selected notes, of `channel` only when given. */
  selectedNotes(channel?: number): WireNote[] {
    const out: WireNote[] = [];
    for (let i = 0; i < this.count; i++) if (this.selected[i] && (channel === undefined || this.channels[i] === channel)) out.push(this.note(i));
    return out;
  }

  /** Slots in drawing order: by key, then by start tick (later starts on top). */
  drawOrder(): Int32Array {
    return this.order.subarray(0, this.count);
  }

  private reserve(n: number) {
    if (n <= this.ids.length) return;
    const cap = Math.max(n, this.ids.length * 2, 1024);
    const grow = <T extends Float64Array | Uint32Array | Uint8Array>(a: T, make: (n: number) => T) => {
      const b = make(cap);
      b.set(a);
      return b;
    };
    this.ids = grow(this.ids, (c) => new Float64Array(c));
    this.channels = grow(this.channels, (c) => new Uint32Array(c));
    this.ticks = grow(this.ticks, (c) => new Float64Array(c));
    this.lengths = grow(this.lengths, (c) => new Float64Array(c));
    this.velocities = grow(this.velocities, (c) => new Float64Array(c));
    this.keys = grow(this.keys, (c) => new Uint8Array(c));
    this.slides = grow(this.slides, (c) => new Uint8Array(c));
    this.selected = grow(this.selected, (c) => new Uint8Array(c));
  }

  private push(n: WireNote) {
    const slot = this.count++;
    this.slotOf.set(n.id, slot);
    this.selected[slot] = 0;
    this.write(slot, n);
  }

  private write(slot: number, n: WireNote) {
    this.ids[slot] = n.id;
    this.channels[slot] = n.channel;
    this.ticks[slot] = n.tick;
    this.lengths[slot] = n.length;
    this.velocities[slot] = n.velocity;
    this.keys[slot] = n.key;
    this.slides[slot] = n.slide ? 1 : 0;
  }

  /** Removes a note by moving the last one into its slot. */
  private remove(id: number) {
    const slot = this.slotOf.get(id);
    if (slot === undefined) return;
    this.slotOf.delete(id);
    const last = --this.count;
    if (slot === last) return;
    this.ids[slot] = this.ids[last];
    this.channels[slot] = this.channels[last];
    this.ticks[slot] = this.ticks[last];
    this.lengths[slot] = this.lengths[last];
    this.velocities[slot] = this.velocities[last];
    this.keys[slot] = this.keys[last];
    this.slides[slot] = this.slides[last];
    this.selected[slot] = this.selected[last];
    this.slotOf.set(this.ids[slot], slot);
  }

  /** Rebuilds the per-key buckets: a counting sort by key, then a native numeric sort of each
   * key's `tick * INDEX_SPAN + slot`. */
  private changed() {
    const n = this.count;
    if (this.order.length < n) {
      this.order = new Int32Array(this.ids.length);
      this.sortKeys = new Float64Array(this.ids.length);
    }
    const start = this.start;
    start.fill(0);
    this.longest.fill(0);
    for (let i = 0; i < n; i++) {
      const k = this.keys[i];
      start[k + 1]++;
      if (this.lengths[i] > this.longest[k]) this.longest[k] = this.lengths[i];
    }
    for (let k = 0; k < KEYS; k++) start[k + 1] += start[k];
    const fill = start.slice(0, KEYS);
    for (let i = 0; i < n; i++) this.sortKeys[fill[this.keys[i]]++] = this.ticks[i] * INDEX_SPAN + i;
    for (let k = 0; k < KEYS; k++) {
      if (start[k + 1] - start[k] > 1) this.sortKeys.subarray(start[k], start[k + 1]).sort();
    }
    for (let p = 0; p < n; p++) this.order[p] = this.sortKeys[p] % INDEX_SPAN;
    this.version++;
  }
}

/** Floats per note instance: tick, length, key, velocity, flags. */
export const INSTANCE_STRIDE = 5;
export const FLAG_SELECTED = 1;
export const FLAG_SLIDE = 2;
/** A note of another channel, drawn faint and never edited. Its channel's ink is `(flags >> 3) - 1`. */
export const FLAG_GHOST = 4;
export const INK_SHIFT = 3;

/** The buffer `buildInstances` fills, kept between builds: a rebuild allocates nothing. */
let scratch = new Float32Array(0);

/** The renderer's instance records for one channel's notes, in drawing order; with `ghosts`, the
 * notes of the other channels come first (faint, in their own channel's ink, never selected),
 * so the channel's own notes draw on top. The records are valid until the next build
 * (renderers copy or redraw them before then). */
export function buildInstances(
  store: NoteStore,
  channel: number,
  ghosts?: { ghosts: boolean; inkOf: (channel: number) => number },
): { data: Float32Array; count: number; own: number } {
  const order = store.drawOrder();
  const want = ghosts?.ghosts ? order.length : 0;
  if (scratch.length < order.length * INSTANCE_STRIDE) scratch = new Float32Array(Math.max(order.length * INSTANCE_STRIDE, scratch.length * 2));
  const data = scratch;
  let o = 0;
  const put = (slot: number, flags: number) => {
    data[o] = store.ticks[slot];
    data[o + 1] = store.lengths[slot];
    data[o + 2] = store.keys[slot];
    data[o + 3] = store.velocities[slot];
    data[o + 4] = flags | (store.slides[slot] ? FLAG_SLIDE : 0);
    o += INSTANCE_STRIDE;
  };
  if (want && ghosts) {
    // The ink of each channel is looked up once per run of notes, not per note.
    let last = -1;
    let flags = 0;
    for (let p = 0; p < order.length; p++) {
      const slot = order[p];
      const c = store.channels[slot];
      if (c === channel) continue;
      if (c !== last) {
        last = c;
        flags = FLAG_GHOST | ((ghosts.inkOf(c) + 1) << INK_SHIFT);
      }
      put(slot, flags);
    }
  }
  const ghostCount = o / INSTANCE_STRIDE;
  for (let p = 0; p < order.length; p++) {
    const slot = order[p];
    if (store.channels[slot] === channel) put(slot, store.selected[slot] ? FLAG_SELECTED : 0);
  }
  const count = o / INSTANCE_STRIDE;
  return { data: data.subarray(0, o), count, own: count - ghostCount };
}
