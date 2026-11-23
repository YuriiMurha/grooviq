import { useEffect, useRef, type RefObject } from "react";
import type { LiveFrame } from "../live/decode";
import { displayTicks, formatPosition, type Signature } from "./position";

interface Props {
  latest: RefObject<LiveFrame | null>;
  bpm: number;
  signature: Signature;
  /** The output's rate, for the latency shift. */
  sampleRate: number;
  playMode: "pattern" | "song" | null;
}

const IDLE = "1.1.01";

/** "bar.beat.step" from the live stream. It reads the latest frame once per animation frame and
 * writes the text itself, so playing never re-renders React. */
export function PositionReadout({ latest, bpm, signature, sampleRate, playMode }: Props) {
  const text = useRef<HTMLSpanElement>(null);
  // What the animation loop needs, kept current without restarting it.
  const params = useRef({ bpm, signature, sampleRate });

  useEffect(() => {
    params.current = { bpm, signature, sampleRate };
  }, [bpm, signature, sampleRate]);

  useEffect(() => {
    let raf = 0;
    let shown = IDLE;
    const draw = () => {
      const frame = latest.current;
      const p = params.current;
      const next = frame
        ? formatPosition(
            displayTicks(frame.positionTicks, frame.outputLatencyFrames, p.bpm, p.sampleRate),
            p.signature,
          )
        : IDLE;
      if (next !== shown && text.current) {
        text.current.textContent = next;
        shown = next;
      }
      raf = requestAnimationFrame(draw);
    };
    raf = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(raf);
  }, [latest]);

  return (
    <div className="position" data-hint={playMode ? `Where the ${playMode} is now: bar, beat and step (a sixteenth).` : "Current playback position: bar, beat and step (a sixteenth)."}>
      <span ref={text} className="position-value" role="timer" aria-label="Position">
        {IDLE}
      </span>
      <span className="field-caption">Bar, beat, step</span>
    </div>
  );
}
