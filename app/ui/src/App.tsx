import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  autosaveStatus,
  errorText,
  onAutosaveFailed,
  projectView,
  transportPlay,
  transportStop,
} from "./api/project";
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
import { HintBar } from "./hint/HintBar";
import { connectLive } from "./live/client";
import type { LiveFrame } from "./live/decode";
import { Meter } from "./meter/Meter";
import { OutputChip } from "./output/OutputChip";
import { OutputPopover } from "./output/OutputPopover";
import { ProjectContext } from "./project/context";
import { metronomeSet, notesRedo, notesUndo, onRecordingStopped, recordSet } from "./api/notes";
import { Workspace } from "./layout/Workspace";
import { ProjectMenu } from "./project/ProjectMenu";
import { SaveState } from "./project/SaveState";
import { stepNotesHistory, trackDrags } from "./project/noteHistory";
import { rollEditsUnsettled } from "./roll/commitQueue";
import { isTypingTarget, modalIsOpen, shortcutFor } from "./project/shortcuts";
import { useProjectFlows } from "./project/useProjectFlows";
import { useSession } from "./project/useSession";
import { Toasts, type Toast } from "./toast/Toasts";
import { KeyboardToggle } from "./transport/KeyboardToggle";
import { MetronomeButton } from "./transport/MetronomeButton";
import { RecordButton } from "./transport/RecordButton";
import { PlayGuard } from "./transport/playGuard";
import { TransportBar } from "./transport/TransportBar";

/** How long a toast stays. */
const TOAST_MS = 6000;
/** When no output is known yet, for the latency shift of the position. */
const FALLBACK_RATE = 48000;

