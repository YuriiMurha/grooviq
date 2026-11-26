// The mixer's view contract and the logic behind its controls. The components are controlled:
// the host supplies every track, option and error by opaque string ids and stays the authority.
// The mixer only emits commands addressed by those ids. It never stores a project, invents engine
// state, validates routes or assigns sources.

import type { Gesture } from "../devices/paramGestures";
import { DEAD_GESTURE } from "../devices/paramGestures";
import type { ParamRange } from "../devices/controls/paramDrag";

/** One choice the host offers (an output, a send destination, an effect, a detector source). */
export interface MixerOption {
  id: string;
  name: string;
}

/** One insert slot on a track's chain. */
export interface MixerInsert {
  /** Stable while the slot exists; never reused for another slot. */
  id: string;
  name: string;
  /** False while bypassed. */
  active: boolean;
  /** Present when the effect has a detector input the host can feed (a sidechain). */
  detector?: { options: readonly MixerOption[]; sourceId: string | null };
}

/** One send from a track. */
export interface MixerSend {
  id: string;
  destinationId: string;
  /** Linear gain 0..4; 0 is off and 1 is unity. */
  amount: number;
  /** Taken before the track fader (true) or after it. */
  pre: boolean;
}

/** One mixer channel as the host holds it. */
export interface MixerTrack {
  /** View identity for this mounted mixer: qualify ids by project generation, or remount on
   * project replacement. Reusing an id for another track would retarget a live gesture. */
  id: string;
  name: string;
  /** Supplied description ("Instrument", "Bus", "Return"). */
  kind: string;
  /** Riso ink 1..6. */
  ink: number;
  /** Fader view value in LEVEL's range. LEVEL.min is the off sentinel, not a finite -60 dB
   * engine gain; the host adapter maps it to/from silence. */
  level: number;
  /** -1 (left) .. 1 (right). */
  pan: number;
  mute: boolean;
  solo: boolean;
  /** The clip indicator is latched. */
  clipped: boolean;
  /** Where its output goes, `null` while unassigned; not used for the master. */
  outputId: string | null;
  /** Outputs the host allows; the mixer does not check routes itself. */
  outputs: readonly MixerOption[];
  inserts: readonly MixerInsert[];
  sends: readonly MixerSend[];
  /** Send destinations the host allows. */
  sendDestinations: readonly MixerOption[];
  /** The host is applying a change: controls show the supplied state and take no input. */
  pending?: boolean;
}

/** The master: no output choice and no sends. */
export interface MixerMaster extends MixerTrack {
  /** What the master plays to, as words ("Audio device"). */
  outputLabel: string;
}

/** A host error, shown until the host clears it. */
export interface MixerError {
  /** The track it concerns, if any. */
  nodeId: string | null;
  message: string;
}

/** Linear peaks per channel, 0..1+ (1 is full scale), for the meters that are on screen. */
export interface MeterSource {
  /** Writes the latest peaks of `nodeId` into `out` (left, right); false while there are none. */
  read(nodeId: string, out: Float32Array): boolean;
}

export type ParamTarget = { kind: "level" | "pan"; nodeId: string } | { kind: "send"; nodeId: string; sendId: string };

/** A continuous control: live while it moves, then one commit or a cancel (back to `value`). */
export interface ParamCommand {
  target: ParamTarget;
  /** View units: level uses LEVEL's off sentinel, pan is bipolar, send is linear gain.
   * The host adapter converts these to engine units; this is not a persistence format. */
  value: number;
  phase: "live" | "commit" | "cancel";
}

export type MixerCommand =
  | ({ type: "param" } & ParamCommand)
  | { type: "select"; nodeId: string }
  | { type: "mute"; nodeId: string; on: boolean }
  | { type: "solo"; nodeId: string; on: boolean }
  | { type: "clearClip"; nodeId: string }
  | { type: "setOutput"; nodeId: string; outputId: string }
  | { type: "addInsert"; nodeId: string; effectId: string }
  | { type: "openInsert"; nodeId: string; slotId: string }
  | { type: "setInsertActive"; nodeId: string; slotId: string; active: boolean }
  /** Moves the slot before `beforeSlotId` (null: to the end). */
  | { type: "moveInsert"; nodeId: string; slotId: string; beforeSlotId: string | null }
  | { type: "removeInsert"; nodeId: string; slotId: string }
  | { type: "setDetector"; nodeId: string; slotId: string; sourceId: string | null }
  /** A new send is post-fader and off: the host may refuse or adjust. */
  | { type: "addSend"; nodeId: string; destinationId: string; pre: false; amount: 0 }
  | { type: "setSendPre"; nodeId: string; sendId: string; pre: boolean }
  | { type: "removeSend"; nodeId: string; sendId: string };

