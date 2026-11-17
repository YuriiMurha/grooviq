import { describe, expect, it } from "vitest";
import {
  DEFAULT_BASE,
  HeldKeys,
  LOWER_ROW,
  UPPER_ROW,
  accepts,
  keyFor,
  shiftOctave,
  type KeyEventLike,
} from "./typingKeyboard";

const press = (code: string, extra: Partial<KeyEventLike> = {}): KeyEventLike => ({
  code,
  repeat: false,
  ctrlKey: false,
  metaKey: false,
  altKey: false,
  target: null,
  ...extra,
});

describe("the typing keyboard", () => {
  it("maps_both_rows_from_the_base", () => {
    expect(DEFAULT_BASE).toBe(48);
    LOWER_ROW.forEach((code, i) => expect(keyFor(code, 48)).toBe(48 + i));
    UPPER_ROW.forEach((code, i) => expect(keyFor(code, 48)).toBe(60 + i));
    expect(keyFor("KeyZ", 48)).toBe(48);
    expect(keyFor("KeyS", 48)).toBe(49);
    expect(keyFor("Slash", 48)).toBe(64);
    expect(keyFor("KeyQ", 48)).toBe(60);
    expect(keyFor("KeyP", 48)).toBe(76);
    expect(keyFor("KeyQ", 0)).toBe(12);
    // Keys outside the two rows, and notes past 127, play nothing.
    expect(keyFor("KeyA", 48)).toBeNull();
    expect(keyFor("Space", 48)).toBeNull();
    expect(keyFor("Minus", 48)).toBeNull();
    expect(keyFor("KeyP", 108)).toBeNull();
    expect(keyFor("KeyT", 108)).toBe(127);
    expect(keyFor("Digit6", 108)).toBeNull();
  });

  it("octave_keys_shift_within_0_108", () => {
    expect(shiftOctave(48, "Equal")).toBe(60);
    expect(shiftOctave(48, "Minus")).toBe(36);
    expect(shiftOctave(0, "Minus")).toBe(0);
    expect(shiftOctave(108, "Equal")).toBe(108);
    expect(shiftOctave(100, "Equal")).toBe(108);
    expect(shiftOctave(5, "Minus")).toBe(0);
    expect(shiftOctave(48, "KeyZ")).toBe(48);
  });

  it("ignores_repeats_modifiers_and_inputs", () => {
    expect(accepts(press("KeyZ"))).toBe(true);
    // Shift is allowed (it is not a modifier for the typing keyboard).
    expect(accepts(press("KeyZ", { shiftKey: true } as Partial<KeyEventLike>))).toBe(true);
    expect(accepts(press("KeyZ", { repeat: true }))).toBe(false);
    expect(accepts(press("KeyZ", { ctrlKey: true }))).toBe(false);
    expect(accepts(press("KeyZ", { metaKey: true }))).toBe(false);
    expect(accepts(press("KeyZ", { altKey: true }))).toBe(false);
    expect(accepts(press("KeyZ", { target: { tagName: "INPUT", type: "text" } }))).toBe(false);
    expect(accepts(press("KeyZ", { target: { tagName: "INPUT", type: "number" } }))).toBe(false);
    expect(accepts(press("KeyZ", { target: { tagName: "TEXTAREA" } }))).toBe(false);
    expect(accepts(press("KeyZ", { target: { tagName: "DIV", isContentEditable: true } }))).toBe(false);
    // A slider or a button keeps the keyboard.
    expect(accepts(press("KeyZ", { target: { tagName: "INPUT", type: "range" } }))).toBe(true);
    expect(accepts(press("KeyZ", { target: { tagName: "BUTTON" } }))).toBe(true);
  });

  it("releases_what_it_played_where_it_played_it", () => {
    const held = new HeldKeys();
    // The note sounding is the one pressed, even if the octave moved before the release.
    expect(held.press("KeyZ", 48, 1)).toEqual({ channel: 1, key: 48 });
    expect(held.press("KeyZ", 48, 1)).toBeNull();
    expect(held.press("KeyQ", 48, 1)).toEqual({ channel: 1, key: 60 });
    expect(held.press("KeyA", 48, 1)).toBeNull();
    expect(held.release("KeyZ")).toEqual({ channel: 1, key: 48 });
    expect(held.release("KeyZ")).toBeNull();
    expect(held.press("KeyX", 60, 2)).toEqual({ channel: 2, key: 62 });
    expect(held.releaseAll()).toEqual([
      { channel: 1, key: 60 },
      { channel: 2, key: 62 },
    ]);
    expect(held.releaseAll()).toEqual([]);
    // Two codes on one note (after an octave shift) sound it once and release it once.
    expect(held.press("KeyQ", 48, 1)).toEqual({ channel: 1, key: 60 });
    expect(held.press("KeyZ", 60, 1)).toBeNull();
    expect(held.release("KeyZ")).toBeNull();
    expect(held.release("KeyQ")).toEqual({ channel: 1, key: 60 });
  });
});