export function App() {
  const [outputs, setOutputs] = useState<Output[]>([]);
  const [chosen, setChosen] = useState(SYSTEM_DEFAULT);
  const [guard, setGuard] = useState(true);
  const [log, setLog] = useState<LogEntry[]>([]);
  const [open, setOpen] = useState(false);
  const [toasts, setToasts] = useState<Toast[]>([]);
  // From live frames, set only when they change.
  const [tone, setTone] = useState(false);
  const [playing, setPlaying] = useState(false);
  const playingRef = useRef(false);
  // Bumped whenever the transport stops (or the project is replaced): the typing keyboard lets go
  // of every held key then.
  const [releaseKeys, setReleaseKeys] = useState(0);
  const releaseHeld = useCallback(() => setReleaseKeys((n) => n + 1), []);
  // Doubts "stopped" frames right after a Play (they may predate it).
  const playGuard = useRef(new PlayGuard());
  const [latencyFrames, setLatencyFrames] = useState<number | null>(null);
  const latest = useRef<LiveFrame | null>(null);
  const nextToast = useRef(0);
  // The typing keyboard is on by default; R records only while it is off.
  const [keyboard, setKeyboard] = useState(true);
  const keyboardRef = useRef(true);
  const [metronome, setMetronome] = useState(false);

  const refresh = useCallback(() => {
    audioOutputs().then(setOutputs, () => {});
    audioLog().then(setLog, () => {});
  }, []);

  const toast = useCallback((message: string, warn: boolean) => {
    const id = nextToast.current++;
    setToasts((all) => [...all, { id, message, warn }]);
    setTimeout(() => setToasts((all) => all.filter((t) => t.id !== id)), TOAST_MS);
  }, []);

  const session = useSession(useCallback((message: string) => toast(message, true), [toast]));
  const { view } = session;

  // A project that is replaced must not go on playing, nor keep notes held on its channels.
  const stopIfPlaying = useCallback(async () => {
    releaseHeld();
    if (playingRef.current) {
      await transportStop().catch(() => {});
      playingRef.current = false;
      setPlaying(false);
    }
  }, [releaseHeld]);
  const flows = useProjectFlows({
    view,
    adopt: session.adopt,
    enqueue: session.enqueue,
    toast,
    stopIfPlaying,
    unsettled: () => session.unsettled() || rollEditsUnsettled(),
  });
  const { perform, modalOpen } = flows;

  const setPlayingNow = useCallback((on: boolean) => {
    playingRef.current = on;
    setPlaying(on);
  }, []);
  const play = useCallback(() => {
    transportPlay().then(
      () => {
        playGuard.current.played();
        setPlayingNow(true);
      },
      (e: unknown) => toast(errorText(e), true),
    );
  }, [setPlayingNow, toast]);
  const stop = useCallback(() => {
    // Every Stop that succeeds lets go of held keys, whether or not the UI thought it played.
    transportStop().then(
      () => {
        setPlayingNow(false);
        releaseHeld();
      },
      (e: unknown) => toast(errorText(e), true),
    );
  }, [setPlayingNow, releaseHeld, toast]);

  // The window title: "name — Grooviq", with a bullet while there are unsaved changes.
  const projectName = view?.name;
  const dirty = view?.dirty;
  useEffect(() => {
    if (projectName === undefined) return;
    const title = `${dirty ? "• " : ""}${projectName} — Grooviq`;
    document.title = title;
    try {
      getCurrentWindow().setTitle(title).catch(() => {});
    } catch {
      /* not inside the Tauri shell (a plain browser) */
    }
  }, [projectName, dirty]);

  const recording = view?.recording ?? false;
  const recordingRef = useRef(recording);
  useEffect(() => {
    recordingRef.current = recording;
    keyboardRef.current = keyboard;
  }, [recording, keyboard]);
  const runCommand = session.context.run;
  const toggleRecord = useCallback(() => {
    const on = !recordingRef.current;
    void runCommand(() => recordSet(on));
  }, [runCommand]);
  // Recording switched itself off (the pattern is full): say what was lost, show it off.
  useEffect(() => {
    const listening = onRecordingStopped((s) => {
      toast(s.message, true);
      void runCommand(projectView);
    });
    return () => {
      listening.then((stop) => stop());
    };
  }, [runCommand, toast]);
  // Undo and redo of note changes: in the project's queue (behind step edits) and, with the
  // piano roll open, in its queue too (behind note edits). Ignored during a drag.
  const { enqueue, adopt } = session;
  const shown = session.latest;
  const stepHistory = useCallback(
    (redo: boolean) => {
      // For the project shown now: if another replaced it before the command's turn, the engine
      // refuses it.
      const generation = shown.current.view?.generation ?? 0;
      stepNotesHistory(
        () => enqueue(() => (redo ? notesRedo : notesUndo)(generation)),
        (ack) => adopt(ack.view),
        (e: unknown) => toast(errorText(e), true),
      );
    },
    [enqueue, adopt, shown, toast],
  );
  useEffect(() => trackDrags(window), []);
  const toggleMetronome = () => {
    const on = !metronome;
    metronomeSet(on).then(() => setMetronome(on), (e: unknown) => toast(errorText(e), true));
  };

  // Shortcuts. They stand aside while a dialog is open.
  const modalRef = useRef(false);
  useEffect(() => {
    modalRef.current = modalOpen;
  }, [modalOpen]);
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (modalRef.current || modalIsOpen()) return;
      const action = shortcutFor(e, navigator.platform, keyboardRef.current);
      if (!action) return;
      e.preventDefault();
      // A held key repeats: one save (or open, export...) per press, not one per repeat.
      if (e.repeat) return;
      if (action === "playStop") (playingRef.current ? stop : play)();
      else if (action === "record") toggleRecord();
      else if (action === "undo" || action === "redo") stepHistory(action === "redo");
      else perform(action);
    };
    // A focused button presses itself when Space is released: play/stop took that key.
    const onKeyUp = (e: KeyboardEvent) => {
      if (e.key === " " && !modalRef.current && !modalIsOpen() && !isTypingTarget(e.target) && !e.ctrlKey && !e.metaKey) {
        e.preventDefault();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
    };
  }, [perform, play, stop, toggleRecord, stepHistory]);

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

  // Autosave trouble the person must hear of. Listen first, then ask what already happened
  // (a copy running without autosave, a failure before the listener was there); a failure told
  // by both is shown once.
  useEffect(() => {
    let cancelled = false;
    let lastEvent: string | null = null;
    const listening = onAutosaveFailed((message) => {
      lastEvent = message;
      toast(message, true);
    });
    listening
      .then(() => autosaveStatus())
      .then(
        (status) => {
          if (cancelled) return;
          if (status.disabled) toast(status.disabled, true);
          if (status.failure && status.failure !== lastEvent) toast(status.failure, true);
        },
        () => {},
      );
    return () => {
      cancelled = true;
      listening.then((stop) => stop());
    };
  }, [toast]);

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
          const believed = playGuard.current.believe(frame.playing);
          if (frame.playing) {
            // A Play the UI did not send (another window): followed on the edge.
            if (before?.playing !== true) {
              playingRef.current = true;
              setPlaying(true);
            }
          } else if (believed && (before?.playing === true || playingRef.current)) {
            // The engine stopped: seen as an edge, or only against the UI's own state (a stall,
            // a stop the UI did not ask for). The UI follows and lets go of held keys.
            playingRef.current = false;
            setPlaying(false);
            releaseHeld();
          }
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
  }, [toast, releaseHeld]);

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

  const sampleRate = current?.sampleRate ?? FALLBACK_RATE;
  const rackError = useCallback((message: string) => toast(message, true), [toast]);

  return (
    <ProjectContext.Provider value={session.context}>
      <div className="app">
        <header className="toolbar">
          {view && (
            <div className="toolbar-project">
              <ProjectMenu name={view.name} platform={navigator.platform} onAction={perform} />
              <SaveState
                dirty={view.dirty}
                untitled={view.path === null}
                cleanSince={session.cleanSince}
                autosavedAt={flows.autosavedAt ?? view.autosavedAt}
              />
            </div>
          )}
          {view && (
            <TransportBar
              view={view}
              playing={playing}
              latest={latest}
              sampleRate={sampleRate}
              onPlay={play}
              onStop={stop}
              onTempo={(bpm) => void session.apply({ kind: "setTempo", bpm })}
              onSignature={(beats, unit) => void session.apply({ kind: "setTimeSignature", beats, unit })}
              extra={
                <>
                  <RecordButton recording={recording} onToggle={toggleRecord} />
                  <MetronomeButton on={metronome} onToggle={toggleMetronome} />
                  <KeyboardToggle on={keyboard} onToggle={() => setKeyboard((on) => !on)} />
                </>
              }
            />
          )}
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
            <button
              type="button"
              className="tone"
              aria-pressed={tone}
              data-hint="Plays a steady tone so you can check your output. Nothing else makes sound until you ask."
              onClick={toggleTone}
            >
              Test tone
            </button>
            <Meter latest={latest} />
          </div>
        </header>
        <main className="stage">
          {view && <Workspace latest={latest} sampleRate={sampleRate} onError={rackError} keyboard={keyboard} releaseKeys={releaseKeys} />}
        </main>
        <HintBar />
        <Toasts toasts={toasts} onDismiss={(id) => setToasts((all) => all.filter((t) => t.id !== id))} />
        {flows.dialogs}
      </div>
    </ProjectContext.Provider>
  );
}
