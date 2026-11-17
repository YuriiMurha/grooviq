interface Props {
  on: boolean;
  onToggle: () => void;
}

/** The metronome: clicks while the transport plays, never in an export. */
export function MetronomeButton({ on, onToggle }: Props) {
  return (
    <button
      type="button"
      className="toggle-btn"
      aria-label="Metronome"
      aria-pressed={on}
      data-hint="Clicks on every beat while the pattern plays. It is never in an export."
      onClick={onToggle}
    >
      <svg viewBox="0 0 16 16" aria-hidden>
        <path d="M6 1.5h4l3 13H3z M8 11.5 11.5 4" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinejoin="round" />
      </svg>
    </button>
  );
}
