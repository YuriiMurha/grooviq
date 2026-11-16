import { useEffect, useRef, type RefObject } from "react";
import type { LiveFrame } from "../live/decode";

/** The meter's floor: quieter reads as empty. */
const FLOOR_DB = -60;

/** A linear peak as a 0..1 bar length on a dB scale. */
function barLength(peak: number): number {
  if (!(peak > 0)) return 0;
  const db = 20 * Math.log10(peak);
  return Math.min(1, Math.max(0, (db - FLOOR_DB) / -FLOOR_DB));
}

/** The two-bar master meter. It reads the latest live frame once per animation frame and moves
 * the bars with CSS transforms, without re-rendering. */
export function Meter({ latest }: { latest: RefObject<LiveFrame | null> }) {
  const left = useRef<HTMLDivElement>(null);
  const right = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let raf = 0;
    const draw = () => {
      const peak = latest.current?.peak ?? [0, 0];
      for (const [bar, value] of [
        [left.current, peak[0]],
        [right.current, peak[1]],
      ] as const) {
        if (bar) {
          bar.style.transform = `scaleX(${barLength(value)})`;
          bar.classList.toggle("clip", value >= 1);
        }
      }
      raf = requestAnimationFrame(draw);
    };
    raf = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(raf);
  }, [latest]);

  return (
    <div className="meter" role="img" aria-label="Master level meter">
      <div className="meter-track">
        <div ref={left} className="meter-bar" />
      </div>
      <div className="meter-track">
        <div ref={right} className="meter-bar" />
      </div>
    </div>
  );
}
