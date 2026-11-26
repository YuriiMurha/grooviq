import { useId, type ButtonHTMLAttributes } from "react";

interface UnsupportedButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  reason?: string;
}

/** An unsupported control stays reachable so its explanation is available by keyboard. */
export function UnsupportedButton({ reason, disabled, onClick, ...button }: UnsupportedButtonProps) {
  const descriptionId = useId();
  if (reason === undefined) return <button {...button} disabled={disabled} onClick={onClick} />;

  return (
    <span className="mx-unsupported">
      <button {...button} title={undefined} aria-disabled="true" aria-describedby={descriptionId} />
      <span className="mx-tooltip" id={descriptionId} role="tooltip">{reason}</span>
    </span>
  );
}
