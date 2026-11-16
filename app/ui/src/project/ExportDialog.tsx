import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { useEffect, useRef, useState } from "react";
import {
  errorText,
  onExportProgress,
  projectExport,
  projectExportCancel,
  type ExportFormat,
} from "../api/project";
import { exportMessage } from "./exportText";
import { Modal } from "./Modal";

const RATES = [44100, 48000, 96000] as const;
/** The project default (spec 5.7). */
const DEFAULT_RATE = 48000;
const LOOPS_MAX = 16;

interface Props {
  /** The project's name, for the suggested file name. */
  name: string;
  onClose: () => void;
}

type Phase =
  | { kind: "options"; error: string | null }
  | { kind: "running"; progress: number }
  | { kind: "done"; text: string; warn: boolean };

/** Export WAV: format, rate and loops, then a file prompt, a progress bar and the result. */
export function ExportDialog({ name, onClose }: Props) {
  const [format, setFormat] = useState<ExportFormat>("pcm24");
  const [rate, setRate] = useState<number>(DEFAULT_RATE);
  const [loops, setLoops] = useState("1");
  const [phase, setPhase] = useState<Phase>({ kind: "options", error: null });
  const cancelled = useRef(false);
  const running = phase.kind === "running";

  useEffect(() => {
    const unlisten = onExportProgress((fraction) =>
      setPhase((p) => (p.kind === "running" ? { kind: "running", progress: fraction } : p)),
    );
    return () => {
      unlisten.then((stop) => stop());
    };
  }, []);

  const start = async () => {
    const path = await saveDialog({
      title: "Export WAV",
      defaultPath: `${name}.wav`,
      filters: [{ name: "WAV audio", extensions: ["wav"] }],
    }).catch(() => null);
    if (!path) return;
    const count = Math.min(LOOPS_MAX, Math.max(1, Math.round(Number(loops)) || 1));
    cancelled.current = false;
    setPhase({ kind: "running", progress: 0 });
    try {
      const result = await projectExport(path, { sampleRate: rate, format, loops: count });
      setPhase({ kind: "done", ...exportMessage(result) });
    } catch (e) {
      // Cancelling is the person's own doing: no error, back to the options.
      setPhase({ kind: "options", error: cancelled.current ? null : errorText(e) });
    }
  };

  const cancel = () => {
    cancelled.current = true;
    projectExportCancel().catch(() => {});
  };

  return (
    <Modal title="Export WAV" onDismiss={running ? undefined : onClose}>
      {phase.kind === "running" ? (
        <>
          <div
            className="progress"
            role="progressbar"
            aria-label="Export progress"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={Math.round(phase.progress * 100)}
          >
            <div className="progress-fill" style={{ transform: `scaleX(${phase.progress})` }} />
          </div>
          <p className="modal-message">Exporting… {Math.round(phase.progress * 100)}%</p>
          <div className="modal-actions">
            <button type="button" className="btn" data-autofocus data-hint="Stops the export and throws the file away." onClick={cancel}>
              Cancel export
            </button>
          </div>
        </>
      ) : phase.kind === "done" ? (
        <>
          <p
            className={phase.warn ? "modal-message warn" : "modal-message"}
            role={phase.warn ? "alert" : "status"}
          >
            {phase.text}
          </p>
          <div className="modal-actions">
            <button type="button" className="btn primary" data-autofocus data-hint="Closes this window." onClick={onClose}>
              Done
            </button>
          </div>
        </>
      ) : (
        <>
          <fieldset className="field">
            <legend>Format</legend>
            <label data-hint="Whole-number samples, 24 bits each: what most programs and mastering chains expect.">
              <input type="radio" name="format" checked={format === "pcm24"} onChange={() => setFormat("pcm24")} />
              24-bit
            </label>
            <label data-hint="Floating-point samples: no rounding, and it keeps levels above full scale.">
              <input type="radio" name="format" checked={format === "float32"} onChange={() => setFormat("float32")} />
              32-bit float
            </label>
          </fieldset>
          <fieldset className="field">
            <legend>Sample rate</legend>
            {RATES.map((r) => (
              <label key={r} data-hint={`Writes the file at ${r / 1000} kHz.`}>
                <input type="radio" name="rate" checked={rate === r} onChange={() => setRate(r)} />
                {`${r / 1000} kHz`}
              </label>
            ))}
          </fieldset>
          <label className="field row" data-hint="How many times the pattern plays in the file, from 1 to 16.">
            <span>Loops</span>
            <input
              type="number"
              min={1}
              max={LOOPS_MAX}
              step={1}
              value={loops}
              onChange={(e) => setLoops(e.target.value)}
              aria-label="Loops"
            />
          </label>
          {phase.error && (
            <p className="modal-message warn" role="alert">
              {phase.error}
            </p>
          )}
          <div className="modal-actions">
            <button type="button" className="btn primary" data-autofocus data-hint="Asks where to save the file, then exports." onClick={start}>
              Export…
            </button>
            <button type="button" className="btn" data-hint="Closes this window without exporting." onClick={onClose}>
              Close
            </button>
          </div>
        </>
      )}
    </Modal>
  );
}
