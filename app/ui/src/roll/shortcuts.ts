import { isMac, isTypingTarget, printedShortcutLetter, type ShortcutEvent } from "../project/shortcuts";

export type RollAction = "selectAll" | "copy" | "paste" | "duplicate" | "remove" | "quantize";

export function rollShortcutFor(event: ShortcutEvent, platform: string): RollAction | null {
  if (event.repeat || isTypingTarget(event.target) || event.shiftKey) return null;
  const mod = isMac(platform) ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;
  if (mod && !event.altKey) {
    return ({ a: "selectAll", c: "copy", v: "paste", b: "duplicate" } as Record<string, RollAction>)[printedShortcutLetter(event)] ?? null;
  }
  if (event.metaKey || event.ctrlKey) return null;
  if (event.altKey) return event.code === "KeyQ" ? "quantize" : null;
  return event.code === "Delete" || event.code === "Backspace" ? "remove" : null;
}
