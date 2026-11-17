import { describe, expect, it } from "vitest";
import { isTypingTarget, shortcutFor, type ShortcutEvent } from "./shortcuts";

const NOT_TYPING = { tagName: "DIV", isContentEditable: false, closest: () => null };
const INPUT = { tagName: "INPUT", isContentEditable: false, closest: () => null };

function key(key: string, mods: Partial<ShortcutEvent> = {}): ShortcutEvent {
  return {
    key,
    metaKey: false,
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    repeat: false,
    target: NOT_TYPING,
    ...mods,
  };
}

describe("shortcutFor", () => {
  it("mod_is_cmd_on_mac_and_ctrl_elsewhere", () => {
    expect(shortcutFor(key("s", { metaKey: true }), "MacIntel")).toBe("save");
    expect(shortcutFor(key("s", { ctrlKey: true }), "MacIntel")).toBeNull();
    expect(shortcutFor(key("s", { ctrlKey: true }), "Win32")).toBe("save");
    expect(shortcutFor(key("s", { metaKey: true }), "Win32")).toBeNull();
    expect(shortcutFor(key("s", { ctrlKey: true }), "Linux x86_64")).toBe("save");

    expect(shortcutFor(key("S", { metaKey: true, shiftKey: true }), "MacIntel")).toBe("saveAs");
    expect(shortcutFor(key("o", { ctrlKey: true }), "Win32")).toBe("open");
    expect(shortcutFor(key("n", { ctrlKey: true }), "Win32")).toBe("new");
    // Both at once is some other combination, never Save.
    expect(shortcutFor(key("s", { metaKey: true, ctrlKey: true }), "MacIntel")).toBeNull();
    expect(shortcutFor(key("s", { metaKey: true, ctrlKey: true }), "Win32")).toBeNull();
    // Alt turns it into some other combination.
    expect(shortcutFor(key("s", { ctrlKey: true, altKey: true }), "Win32")).toBeNull();
    // Shift is part of Save as only.
    expect(shortcutFor(key("o", { ctrlKey: true, shiftKey: true }), "Win32")).toBeNull();
  });

  it("space_toggles_play_unless_typing", () => {
    expect(shortcutFor(key(" "), "MacIntel")).toBe("playStop");
    expect(shortcutFor(key(" ", { target: null }), "Win32")).toBe("playStop");
    expect(shortcutFor(key(" ", { target: INPUT }), "Win32")).toBeNull();
    expect(
      shortcutFor(key(" ", { target: { ...NOT_TYPING, tagName: "TEXTAREA" } }), "Win32"),
    ).toBeNull();
    // Only text entry is typing: a select, a slider or a checkbox keeps Space as play/stop.
    expect(
      shortcutFor(key(" ", { target: { ...NOT_TYPING, tagName: "SELECT" } }), "Win32"),
    ).toBe("playStop");
    for (const type of ["range", "checkbox", "button"]) {
      expect(
        shortcutFor(key(" ", { target: { ...NOT_TYPING, tagName: "INPUT", type } }), "Win32"),
      ).toBe("playStop");
    }
    for (const type of ["text", "number", "search", "", "TEXT"]) {
      expect(
        shortcutFor(key(" ", { target: { ...NOT_TYPING, tagName: "INPUT", type } }), "Win32"),
      ).toBeNull();
    }
    expect(
      shortcutFor(key(" ", { target: { ...NOT_TYPING, isContentEditable: true } }), "Win32"),
    ).toBeNull();
    // A button inside a menu or dialog needs Space to press itself.
    expect(
      shortcutFor(key(" ", { target: { ...NOT_TYPING, closest: () => ({}) } }), "Win32"),
    ).toBeNull();
    // A device window is not a blocking dialog: Space plays from inside it, the typing keyboard
    // plays its channel there, and a text input inside it still takes its keys.
    const deviceWindow = { hasAttribute: (a: string) => a === "data-device-window" };
    const inDevice = { ...NOT_TYPING, closest: () => deviceWindow };
    expect(shortcutFor(key(" ", { target: inDevice }), "Win32")).toBe("playStop");
    expect(isTypingTarget(inDevice)).toBe(false);
    expect(isTypingTarget({ ...INPUT, type: "text", closest: () => deviceWindow })).toBe(true);
    expect(isTypingTarget({ ...NOT_TYPING, closest: () => ({ hasAttribute: () => false }) })).toBe(true);
    // Holding it down must not flip play and stop over and over.
    expect(shortcutFor(key(" ", { repeat: true }), "Win32")).toBeNull();
    // With a modifier it is not play/stop.
    expect(shortcutFor(key(" ", { ctrlKey: true }), "Win32")).toBeNull();
    expect(shortcutFor(key(" ", { shiftKey: true }), "Win32")).toBeNull();
  });

  it("mod_r_exports", () => {
    expect(shortcutFor(key("r", { metaKey: true }), "MacIntel")).toBe("export");
    expect(shortcutFor(key("r", { ctrlKey: true }), "Win32")).toBe("export");
    // R alone is record while the typing keyboard is off, and a note while it is on.
    expect(shortcutFor(key("r"), "Win32", true)).toBeNull();
  });

  it("mod shortcuts also work while typing (they are not text)", () => {
    expect(shortcutFor(key("s", { ctrlKey: true, target: INPUT }), "Win32")).toBe("save");
  });

  it("ignores everything else", () => {
    expect(shortcutFor(key("a"), "Win32")).toBeNull();
    expect(shortcutFor(key("Enter"), "Win32")).toBeNull();
  });
});

