import { useEffect, useRef, useState, type PointerEvent, type RefObject } from "react";
import { channelNote, TYPING_VELOCITY } from "../api/notes";
import { errorText } from "../api/project";

const BLACK = new Set([1, 3, 6, 8, 10]);
const KEYS = Array.from({ length: 128 }, (_, i) => 127 - i);

interface Props {
  channel: number | null;
  /** The key row under a y offset (CSS pixels from the column's top). */
  keyAt: (y: number) => number;
  /** The inner strip the roll's draw loop scrolls with the notes. */
  strip: RefObject<HTMLDivElement | null>;
  onError: (message: string) => void;
}

/** The piano keys beside the roll: press one to hear it on the channel, slide to play others;
 * the key sounds until the pointer lets go (or the channel changes). */
export function KeyColumn({ channel, keyAt, strip, onError }: Props) {
  const [held, setHeld] = useState<number | null>(null);
  const sounding = useRef<{ channel: number; key: number } | null>(null);
  const report = useRef(onError);
  useEffect(() => {
    report.current = onError;
  }, [onError]);

  const release = () => {
    const s = sounding.current;
    if (!s) return;
    sounding.current = null;
    setHeld(null);
    // Best effort: the channel may just have been removed.
    channelNote(s.channel, s.key, 0, false).catch(() => {});
  };
  const play = (key: number) => {
    if (channel === null || key < 0 || key > 127 || sounding.current?.key === key) return;
    release();
    sounding.current = { channel, key };
    setHeld(key);
    channelNote(channel, key, TYPING_VELOCITY, true).catch((e: unknown) => report.current(errorText(e)));
  };
  const keyOf = (e: PointerEvent<HTMLDivElement>) => keyAt(e.clientY - e.currentTarget.getBoundingClientRect().top);

  // A new channel, or leaving: let the old one go.
  useEffect(() => release, [channel]);

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
