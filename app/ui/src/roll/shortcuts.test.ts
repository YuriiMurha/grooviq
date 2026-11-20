import { describe, expect, it } from "vitest";
import { rollShortcutFor } from "./shortcuts";
import type { ShortcutEvent } from "../project/shortcuts";

const event = (key: string, code?: string, rest: Partial<ShortcutEvent> = {}): ShortcutEvent => ({
  key, code, metaKey: false, ctrlKey: true, shiftKey: false, altKey: false, repeat: false, target: null, ...rest,
});

describe("roll edit shortcuts", () => {
  for (const platform of ["Win32", "MacIntel"]) {
    const mod = platform === "MacIntel" ? { metaKey: true, ctrlKey: false } : { ctrlKey: true };
    for (const [letter, code, action] of [["a", "KeyQ", "selectAll"], ["c", "KeyI", "copy"], ["v", "Period", "paste"], ["b", "KeyX", "duplicate"]] as const) {
      it(`${platform} printed ${letter} wins`, () => {
        expect(rollShortcutFor(event(letter, code, mod), platform)).toBe(action);
        expect(rollShortcutFor(event(letter.toUpperCase(), code, mod), platform)).toBe(action);
        expect(rollShortcutFor(event("q", `Key${letter.toUpperCase()}`, mod), platform)).toBeNull();
        for (const key of ["я", "Dead", "Unidentified", ".", "", "ſ", "K", "aa", "a\n"]) {
          expect(rollShortcutFor(event(key, `Key${letter.toUpperCase()}`, mod), platform)).toBe(action);
        }
        for (const code of [undefined, "keyA", "Keya", "KeyAA", "Key", "XKeyA", "KeyAextra", `Key${letter.toUpperCase()}\n`]) {
          expect(rollShortcutFor(event("Dead", code, mod), platform)).toBeNull();
        }
        for (const rest of [{ repeat: true }, { shiftKey: true }, { altKey: true }, { ctrlKey: true, metaKey: true }, { target: { tagName: "INPUT" } }, { target: { isContentEditable: true } }, { target: { closest: () => ({}) } }]) {
          expect(rollShortcutFor(event(letter, code, { ...mod, ...rest }), platform)).toBeNull();
        }
      });
    }
    it(`${platform} preserves physical quantize and delete`, () => {
      expect(rollShortcutFor(event("œ", "KeyQ", { ctrlKey: false, altKey: true }), platform)).toBe("quantize");
      expect(rollShortcutFor(event("q", "KeyA", { ctrlKey: false, altKey: true }), platform)).toBeNull();
      for (const code of ["Delete", "Backspace"]) expect(rollShortcutFor(event("", code, { ctrlKey: false }), platform)).toBe("remove");
      expect(rollShortcutFor(event("a", "KeyA", { ctrlKey: false }), platform)).toBeNull();
      expect(rollShortcutFor(event("a", "KeyA", platform === "MacIntel" ? { ctrlKey: true } : { ctrlKey: false, metaKey: true }), platform)).toBeNull();
    });
  }
});
