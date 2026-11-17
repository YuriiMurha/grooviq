import { useCallback, useEffect, useRef } from "react";
import { channelNote, TYPING_VELOCITY } from "../api/notes";
import { errorText } from "../api/project";
import { modalIsOpen } from "../project/shortcuts";
import { DEFAULT_BASE, HeldKeys, accepts, keyFor, shiftOctave, type LiveNote } from "./typingKeyboard";

/** The transport as the typing keyboard needs it: every change of `release` (each stop, a
 * replaced project) and the end of a recording release every held key. */
export interface TransportEdges {
  release: number;
  recording: boolean;
}

/** Plays `channel` from the computer keyboard while `on`. Every held key is released when the
 * window loses focus or is hidden, when it is switched off, when the channel changes (on the old
 * channel), when the transport stops (`release` changes) or recording ends, and on unmount. */
export function useTypingKeyboard(
  on: boolean,
  channel: number | null,
  onError: (message: string) => void,
  { release, recording }: TransportEdges,
) {
  const held = useRef(new HeldKeys());
  const base = useRef(DEFAULT_BASE);
  const report = useRef(onError);
  useEffect(() => {
    report.current = onError;
  }, [onError]);

  // A failed note-on is reported. A note-off is best effort: its channel may just have been
  // removed, and the engine drops a removed channel's live notes itself.
  const send = useCallback((n: LiveNote, down: boolean) => {
    channelNote(n.channel, n.key, down ? TYPING_VELOCITY : 0, down).catch((e: unknown) => {
      if (down) report.current(errorText(e));
    });
  }, []);
  const releaseAll = useCallback(() => {
    for (const n of held.current.releaseAll()) send(n, false);
  }, [send]);

  // The transport stopped or recording ended: the notes held across the edge end there.
  const was = useRef({ release, recording });
  useEffect(() => {
    const before = was.current;
    was.current = { release, recording };
    if (before.release !== release || (before.recording && !recording)) releaseAll();
  }, [release, recording, releaseAll]);

  // Releases whatever sounds when `on` or `channel` change and on unmount.
  useEffect(() => {
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
      if (note) send(note, true);
    };
    const onKeyUp = (e: KeyboardEvent) => {
      const note = held.current.release(e.code);
      if (note) send(note, false);
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
  }, [on, channel, send, releaseAll]);
}
