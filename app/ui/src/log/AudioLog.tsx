import type { LogEntry } from "../audio";

/** Entries the popover shows. */
const SHOWN = 20;

function time(atMs: number): string {
  return new Date(atMs).toLocaleTimeString([], { hour12: false });
}

/** The last 20 audio-log entries, newest first. */
export function AudioLog({ entries }: { entries: LogEntry[] }) {
  const shown = entries.slice(-SHOWN).reverse();
  return (
    <section className="log" aria-label="Audio log">
      <h3>Audio log</h3>
      {shown.length === 0 ? (
        <p className="log-empty">Nothing yet.</p>
      ) : (
        <ol>
          {shown.map((e, i) => (
            <li key={`${e.atMs}-${i}`} className={`log-${e.level}`}>
              <time dateTime={new Date(e.atMs).toISOString()}>{time(e.atMs)}</time>
              <span>{e.message}</span>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}
