import { useCallback, useEffect, useRef, useState, type PointerEvent, type RefObject } from "react";
import { channelNoteOff, channelNoteOn, TYPING_VELOCITY } from "../api/notes";
import { errorText } from "../api/project";
import { Press } from "../keyboard/press";

const BLACK = new Set([1, 3, 6, 8, 10]);
const KEYS = Array.from({ length: 128 }, (_, i) => 127 - i);

interface Props {
  channel: number | null;
  /** The key row under a y offset (CSS pixels from the column's top). */
  keyAt: (y: number) => number;
  /** The inner strip the roll's draw loop scrolls with the notes. */
  strip: RefObject<HTMLDivElement | null>;
  onError: (message: string) => void;
  /** Changes on every transport stop and project replacement: a held key lets go. */
  releaseKeys: number;
}

/** The piano keys beside the roll: press one to hear it on the channel, slide to play others.
 * The key sounds until the pointer lets go, the window loses focus or hides, the transport
 * stops, the project is replaced or the channel changes. */
export function KeyColumn({ channel, keyAt, strip, onError, releaseKeys }: Props) {
  const [held, setHeld] = useState<number | null>(null);
  const sounding = useRef<{ channel: number; key: number; press: Press } | null>(null);
  const report = useRef(onError);
  useEffect(() => {
    report.current = onError;
  }, [onError]);

  const release = useCallback(() => {
    const s = sounding.current;
    if (!s) return;
    sounding.current = null;
    setHeld(null);
    // Best effort: the channel may just have been removed.
    void s.press.release((id) => channelNoteOff(s.channel, s.key, id));
  }, []);
  const play = (key: number) => {
    if (channel === null || key < 0 || key > 127 || sounding.current?.key === key) return;
    release();
    const press = new Press(channelNoteOn(channel, key, TYPING_VELOCITY), (e: unknown) =>
      report.current(errorText(e)),
    );
    sounding.current = { channel, key, press };
    setHeld(key);
  };
  const keyOf = (e: PointerEvent<HTMLDivElement>) => keyAt(e.clientY - e.currentTarget.getBoundingClientRect().top);

  // A new channel, or leaving: let the old one go.
  useEffect(() => release, [channel, release]);
  // A stop or a replaced project (not the first render).
  const lastRelease = useRef(releaseKeys);
  useEffect(() => {
    if (lastRelease.current === releaseKeys) return;
    lastRelease.current = releaseKeys;
    release();
  }, [releaseKeys, release]);
  // The pointer-up may never come here once the window loses focus or hides.
  useEffect(() => {
    const onHidden = () => {
      if (document.visibilityState === "hidden") release();
    };
    window.addEventListener("blur", release);
    document.addEventListener("visibilitychange", onHidden);
    return () => {
      window.removeEventListener("blur", release);
      document.removeEventListener("visibilitychange", onHidden);
    };
  }, [release]);

  return (
    <div
      className="roll-keys"
      role="group"
      aria-label="Piano keys"
      data-hint="Press a key to hear it; slide to play the keys around it."
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        try {
          e.currentTarget.setPointerCapture(e.pointerId);
        } catch {
          /* no pointer capture here */
        }
        play(keyOf(e));
      }}
      onPointerMove={(e) => {
        if (sounding.current) play(keyOf(e));
      }}
      onPointerUp={release}
      onPointerCancel={release}
      onLostPointerCapture={release}
    >
      <div className="roll-keys-strip" ref={strip}>
        {KEYS.map((k) => (
          <div key={k} className={`roll-key${BLACK.has(k % 12) ? " black" : ""}${held === k ? " held" : ""}`}>
            {k % 12 === 0 ? `C${k / 12 - 1}` : null}
          </div>
        ))}
      </div>
    </div>
  );
}
