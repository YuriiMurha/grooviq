import { isTypingTarget, type ShortcutEvent } from "../project/shortcuts";

// The computer keyboard as a two-octave piano, by `KeyboardEvent.code` so it is the same on
// every layout. Pure functions and the held-key book-keeping; `useTypingKeyboard` wires them.

/** From `base`: Z S X D C V G B H N J M , L . ; / */
export const LOWER_ROW = [
  "KeyZ", "KeyS", "KeyX", "KeyD", "KeyC", "KeyV", "KeyG", "KeyB", "KeyH",
  "KeyN", "KeyJ", "KeyM", "Comma", "KeyL", "Period", "Semicolon", "Slash",
] as const;
/** From `base + 12`: Q 2 W 3 E R 5 T 6 Y 7 U I 9 O 0 P */
export const UPPER_ROW = [
  "KeyQ", "Digit2", "KeyW", "Digit3", "KeyE", "KeyR", "Digit5", "KeyT", "Digit6",
  "KeyY", "Digit7", "KeyU", "KeyI", "Digit9", "KeyO", "Digit0", "KeyP",
] as const;

export const DEFAULT_BASE = 48;
export const BASE_MIN = 0;
export const BASE_MAX = 108;
const KEY_MAX = 127;

const OFFSETS = new Map<string, number>([
  ...LOWER_ROW.map((code, i) => [code, i] as [string, number]),
  ...UPPER_ROW.map((code, i) => [code, 12 + i] as [string, number]),
]);

/** The note a key plays from `base`, or `null` for a key that plays none. */
export function keyFor(code: string, base: number): number | null {
  const offset = OFFSETS.get(code);
  if (offset === undefined) return null;
  const key = base + offset;
  return key >= 0 && key <= KEY_MAX ? key : null;
}

/** `Minus` and `Equal` move the base an octave down or up, within 0..=108. */
export function shiftOctave(base: number, code: string): number {
  const step = code === "Minus" ? -12 : code === "Equal" ? 12 : 0;
  return Math.min(BASE_MAX, Math.max(BASE_MIN, base + step));
}

/** The part of a `KeyboardEvent` the keyboard reads. */
export interface KeyEventLike {
  code: string;
  repeat: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey?: boolean;
  target: ShortcutEvent["target"];
}

/** A key press the typing keyboard may take: not a repeat, no modifier but Shift, not typed
 * into a text field (or a menu or dialog). */
export function accepts(e: KeyEventLike): boolean {
  return !e.repeat && !e.ctrlKey && !e.metaKey && !e.altKey && !isTypingTarget(e.target);
}

/** A note sounding on a channel. */
export interface LiveNote {
  channel: number;
  key: number;
}

/** Which keys hold which note, so each note-off goes where its note-on went: the same key on the
 * same channel, even if the octave or the selected channel changed in between. */
export class HeldKeys {
  private byCode = new Map<string, LiveNote>();

  /** The note to start for this key, or `null` (no note, already held, or already sounding
   * from another key). */
  press(code: string, base: number, channel: number): LiveNote | null {
    if (this.byCode.has(code)) return null;
    const key = keyFor(code, base);
    if (key === null) return null;
    const note = { channel, key };
    const sounding = [...this.byCode.values()].some((n) => n.channel === channel && n.key === key);
    this.byCode.set(code, note);
    return sounding ? null : note;
  }

  /** The note to stop when this key comes up, or `null`. */
  release(code: string): LiveNote | null {
    const note = this.byCode.get(code);
    if (!note) return null;
    this.byCode.delete(code);
    const still = [...this.byCode.values()].some((n) => n.channel === note.channel && n.key === note.key);
    return still ? null : note;
  }

  /** Every sounding note, once each; nothing is held afterwards. */
  releaseAll(): LiveNote[] {
    const out: LiveNote[] = [];
    for (const n of this.byCode.values()) {
      if (!out.some((o) => o.channel === n.channel && o.key === n.key)) out.push(n);
    }
    this.byCode.clear();
    return out;
  }

  get size(): number {
    return this.byCode.size;
  }
}
