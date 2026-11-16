import { useEffect, useState } from "react";
import { formatAgo, formatClock } from "./ago";

interface Props {
  dirty: boolean;
  /** Not saved to a file yet. */
  untitled: boolean;
  /** Since when the project is clean, in ms. */
  cleanSince: number;
  /** Unix seconds of the last autosave. */
  autosavedAt: number | null;
}

/** "Unsaved changes", "Not saved yet" or "Saved 12 s ago". It ticks on its own so the toolbar
 * does not re-render every second. */
export function SaveState({ dirty, untitled, cleanSince, autosavedAt }: Props) {
  const [now, setNow] = useState(() => Date.now());
  const timed = !dirty && !untitled;

  useEffect(() => {
    if (!timed) return;
    const tick = () => setNow(Date.now());
    tick();
    const id = setInterval(tick, 1000);
    return () => clearInterval(id);
  }, [timed, cleanSince]);

  const text = dirty
    ? "Unsaved changes"
    : untitled
      ? "Not saved yet"
      : `Saved ${formatAgo((now - cleanSince) / 1000)}`;
  return (
    <span
      className={dirty ? "save-state dirty" : "save-state"}
      title={autosavedAt === null ? undefined : `Autosaved at ${formatClock(autosavedAt)}`}
    >
      {text}
    </span>
  );
}
