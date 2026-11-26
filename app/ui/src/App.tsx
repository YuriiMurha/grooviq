import { arrangementHistory, arrangementView } from "./api/arrangement";
import { dispatchGlobalHistory } from "./project/globalHistory";
import { onProjectReplacing, projectGeneration } from "./project/replacement";
import { usePatterns } from "./patterns/usePatterns";
import { Playlist } from "./playlist/Playlist";
import { usePlaylist } from "./playlist/usePlaylist";
import { PickerRail, PatternModeControls } from "./patterns/PickerRail";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { installMenuHistory } from "./project/menuHistory";
import { isProjectReplacing, isProjectReplacementPending } from "./project/replacement";
import { settleProjectEdits } from "./project/pending";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
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
  liveEndpoint,
  onAudioEvent,
  testTone,
  type LogEntry,
  type Output,
} from "./audio";
import { AudioReplay } from "./toast/audioReplay";
import { HintBar } from "./hint/HintBar";
import { MeterHistory, type MeterContext } from "./meter/history";
import { connectLive } from "./live/client";
import type { LiveFrame } from "./live/decode";
import { Meter } from "./meter/Meter";
import { OutputChip } from "./output/OutputChip";
import { OutputPopover } from "./output/OutputPopover";
import { ProjectContext } from "./project/context";
import { metronomeSet, onRecordingStopped, recordSet } from "./api/notes";
import { Workspace } from "./layout/Workspace";
import { ProjectMenu } from "./project/ProjectMenu";
import { SaveState } from "./project/SaveState";
import { historyDragActive, invalidateRollHistory, withRollHistoryBarrier, trackDrags } from "./project/noteHistory";
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
  const playbackStarts = useRef(new Set<() => void>());
  const subscribePlaybackStart = useCallback((wake: () => void) => {
    const listeners = playbackStarts.current;
    listeners.add(wake);
    return () => { listeners.delete(wake); };
  }, []);
  const rulerPosition = useRef<number | null>(null);
  const [meterHistory] = useState(() => new MeterHistory());
  const meterContext = useRef<MeterContext>({});
  const outputRequest = useRef(0);
  const nextToast = useRef(0);
  const audioReplay = useRef(new AudioReplay());
  // The typing keyboard is on by default; R records only while it is off.
  const [keyboard, setKeyboard] = useState(true);
  const keyboardRef = useRef(true);
  const [metronome, setMetronome] = useState(false);

  const refreshOutputs = useCallback((active: () => boolean = () => true) => {
    const request = ++outputRequest.current;
    audioOutputs().then((outputs) => {
      if (!active() || request !== outputRequest.current) return;
      const output = outputs.find((o) => o.current);
      const context = { id: output?.id, rate: output?.sampleRate };
      if (context.id !== meterContext.current.id || !Object.is(context.rate, meterContext.current.rate)) {
        meterHistory.reset();
      }
      meterContext.current = context;
      setOutputs(outputs);
    }, () => {});
  }, [meterHistory]);
  const refresh = useCallback(() => {
    refreshOutputs();
    audioLog().then(setLog, () => {});
  }, [refreshOutputs]);

  const toast = useCallback((message: string, warn: boolean) => {
    const id = nextToast.current++;
    setToasts((all) => [...all, { id, message, warn }]);
    setTimeout(() => setToasts((all) => all.filter((t) => t.id !== id)), TOAST_MS);
  }, []);

  const session = useSession(useCallback((message: string) => toast(message, true), [toast]));
  const { view } = session;
  const patterns = usePatterns({ view, publishPatterns: session.publishPatterns, enqueue: session.enqueue, run: session.context.run });
  const playlist = usePlaylist(session);
  const { invalidateWaveforms } = playlist;
  useEffect(() => onProjectReplacing(() => { rulerPosition.current = null; }), []);

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
    unsettled: () => session.unsettled() || rollEditsUnsettled() || patterns.unsettled() || playlist.unsettled(),
    settle: settleProjectEdits,
    settleReplacement: () => settleProjectEdits(playlist.settleMedia),
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
  // Text/menu routing stays outside this shared project-history command.
  const stepHistory = useCallback((redo: boolean) => {
    if (isProjectReplacing() || historyDragActive()) return;
    const origin = session.context.stamp();
    const current = () => !isProjectReplacementPending() && projectGeneration() === origin.replacements &&
      session.latest.current.view?.generation === origin.generation;
    void dispatchGlobalHistory(origin, redo, {
      isCurrent: current,
      blocked: historyDragActive,
      settle: settleProjectEdits,
      ordered: async (command) => {
        const result = await withRollHistoryBarrier(
          (still) => session.history(origin, command, still, invalidateRollHistory), current,
          (reply) => reply.status === "applied",
        );
        if (result.status === "completed") return result.value;
        if (result.error) throw result.error;
        return { status: result.reason === "blocked" ? "blocked" : "stale" };
      },
      stamp: arrangementView,
      invoke: arrangementHistory,
      // Native view adoption + synchronous roll invalidation happen in session.history.
      refresh: async effect => { if (effect.kind === "arrangement") invalidateWaveforms(); },
    }).then((reply) => {
      if (!current()) return;
      if (reply.status === "refused") toast(reply.message, true);
      else if (reply.status === "unsettled") toast("Pending edits were refused. History was not changed.", true);
    }, (error: unknown) => { if (current()) toast(errorText(error), true); });
  }, [session, toast, invalidateWaveforms]);
  useEffect(() => trackDrags(window), []);
  const menuHistoryAction = useRef(stepHistory);
  useLayoutEffect(() => { menuHistoryAction.current = stepHistory; }, [stepHistory]);
  useEffect(() => {
    const sync = () => {
      const text = isTypingTarget(document.activeElement);
      void invoke("edit_history_enabled", {
        undo: text ? (document.queryCommandEnabled?.("undo") ?? false) : Boolean(view?.projectCanUndo),
        redo: text ? (document.queryCommandEnabled?.("redo") ?? false) : Boolean(view?.projectCanRedo),
      }).catch(() => {});
    };
    sync();
    document.addEventListener("focusin", sync);
    document.addEventListener("input", sync);
    document.addEventListener("selectionchange", sync);
    return () => {
      document.removeEventListener("focusin", sync);
      document.removeEventListener("input", sync);
      document.removeEventListener("selectionchange", sync);
    };
  }, [view?.projectCanUndo, view?.projectCanRedo]);
  useEffect(() => {
    const router = installMenuHistory(document, (redo) => menuHistoryAction.current(redo), () => modalIsOpen());
    let disposed = false;
    const listening = listen<boolean>("edit-history", ({ payload }) => {
      if (!disposed && typeof payload === "boolean") router.dispatch(payload);
    });
    return () => {
      disposed = true;
      router.dispose();
      void listening.then((stop) => stop());
    };
  }, []);
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
    audioSettings().then((settings) => {
      setChosen(settings.output);
      setGuard(settings.guard);
    }, () => {});
  }, []);

  // Subscribe before reading history: native logs each event before emitting it.
  useEffect(() => {
    let active = true;
    const refreshAudio = () => {
      meterHistory.reset();
      meterContext.current = {};
      refreshOutputs(() => active);
      audioLog().then((entries) => { if (active) setLog(entries); }, () => {});
    };
    refreshOutputs(() => active);
    audioSettings().then((settings) => {
      if (!active) return;
      setChosen(settings.output);
      setGuard(settings.guard);
    }, () => {});
    const stop = audioReplay.current.connect(onAudioEvent, async () => {
      const entries = await audioLog();
      if (active) setLog(entries);
      return entries;
    }, toast, refreshAudio);
    return () => { active = false; stop(); };
  }, [toast, refreshOutputs, meterHistory]);

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

  // Capture each meter observation before latest is overwritten; the rest becomes state only
  // when it changes.
  useEffect(() => {
    let disconnect: (() => void) | null = null;
    let cancelled = false;
    liveEndpoint().then(
      (endpoint) => {
        if (cancelled) return;
        disconnect = connectLive(endpoint, (frame) => {
          meterHistory.push(frame, meterContext.current, performance.now());
          const before = latest.current;
          latest.current = frame;
          if (!isProjectReplacementPending()) rulerPosition.current = Number(frame.positionTicks);
          if (before?.tone !== frame.tone) setTone(frame.tone);
          const believed = playGuard.current.believe(frame.playing);
          if (frame.playing) {
            // A Play the UI did not send (another window): followed on the edge.
            if (before?.playing !== true) {
              playbackStarts.current.forEach((wake) => wake());
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
        }, { onConnectionChange: () => {
          meterHistory.reset();
          rulerPosition.current = null;
        } });
      },
      (e: unknown) => toast(`Live meters are off: ${String(e)}`, true),
    );
    return () => {
      cancelled = true;
      disconnect?.();
    };
  }, [toast, releaseHeld, meterHistory]);

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
              playMode={patterns.mode}
              modeControls={<PatternModeControls picker={patterns} />}
              onPlay={play}
              onStop={stop}
              onTempo={(bpm) => session.apply({ kind: "setTempo", bpm })}
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
            <Meter history={meterHistory} />
          </div>
        </header>
        <main className="stage">
          {view && <Workspace picker={<PickerRail key={view.generation} picker={patterns} />} playlist={<Playlist rulerPosition={rulerPosition} model={playlist} patterns={patterns.state.canonical} bpm={view.bpm} beats={view.timeSignature.beats} unit={view.timeSignature.unit} notifyAudioDrop={message => toast(message, false)} />} latest={latest} subscribePlaybackStart={subscribePlaybackStart} sampleRate={sampleRate} onError={rackError} keyboard={keyboard} releaseKeys={releaseKeys} />}
        </main>
        <HintBar />
        <Toasts toasts={toasts} onDismiss={(id) => setToasts((all) => all.filter((t) => t.id !== id))} />
        {flows.dialogs}
      </div>
    </ProjectContext.Provider>
  );
}