/** What the host answers: nothing (it updates its props), or a promise settling when it has. */
export type CommandResult = void | PromiseLike<unknown>;

export interface MixerProps {
  tracks: readonly MixerTrack[];
  master: MixerMaster;
  /** Effects that can be added. */
  effects: readonly MixerOption[];
  /** The selected track (or master) id; a stale id shows the removed-selection state. */
  selectedId: string | null;
  error?: MixerError | null;
  /** Supplied reason for each control the host cannot support. */
  unsupported?: Partial<Record<"mute" | "solo" | "bypass" | "detector", string>>;
  /** Supplied meter data; without it the meters stay empty. */
  meters?: MeterSource;
  onCommand: (command: MixerCommand) => CommandResult;
}

export function unsupportedReason(unsupported: MixerProps["unsupported"], type: MixerCommand["type"]): string | undefined {
  switch (type) {
    case "mute": return unsupported?.mute;
    case "solo": return unsupported?.solo;
    case "setInsertActive": return unsupported?.bypass;
    case "setDetector": return unsupported?.detector;
    default: return undefined;
  }
}

/** The detector's local None choice must not collide with a host's opaque id. */
export function unusedOptionId(options: readonly MixerOption[]): string {
  const ids = new Set(options.map(({ id }) => id));
  let value = "";
  while (ids.has(value)) value += " ";
  return value;
}

/** A current destination may need a display label even when the host excludes it from the picker. */
export function outputChoices(options: readonly MixerOption[], outputId: string | null, nodes: ReadonlyMap<string, { name: string }>) {
  const current = outputId === null ? undefined : options.find((option) => option.id === outputId);
  const label = current?.name ?? (outputId === null ? "Unassigned" : nodes.get(outputId)?.name ?? "Unavailable output");
  return { options, label };
}

/** Keep the host's order while excluding existing sends and the main output. */
export function availableSends({ sendDestinations, sends, outputId }: Pick<MixerTrack, "sendDestinations" | "sends" | "outputId">): MixerOption[] {
  const used = new Set(sends.map((send) => send.destinationId));
  return sendDestinations.filter(({ id }) => id !== outputId && !used.has(id));
}

/** Fader view: the finite bottom stop denotes off, matching the approved prototype. */
export const LEVEL: ParamRange = { min: -60, max: 6, default: 0, curve: "lin", stepped: false };
export const PAN: ParamRange = { min: -1, max: 1, default: 0, curve: "lin", stepped: false };
export const SEND: ParamRange = { min: 0, max: 4, default: 0, curve: "lin", stepped: false };

export function formatDb(db: number): string {
  if (db <= LEVEL.min) return "−∞ dB";
  const r = Math.round(db * 10) / 10;
  return `${r > 0 ? "+" : r < 0 ? "−" : ""}${Math.abs(r).toFixed(1)} dB`;
}

export function formatPan(pan: number): string {
  const p = Math.round(pan * 100);
  return p === 0 ? "C" : p < 0 ? `L${-p}` : `R${p}`;
}

export const formatPercent = (v: number) => `${Math.round(v * 100)}%`;

// ---------------- windowing ----------------

export interface Window {
  /** First strip drawn. */
  start: number;
  /** One past the last strip drawn. */
  end: number;
}

/** The strips to draw for a horizontal scroll position: the visible ones plus `overscan` on each
 * side. Bounded by the viewport, whatever the list length. */
export function visibleWindow(o: { count: number; scroll: number; viewport: number; pitch: number; overscan: number }): Window {
  if (o.count <= 0 || !(o.pitch > 0)) return { start: 0, end: 0 };
  const viewport = Number.isFinite(o.viewport) && o.viewport > 0 ? o.viewport : 0;
  const maxScroll = Math.max(0, o.count * o.pitch - viewport);
  const scroll = Number.isFinite(o.scroll) ? Math.min(maxScroll, Math.max(0, o.scroll)) : 0;
  const first = Math.min(o.count - 1, Math.floor(scroll / o.pitch));
  const last = Math.max(first + 1, Math.ceil((scroll + viewport) / o.pitch));
  return { start: Math.max(0, first - o.overscan), end: Math.min(o.count, last + o.overscan) };
}

