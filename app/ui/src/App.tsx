import { useCallback, useEffect, useRef, useState } from "react";
import {
  SYSTEM_DEFAULT,
  audioLog,
  audioOutputs,
  audioSelect,
  audioSetGuard,
  audioSettings,
  eventWarns,
  liveEndpoint,
  onAudioEvent,
  testTone,
  type LogEntry,
  type Output,
} from "./audio";
import { connectLive } from "./live/client";
import type { LiveFrame } from "./live/decode";
import { Meter } from "./meter/Meter";
import { OutputChip } from "./output/OutputChip";
import { OutputPopover } from "./output/OutputPopover";
import { Toasts, type Toast } from "./toast/Toasts";

/** How long a toast stays. */
const TOAST_MS = 6000;

export function App() {
  const [outputs, setOutputs] = useState<Output[]>([]);
  const [chosen, setChosen] = useState(SYSTEM_DEFAULT);
  const [guard, setGuard] = useState(true);
  const [log, setLog] = useState<LogEntry[]>([]);
  const [open, setOpen] = useState(false);
  const [toasts, setToasts] = useState<Toast[]>([]);
  // From live frames, set only when they change.
  const [tone, setTone] = useState(false);
  const [latencyFrames, setLatencyFrames] = useState<number | null>(null);
  const latest = useRef<LiveFrame | null>(null);
  const nextToast = useRef(0);

  const refresh = useCallback(() => {
    audioOutputs().then(setOutputs, () => {});
    audioLog().then(setLog, () => {});
  }, []);

  const toast = useCallback((message: string, warn: boolean) => {
    const id = nextToast.current++;
    setToasts((all) => [...all, { id, message, warn }]);
    setTimeout(() => setToasts((all) => all.filter((t) => t.id !== id)), TOAST_MS);
  }, []);

  const loadSettings = useCallback(() => {
    audioSettings().then(
      (s) => {
        setChosen(s.output);
        setGuard(s.guard);
      },
      () => {},
    );
  }, []);

  // Startup: what plays, the saved settings, the log; then follow audio events.
  useEffect(() => {
    refresh();
    loadSettings();
    const unlisten = onAudioEvent((e) => {
      toast(e.message, eventWarns(e.kind));
      refresh();
    });
    return () => {
      unlisten.then((stop) => stop());
    };
  }, [refresh, loadSettings, toast]);

  // The live stream: meters read `latest` every animation frame; the rest becomes state only
  // when it changes.
  useEffect(() => {
    let disconnect: (() => void) | null = null;
    let cancelled = false;
    liveEndpoint().then(
      (endpoint) => {
        if (cancelled) return;
        disconnect = connectLive(endpoint, (frame) => {
          const before = latest.current;
          latest.current = frame;
          if (before?.tone !== frame.tone) setTone(frame.tone);
          if (before?.outputLatencyFrames !== frame.outputLatencyFrames) {
            setLatencyFrames(frame.outputLatencyFrames);
          }
        });
      },
      (e: unknown) => toast(`Live meters are off: ${String(e)}`, true),
    );
    return () => {
      cancelled = true;
      disconnect?.();
    };
  }, [toast]);

  const current = outputs.find((o) => o.current) ?? null;
  const latencyMs =
    current && latencyFrames !== null ? Math.round((latencyFrames / current.sampleRate) * 1000) : null;

  const toggleOpen = () => {
    if (!open) refresh();
    setOpen(!open);
  };
  const close = useCallback(() => setOpen(false), []);
  // A failed change shows why and puts the saved settings back.
  const fail = (e: unknown) => {
    toast(String(e), true);
    loadSettings();
  };

  const select = (id: string) => {
    setChosen(id);
    audioSelect(id).then(refresh, fail);
  };
  const changeGuard = (on: boolean) => {
    setGuard(on);
    audioSetGuard(on).then(refresh, fail);
  };
  // Live frames confirm the tone (and turn it off after a guard stop); set it here too, so the
  // button still works while the stream reconnects.
  const toggleTone = () => {
    const on = !tone;
    testTone(on).then(() => setTone(on), (e: unknown) => toast(String(e), true));
  };

  return (
    <div className="app">
      <header className="toolbar">
        <span className="wordmark">Grooviq</span>
        <div className="toolbar-audio">
          <div className="chip-anchor">
            <OutputChip output={current} latencyMs={latencyMs} open={open} onToggle={toggleOpen} />
            {open && (
              <OutputPopover
                outputs={outputs}
                chosen={chosen}
                guard={guard}
                log={log}
                onSelect={select}
                onGuard={changeGuard}
                onClose={close}
              />
            )}
          </div>
          <button type="button" className="tone" aria-pressed={tone} onClick={toggleTone}>
            Test tone
          </button>
          <Meter latest={latest} />
        </div>
      </header>
      <main className="stage">
        <p className="hint">Nothing plays until you ask: switch on the test tone to check your output.</p>
      </main>
      <Toasts toasts={toasts} onDismiss={(id) => setToasts((all) => all.filter((t) => t.id !== id))} />
    </div>
  );
}
