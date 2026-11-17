interface Props {
  recording: boolean;
  onToggle: () => void;
}

/** Record: what is played while the pattern loops is added to it. */
export function RecordButton({ recording, onToggle }: Props) {
  return (
    <button
      type="button"
      className="transport-btn record"
      aria-label="Record"
      aria-pressed={recording}
      data-hint="Record what you play into the pattern while it loops (R)."
      onClick={onToggle}
    >
      <svg viewBox="0 0 16 16" aria-hidden>
        <circle cx="8" cy="8" r="4.5" fill="currentColor" />
      </svg>
    </button>
  );
}
