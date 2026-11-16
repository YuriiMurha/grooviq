import { Modal } from "./Modal";

export interface ConfirmAction {
  label: string;
  /** The hint-bar sentence. */
  hint: string;
  onSelect: () => void;
  /** The suggested answer: it has focus first. */
  primary?: boolean;
}

interface Props {
  title: string;
  message: string;
  actions: ConfirmAction[];
  onDismiss?: () => void;
}

/** A question with a row of answers ("Save changes?", the crash recovery offer). */
export function ConfirmDialog({ title, message, actions, onDismiss }: Props) {
  return (
    <Modal title={title} onDismiss={onDismiss}>
      <p className="modal-message">{message}</p>
      <div className="modal-actions">
        {actions.map((a) => (
          <button
            key={a.label}
            type="button"
            className={a.primary ? "btn primary" : "btn"}
            data-autofocus={a.primary ? "" : undefined}
            data-hint={a.hint}
            onClick={a.onSelect}
          >
            {a.label}
          </button>
        ))}
      </div>
    </Modal>
  );
}
