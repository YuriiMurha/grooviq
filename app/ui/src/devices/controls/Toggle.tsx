export interface ToggleProps {
  /** The parameter id, for `data-param`. */
  id: string;
  /** The label, also its accessible name. */
  label: string;
  /** What it does (one sentence). */
  does: string;
  /** On: the engine's switches are on at 0.5 and up. */
  on: boolean;
  onChange: (on: boolean) => void;
}

/** An on/off parameter: a pill that lights up while it is on. A click or Enter flips it (Space
 * plays and stops in a device window). */
export function Toggle({ id, label, does, on, onChange }: ToggleProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      className={`dtoggle${on ? " on" : ""}`}
      data-param={id}
      data-hint={`${does} Click to switch it ${on ? "off" : "on"}.`}
      onClick={() => onChange(!on)}
    >
      <span className="dtoggle-led" aria-hidden="true" />
      {label}
    </button>
  );
}