/** The window plus pinned strips (focused, selected, mid-gesture) outside it, ascending. */
export function renderIndices(w: Window, pinned: readonly number[], count: number): number[] {
  const out = new Set<number>();
  // The window may predate a host update that shortened the list.
  for (let i = Math.max(0, w.start); i < Math.min(w.end, count); i++) out.add(i);
  for (const i of pinned) if (Number.isInteger(i) && i >= 0 && i < count) out.add(i);
  return [...out].sort((a, b) => a - b);
}

/** The scroll position that brings strip `index` fully into view, moving as little as possible. */
export function revealScroll(index: number, scroll: number, viewport: number, pitch: number): number {
  const left = index * pitch;
  const right = left + pitch;
  if (left < scroll) return left;
  if (right > scroll + viewport) return right - viewport;
  return scroll;
}

/** The strip a key on a strip's name moves to, or null for a key strip navigation leaves alone
 * (the level and pan controls keep their arrows: this only runs on the name button). */
export function navTarget(ids: readonly string[], current: string | null, key: string): string | null {
  if (ids.length === 0) return null;
  if (key === "Home") return ids[0];
  if (key === "End") return ids[ids.length - 1];
  const dir = key === "ArrowRight" ? 1 : key === "ArrowLeft" ? -1 : 0;
  if (dir === 0) return null;
  const i = current === null ? -1 : ids.indexOf(current);
  if (i < 0) return ids[0];
  return ids[Math.min(ids.length - 1, Math.max(0, i + dir))];
}

/** Where moving a slot one place earlier (-1) or later (1) puts it, named by the slot it lands
 * before (null: the end); null when it cannot move. */
export function moveTarget(slots: readonly { id: string }[], slotId: string, dir: -1 | 1): { beforeSlotId: string | null } | null {
  const i = slots.findIndex((s) => s.id === slotId);
  if (i < 0) return null;
  const j = i + dir;
  if (j < 0 || j >= slots.length) return null;
  if (dir < 0) return { beforeSlotId: slots[j].id };
  return { beforeSlotId: slots[i + 2]?.id ?? null };
}

// ---------------- gestures ----------------

export const targetKey = (t: ParamTarget) => (t.kind === "send" ? `send\u0000${t.nodeId}\u0000${t.sendId}` : `${t.kind}\u0000${t.nodeId}`);

export interface GestureHost {
  emit(command: ParamCommand): CommandResult;
  /** The host's value of the target, or undefined once the target is gone. */
  stored(target: ParamTarget): number | undefined;
  /** The host is busy with the target's track. */
  locked(target: ParamTarget): boolean;
  /** The values shown instead of the host's changed. */
  changed(): void;
}

interface Active {
  token: number;
  target: ParamTarget;
  origin: number;
  originPending?: number;
  heard: boolean;
}

interface Pending {
  seq: number;
  target: ParamTarget;
  value: number;
  visible: boolean;
}

/** The mixer's continuous controls. Every gesture is bound to the target it began on: once that
 * target is gone, or its track becomes pending, it ends, and nothing it does later reaches the
 * host. A committed value stays shown until the host settles it, then the host's value shows. */
export class MixerGestures {
  private active = new Map<string, Active>();
  private moving = new Map<string, number>();
  private pending = new Map<string, Pending>();
  private listeners = new Set<() => void>();
  private nextToken = 1;
  private seq = 1;
  private closed = false;
  private host: GestureHost = { emit: () => {}, stored: () => undefined, locked: () => true, changed: () => {} };

  /** Points the gestures at the host's latest props and callbacks (every render). */
  connect(host: GestureHost) {
    this.host = host;
  }

