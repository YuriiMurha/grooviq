import type { Output } from "../audio";
import { kindWarns } from "./kind";
import { OutputIcon } from "./OutputIcon";

interface Props {
  output: Output | null;
  latencyMs: number | null;
  open: boolean;
  onToggle: () => void;
}

/** The toolbar chip: what plays now, and how late. Speakers get the warning style. */
export function OutputChip({ output, latencyMs, open, onToggle }: Props) {
  const warn = output !== null && kindWarns(output.kind);
  return (
    <button
      type="button"
      className={`chip${warn ? " warn" : ""}`}
      aria-haspopup="dialog"
      aria-expanded={open}
      onClick={onToggle}
      title={warn ? "Playing on speakers: the room can hear it" : undefined}
    >
      <span className="chip-icon" aria-hidden>
        <OutputIcon kind={output?.kind ?? "none"} />
      </span>
      <span className="chip-name">{output?.name ?? "No output"}</span>
      {output && latencyMs !== null && <span className="chip-latency">{latencyMs} ms</span>}
    </button>
  );
}