describe("undo", () => {
  it("mod_z_and_mod_shift_z_map_to_undo_and_redo_and_ctrl_y_off_mac", () => {
    expect(shortcutFor(key("z", { metaKey: true }), "MacIntel")).toBe("undo");
    expect(shortcutFor(key("Z", { metaKey: true, shiftKey: true }), "MacIntel")).toBe("redo");
    expect(shortcutFor(key("z", { ctrlKey: true }), "Win32")).toBe("undo");
    expect(shortcutFor(key("Z", { ctrlKey: true, shiftKey: true }), "Linux x86_64")).toBe("redo");
    expect(shortcutFor(key("y", { ctrlKey: true }), "Win32")).toBe("redo");
    expect(shortcutFor(key("y", { ctrlKey: true }), "Linux x86_64")).toBe("redo");
    // Ctrl+Y is not Mod on macOS, and Cmd+Y is nothing there.
    expect(shortcutFor(key("y", { ctrlKey: true }), "MacIntel")).toBeNull();
    expect(shortcutFor(key("y", { metaKey: true }), "MacIntel")).toBeNull();
    expect(shortcutFor(key("y", { ctrlKey: true, shiftKey: true }), "Win32")).toBeNull();
    // The other modifier, or Alt, makes it some other combination.
    expect(shortcutFor(key("z", { ctrlKey: true }), "MacIntel")).toBeNull();
    expect(shortcutFor(key("z", { metaKey: true }), "Win32")).toBeNull();
    expect(shortcutFor(key("z", { metaKey: true, altKey: true }), "MacIntel")).toBeNull();
    // A text field undoes its own typing.
    expect(shortcutFor(key("z", { metaKey: true, target: INPUT }), "MacIntel")).toBeNull();
    expect(shortcutFor(key("Z", { metaKey: true, shiftKey: true, target: INPUT }), "MacIntel")).toBeNull();
    expect(shortcutFor(key("y", { ctrlKey: true, target: INPUT }), "Win32")).toBeNull();
    // Z alone is a note of the typing keyboard, never undo.
    expect(shortcutFor(key("z"), "MacIntel")).toBeNull();
  });
});

describe("record", () => {
  it("r_is_record_only_while_the_typing_keyboard_is_off", () => {
    expect(shortcutFor(key("r"), "MacIntel")).toBe("record");
    expect(shortcutFor(key("R"), "Win32", false)).toBe("record");
    expect(shortcutFor(key("r"), "MacIntel", true)).toBeNull();
    expect(shortcutFor(key("r", { repeat: true }), "MacIntel")).toBeNull();
    expect(shortcutFor(key("r", { target: INPUT }), "MacIntel")).toBeNull();
    expect(shortcutFor(key("r", { shiftKey: true }), "MacIntel")).toBeNull();
    // Mod+R stays Export.
    expect(shortcutFor(key("r", { metaKey: true }), "MacIntel")).toBe("export");
  });
});


describe("non-Latin Mod shortcuts", () => {
  for (const platform of ["MacIntel", "Win32", "Linux x86_64"]) {
    const mod = platform === "MacIntel" ? { metaKey: true } : { ctrlKey: true };
    for (const letter of ["і", "σ"]) {
      for (const [code, action] of [["KeyS", "save"], ["KeyN", "new"], ["KeyO", "open"], ["KeyR", "export"], ["KeyZ", "undo"]] as const) {
        it(`${platform} ${letter}/${code} ${action}`, () => {
          expect(shortcutFor(key(letter, { ...mod, code }), platform)).toBe(action);
        });
      }
      it(`${platform} ${letter} shifted and redo`, () => {
        expect(shortcutFor(key(letter, { ...mod, code: "KeyS", shiftKey: true }), platform)).toBe("saveAs");
        expect(shortcutFor(key(letter, { ...mod, code: "KeyZ", shiftKey: true }), platform)).toBe("redo");
        expect(shortcutFor(key(letter, { ...mod, code: "KeyY" }), platform)).toBe(platform === "MacIntel" ? null : "redo");
        expect(shortcutFor(key(letter, { ...mod, code: "KeyY", shiftKey: true }), platform)).toBeNull();
        expect(shortcutFor(key(letter, { ...mod, code: "KeyO", shiftKey: true }), platform)).toBeNull();
      });
    }
    it(`${platform} preserves ASCII letters and rejects invalid codes and modifiers`, () => {
      for (const [letter, code, action] of [["z", "KeyY", "undo"], ["S", "KeyZ", "save"], ["q", "KeyS", null]] as const) {
        expect(shortcutFor(key(letter, { ...mod, code }), platform)).toBe(action);
      }
      for (const code of [undefined, "", "Keys", "KeySS", "Key1", "DigitS", "KeyＳ"]) {
        expect(shortcutFor(key("і", { ...mod, code }), platform)).toBeNull();
      }
      expect(shortcutFor(key("і", { ...mod, code: "KeyS", altKey: true }), platform)).toBeNull();
      expect(shortcutFor(key("і", { code: "KeyS", metaKey: true, ctrlKey: true }), platform)).toBeNull();
      const wrongMod = platform === "MacIntel" ? { ctrlKey: true } : { metaKey: true };
      expect(shortcutFor(key("і", { ...wrongMod, code: "KeyS" }), platform)).toBeNull();
      for (const code of ["KeyZ", "KeyY"]) {
        expect(shortcutFor(key("я", { ...mod, code, target: INPUT }), platform)).toBeNull();
      }
      expect(shortcutFor(key("і", { ...mod, code: "KeyS", target: INPUT }), platform)).toBe("save");
      expect(shortcutFor(key("і", { code: "KeyR" }), platform)).toBeNull();
      expect(shortcutFor(key("і", { code: "Space" }), platform)).toBeNull();
    });
  }
});
