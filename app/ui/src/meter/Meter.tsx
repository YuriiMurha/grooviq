import { useEffect, useRef } from "react";
import type { MeterHistory } from "./history";

/** The meter's floor: quieter reads as empty. */
const FLOOR_DB = -60;

/** A linear peak as a 0..1 bar length on a dB scale. */
function barLength(peak: number): number {
  if (!(peak > 0)) return 0;
  const db = 20 * Math.log10(peak);
  return Math.min(1, Math.max(0, (db - FLOOR_DB) / -FLOOR_DB));
}

/** The two-bar master meter. It reads delayed master peaks once per animation frame and moves
 * the bars with CSS transforms, without re-rendering. */
export function Meter({ history }: { history: MeterHistory }) {
  const left = useRef<HTMLDivElement>(null);
  const right = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let raf = 0;
    const draw = () => {
      const peak = history.read(performance.now());
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
  }, [history]);

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
