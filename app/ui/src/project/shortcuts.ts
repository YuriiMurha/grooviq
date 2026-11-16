export type Action = "playStop" | "new" | "open" | "save" | "saveAs" | "export";

/** The part of a `KeyboardEvent` the shortcuts read (so tests need no DOM). */
export interface ShortcutEvent {
  key: string;
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
 * itself. */
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
  return Boolean(
    el.closest?.("[role='menu'], [role='dialog'], [role='alertdialog'], [role='listbox']"),
  );
}

const MOD_KEYS: Record<string, Action> = { n: "new", o: "open", s: "save", r: "export" };

/** The action a key press asks for, or `null`. Mod is Cmd on macOS and Ctrl elsewhere; the
 * other of the two never counts as Mod. Space (alone, not held down) is play/stop unless the
 * person is typing. */
export function shortcutFor(event: ShortcutEvent, platform: string): Action | null {
  if (event.altKey) return null;
  const mac = isMac(platform);
  const mod = mac ? event.metaKey : event.ctrlKey;
  const wrongMod = mac ? event.ctrlKey : event.metaKey;
  if (wrongMod) return null;
  const key = event.key.toLowerCase();

  if (mod) {
    if (key === "s" && event.shiftKey) return "saveAs";
    if (event.shiftKey) return null;
    return MOD_KEYS[key] ?? null;
  }
  if (key === " " && !event.shiftKey && !event.repeat && !isTypingTarget(event.target)) {
    return "playStop";
  }
  return null;
}

/** How a shortcut is written in the menu: "⌘N" on macOS, "Ctrl+N" elsewhere. */
export function shortcutLabel(action: Exclude<Action, "playStop">, platform: string): string {
  const letter = { new: "N", open: "O", save: "S", saveAs: "S", export: "R" }[action];
  const shift = action === "saveAs";
  return isMac(platform) ? `${shift ? "⇧" : ""}⌘${letter}` : `Ctrl+${shift ? "Shift+" : ""}${letter}`;
}
