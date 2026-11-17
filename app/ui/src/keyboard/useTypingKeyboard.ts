import { useEffect, useRef } from "react";
import { channelNote, TYPING_VELOCITY } from "../api/notes";
import { errorText } from "../api/project";
import { modalIsOpen } from "../project/shortcuts";
import { DEFAULT_BASE, HeldKeys, accepts, keyFor, shiftOctave, type LiveNote } from "./typingKeyboard";

/** Plays `channel` from the computer keyboard while `on`. Every held key is released when the
 * window loses focus or is hidden, when it is switched off, when the channel changes (on the old
 * channel) and on unmount. */
export function useTypingKeyboard(on: boolean, channel: number | null, onError: (message: string) => void) {
  const held = useRef(new HeldKeys());
  const base = useRef(DEFAULT_BASE);
  const report = useRef(onError);
  useEffect(() => {
    report.current = onError;
  }, [onError]);

  // Releases whatever sounds; runs when `on` or `channel` change and on unmount.
  useEffect(() => {
    // A failed note-on is reported. A note-off is best effort: its channel may just have been
    // removed, and the engine drops a removed channel's live notes itself.
    const send = (n: LiveNote, down: boolean) =>
      channelNote(n.channel, n.key, down ? TYPING_VELOCITY : 0, down).catch((e: unknown) => {
        if (down) report.current(errorText(e));
      });
    const releaseAll = () => {
      for (const n of held.current.releaseAll()) void send(n, false);
    };
    if (!on || channel === null) return releaseAll;

    const onKeyDown = (e: KeyboardEvent) => {
      // macOS sends no keyup for a key let go while Cmd is down: a modifier chord ends every
      // held note rather than leave one ringing.
      if (e.metaKey || e.ctrlKey) {
        releaseAll();
        return;
      }
      if (!accepts(e) || modalIsOpen()) return;
      if (e.code === "Minus" || e.code === "Equal") {
        e.preventDefault();
        base.current = shiftOctave(base.current, e.code);
        return;
      }
      if (keyFor(e.code, base.current) === null) return;
      e.preventDefault();
      const note = held.current.press(e.code, base.current, channel);
      if (note) void send(note, true);
    };
    const onKeyUp = (e: KeyboardEvent) => {
      const note = held.current.release(e.code);
      if (note) void send(note, false);
    };
    const onHidden = () => {
      if (document.visibilityState === "hidden") releaseAll();
    };
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("blur", releaseAll);
    document.addEventListener("visibilitychange", onHidden);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
      window.removeEventListener("blur", releaseAll);
      document.removeEventListener("visibilitychange", onHidden);
      releaseAll();
    };
  }, [on, channel]);
}
