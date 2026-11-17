import { useCallback, useEffect, useRef } from "react";
import { channelNoteOff, channelNoteOn, TYPING_VELOCITY } from "../api/notes";
import { errorText } from "../api/project";
import { modalIsOpen } from "../project/shortcuts";
import { Press } from "./press";
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
  /** The press of each sounding note, by "channel:key" (one at a time per note: HeldKeys). */
  const presses = useRef(new Map<string, Press>());
  const base = useRef(DEFAULT_BASE);
  const report = useRef(onError);
  useEffect(() => {
    report.current = onError;
  }, [onError]);

  // A failed note-on is reported. A note-off names its own press and is best effort: its
  // channel may just have been removed, and the engine drops a removed channel's live notes.
  const send = useCallback((n: LiveNote, down: boolean) => {
    const at = `${n.channel}:${n.key}`;
    if (down) {
      const on = channelNoteOn(n.channel, n.key, TYPING_VELOCITY);
      presses.current.set(at, new Press(on, (e: unknown) => report.current(errorText(e))));
      return;
    }
    const press = presses.current.get(at);
    presses.current.delete(at);
    void press?.release((id) => channelNoteOff(n.channel, n.key, id));
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
