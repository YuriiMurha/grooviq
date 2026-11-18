export type Action = "playStop" | "record" | "undo" | "redo" | "new" | "open" | "save" | "saveAs" | "export";
/** The project actions: the menu's items and what `perform` takes. */
export type MenuAction = Exclude<Action, "playStop" | "record" | "undo" | "redo">;

/** The part of a `KeyboardEvent` the shortcuts read (so tests need no DOM). */
export interface ShortcutEvent {
  key: string;
  code?: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  repeat: boolean;
  target:
    | EventTarget
    | { tagName?: string; type?: string; isContentEditable?: boolean; closest?: unknown }
    | null;
}

/** True on macOS, where Mod is Cmd. */
export function isMac(platform: string): boolean {
  return /^mac/i.test(platform) || /iphone|ipad/i.test(platform);
}

/** Inputs whose type takes typed text (an input with no type is text too). */
const TEXT_INPUTS = new Set(["", "text", "number", "search", "email", "url", "tel", "password"]);

/** Text goes here, so Space is a space, not play. Only text entry counts: a slider, a select or
 * a button keeps Space as play/stop. A button inside a menu or dialog needs Space to press
 * itself, except in a device window (`data-device-window`), where Space plays and stops and the
 * typing keyboard plays that window's channel. */
export function isTypingTarget(target: ShortcutEvent["target"]): boolean {
  const el = target as {
    tagName?: string;
    type?: string;
    isContentEditable?: boolean;
    closest?: (selector: string) => unknown;
  } | null;
  if (!el) return false;
  const tag = (el.tagName ?? "").toUpperCase();
  if (tag === "INPUT" && TEXT_INPUTS.has((el.type ?? "").toLowerCase())) return true;
  if (tag === "TEXTAREA") return true;
  if (el.isContentEditable) return true;
  const box = el.closest?.("[role='menu'], [role='dialog'], [role='alertdialog'], [role='listbox']") as
    | { hasAttribute?: (name: string) => boolean }
    | null
    | undefined;
  // A device window is a floating editor, not a dialog to answer: the keys go on playing there.
  return Boolean(box) && !box?.hasAttribute?.("data-device-window");
}

const MOD_KEYS: Record<string, Action> = { n: "new", o: "open", s: "save", r: "export" };

/** The action a key press asks for, or `null`. Mod is Cmd on macOS and Ctrl elsewhere; the
 * other of the two never counts as Mod. Space (alone, not held down) is play/stop unless the
 * person is typing. R alone is record, but only while the typing keyboard is off (it plays a
 * note then). Mod+Z undoes a note change and Mod+Shift+Z (Ctrl+Y off macOS) redoes it, except
 * in a text field, which undoes its own typing. */
export function shortcutFor(event: ShortcutEvent, platform: string, typingKeyboard = false): Action | null {
  if (event.altKey) return null;
  const mac = isMac(platform);
  const mod = mac ? event.metaKey : event.ctrlKey;
  const wrongMod = mac ? event.ctrlKey : event.metaKey;
  if (wrongMod) return null;
  const key = event.key.toLowerCase();

  if (mod) {
    // Keep printed ASCII letters on QWERTZ/AZERTY; physical positions only rescue
    // layouts whose delivered key is not an ASCII letter.
    const codeLetter = /^Key([A-Z])$/.exec(event.code ?? "")?.[1]?.toLowerCase();
    const modKey = /^[a-z]$/.test(key) ? key : (codeLetter ?? key);
    const redoY = !mac && modKey === "y" && !event.shiftKey;
    if (modKey === "z" || redoY) {
      if (isTypingTarget(event.target)) return null;
      return event.shiftKey || redoY ? "redo" : "undo";
    }
    if (modKey === "s" && event.shiftKey) return "saveAs";
    if (event.shiftKey) return null;
    return MOD_KEYS[modKey] ?? null;
  }
  if (event.shiftKey || event.repeat || isTypingTarget(event.target)) return null;
  if (key === " ") return "playStop";
  if (key === "r" && !typingKeyboard) return "record";
  return null;
}

/** How a shortcut is written in the menu: "⌘N" on macOS, "Ctrl+N" elsewhere. */
export function shortcutLabel(action: MenuAction, platform: string): string {
  const letter = { new: "N", open: "O", save: "S", saveAs: "S", export: "R" }[action];
  const shift = action === "saveAs";
  return isMac(platform) ? `${shift ? "⇧" : ""}⌘${letter}` : `Ctrl+${shift ? "Shift+" : ""}${letter}`;
}

/** A modal dialog is open: shortcuts, the typing keyboard and dropped files stand aside. */
export function modalIsOpen(): boolean {
  return typeof document !== "undefined" && document.querySelector("[aria-modal='true']") !== null;
}