  /** Memoized strips can read live send previews without new props on every gesture update. */
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };

  private changed() {
    this.host.changed();
    for (const listener of this.listeners) listener();
  }

  /** The value to show instead of the host's, if any. */
  shown(t: ParamTarget): number | undefined {
    if (this.host.locked(t)) return undefined;
    const key = targetKey(t);
    const pending = this.pending.get(key);
    return this.moving.get(key) ?? (pending?.visible ? pending.value : undefined);
  }

  begin(t: ParamTarget): Gesture {
    if (this.closed || this.host.locked(t)) return DEAD_GESTURE;
    const stored = this.host.stored(t);
    if (stored === undefined) return DEAD_GESTURE;
    const key = targetKey(t);
    const before = this.active.get(key);
    const pending = this.pending.get(key);
    const token = this.nextToken++;
    // A gesture already on the target (a wheel turn, then a drag) hands over and keeps its start.
    this.active.set(key, {
      token, target: t,
      origin: before?.origin ?? (pending?.visible ? pending.value : stored),
      originPending: before ? before.originPending : pending?.seq,
      heard: before?.heard ?? false,
    });
    const mine = () => !this.closed && this.active.get(key)?.token === token;
    return {
      alive: mine,
      input: (value) => {
        if (!mine()) return false;
        if (!Number.isFinite(value)) return true;
        this.active.get(key)!.heard = true;
        this.show(key, value);
        void this.host.emit({ target: t, value, phase: "live" });
        return true;
      },
      commit: (value) => {
        if (!mine()) return;
        const a = this.active.get(key)!;
        if (!Number.isFinite(value) || value === a.origin) {
          this.end(key, true);
          return;
        }
        this.active.delete(key);
        this.store(t, value);
      },
      cancel: () => {
        if (mine()) this.end(key, true);
      },
    };
  }

  /** A one-shot change (a key, a double-click), after ending any gesture on the target. */
  set(t: ParamTarget, value: number) {
    if (this.closed || !Number.isFinite(value) || this.host.locked(t)) return;
    const stored = this.host.stored(t);
    if (stored === undefined) return;
    const key = targetKey(t);
    if (this.active.has(key)) this.end(key, true);
    if (value === (this.shown(t) ?? stored)) return;
    this.store(t, value);
  }

  /** Call after every host update: ends gestures on removed targets silently, and cancels (on
   * their own ids) gestures on tracks that became pending. */
  retain() {
    for (const [key, p] of [...this.pending]) {
      if (this.host.stored(p.target) === undefined) this.drop(key);
      else if (this.host.locked(p.target) && p.visible) {
        // Once the host takes control, an old preview must not reappear on unlock.
        p.visible = false;
        this.changed();
      }
    }
    for (const [key, a] of [...this.active]) {
      const stored = this.host.stored(a.target);
      if (stored === undefined) this.drop(key);
      else if (this.host.locked(a.target)) {
        a.origin = stored;
        a.originPending = undefined;
        this.end(key, true);
      }
    }
    for (const key of [...this.moving.keys()]) {
      if (!this.active.has(key)) this.unshow(key);
    }
  }

  /** End live gestures while keeping already-issued commits and future input intact. */
  cancelAll(): boolean {
    const hadActive = this.active.size > 0;
    for (const key of [...this.active.keys()]) if (this.active.has(key)) this.end(key, true);
    return hadActive;
  }

  /** Takes gestures again after `dispose` (a remount). */
  resume() {
    this.closed = false;
  }

  /** Cancels everything and refuses new gestures (the mixer unmounted). */
  dispose() {
    this.cancelAll();
    this.closed = true;
  }

  private show(key: string, value: number) {
    this.moving.set(key, value);
    this.changed();
  }

  private unshow(key: string) {
    if (this.moving.delete(key)) this.changed();
  }

  /** A removed target: forget it without a word to the host. */
  private drop(key: string) {
    this.active.delete(key);
    this.pending.delete(key);
    this.unshow(key);
  }

  private end(key: string, restore: boolean) {
    const a = this.active.get(key)!;
    this.active.delete(key);
    if (restore && a.heard) void this.host.emit({ target: a.target, value: a.origin, phase: "cancel" });
    this.unshow(key);
  }

  private store(t: ParamTarget, value: number) {
    const key = targetKey(t);
    const seq = this.seq++;
    this.pending.set(key, { seq, target: t, value, visible: true });
    this.moving.delete(key);
    this.changed();
    const result = this.host.emit({ target: t, value, phase: "commit" });
    const settle = () => {
      if (this.pending.get(key)?.seq !== seq) return;
      this.pending.delete(key);
      const active = this.active.get(key);
      if (active?.originPending === seq) {
        // The older commit may have been refused: only the settled host value is authoritative.
        active.origin = this.host.stored(t) ?? active.origin;
        active.originPending = undefined;
      }
      this.changed();
    };
    if (result && typeof result.then === "function") result.then(settle, settle);
    else settle();
  }
}
