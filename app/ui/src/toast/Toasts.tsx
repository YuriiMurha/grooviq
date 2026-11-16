export interface Toast {
  id: number;
  message: string;
  warn: boolean;
}

/** Messages from `audio-event`, newest last. Warnings use the warning style. */
export function Toasts({ toasts, onDismiss }: { toasts: Toast[]; onDismiss: (id: number) => void }) {
  return (
    <div className="toasts" role="status" aria-live="polite">
      {toasts.map((t) => (
        <div key={t.id} className={`toast${t.warn ? " warn" : ""}`}>
          <span>{t.message}</span>
          <button type="button" aria-label="Dismiss" onClick={() => onDismiss(t.id)}>
            ×
          </button>
        </div>
      ))}
    </div>
  );
}
