interface Props {
  on: boolean;
  onToggle: () => void;
}

/** Switches the typing keyboard (the computer keyboard as a piano) on and off. */
export function KeyboardToggle({ on, onToggle }: Props) {
  return (
    <button
      type="button"
      className="toggle-btn"
      aria-label="Typing keyboard"
      aria-pressed={on}
      data-hint="Play the selected channel from your computer keyboard: Z to / and Q to P, with - and = for the octave."
      onClick={onToggle}
    >
      <svg viewBox="0 0 16 16" aria-hidden>
        <rect x="1.5" y="4" width="13" height="8.5" rx="1.5" fill="none" stroke="currentColor" strokeWidth="1.3" />
        <path d="M4 7h1M7 7h1M10 7h1M5 10h6" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" />
      </svg>
    </button>
  );
}
