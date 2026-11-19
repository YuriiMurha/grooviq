import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent, type PointerEvent, type RefObject, type ReactNode } from "react";
import { channelSelect } from "../api/notes";
import { errorText, type ChannelView } from "../api/project";
import { DeviceWindows } from "../devices/DeviceWindows";
import { useTypingKeyboard } from "../keyboard/useTypingKeyboard";
import type { LiveFrame } from "../live/decode";
import { useProject } from "../project/context";
import { isTypingTarget, modalIsOpen } from "../project/shortcuts";
import { relinkChannel } from "../rack/audioFiles";
import { ChannelRack } from "../rack/ChannelRack";
import { PianoRoll } from "../roll/PianoRoll";
import { DEFAULT_PANELS, panelFor, rollTarget, splitAfterDrag, togglePanel, type Panels } from "./panels";
import { SelectionContext, type Selection } from "./selection";

/** What one arrow key moves the splitter by. */
const SPLIT_KEY_STEP = 0.05;

interface Props {
  picker?: ReactNode;
  latest: RefObject<LiveFrame | null>;
  sampleRate: number;
  onError: (message: string) => void;
  /** The typing keyboard is on. */
  keyboard: boolean;
  /** Changes whenever the transport stops or the project is replaced: held keys are let go. */
  releaseKeys: number;
}

/** The channel rack and the piano roll, stacked, each toggled by its key (F6, F7), with a
 * splitter between them; the selected channel they and the typing keyboard share. */
export function Workspace({ latest, sampleRate, onError, keyboard, releaseKeys, picker }: Props) {
  const { view, runFor, stamp } = useProject();
  const [panels, setPanels] = useState<Panels>(DEFAULT_PANELS);
  const [playlistVisible, setPlaylistVisible] = useState(false);
  const [chosen, setChosen] = useState<number | null>(null);
  const area = useRef<HTMLDivElement>(null);
  const drag = useRef<{ y: number; start: number } | null>(null);

  const ids = useMemo(() => (view?.channels ?? []).map((c) => c.id), [view?.channels]);
  // The selected channel: the one chosen while it exists, else the first.
  const channel = rollTarget(chosen, ids);
  const selection: Selection = useMemo(() => ({ channel, select: setChosen }), [channel]);

  // The engine plays MIDI input on the selected channel.
  useEffect(() => {
    channelSelect(channel).catch((e: unknown) => onError(errorText(e)));
  }, [channel, onError]);

  useTypingKeyboard(keyboard, channel, onError, { release: releaseKeys, recording: view?.recording ?? false });

  const relink = useCallback(
    (c: ChannelView) => relinkChannel(c, { runFor, stamp }, onError),
    [runFor, stamp, onError],
  );

  const openRoll = useCallback((id: number) => {
    setChosen(id);
    setPanels((p) => ({ ...p, roll: true }));
  }, []);

  useEffect(() => {
    const onKeyDown = (e: globalThis.KeyboardEvent) => {
      const panel = panelFor(e.key);
      if (!panel || e.metaKey || e.ctrlKey || e.altKey || e.shiftKey || modalIsOpen()) return;
      if (isTypingTarget(e.target)) return;
      e.preventDefault();
      if (e.repeat) return;
      setPanels((p) => togglePanel(p, panel));
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  useEffect(() => {
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "F5" || event.metaKey || event.ctrlKey || event.altKey || event.shiftKey || modalIsOpen() || isTypingTarget(event.target)) return;
      event.preventDefault();
      if (!event.repeat) setPlaylistVisible((visible) => !visible);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const both = panels.rack && panels.roll;
  const onSplitDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    drag.current = { y: e.clientY, start: panels.split };
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* no pointer capture here */
    }
  };
  const onSplitMove = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d) return;
    const height = area.current?.clientHeight ?? 0;
    setPanels((p) => ({ ...p, split: splitAfterDrag(d.start, e.clientY - d.y, height) }));
  };
  const onSplitKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const dir = e.key === "ArrowUp" ? -1 : e.key === "ArrowDown" ? 1 : 0;
    if (!dir) return;
    e.preventDefault();
    setPanels((p) => ({ ...p, split: splitAfterDrag(p.split, dir * SPLIT_KEY_STEP, 1) }));
  };

  if (!view) return null;
  const rollChannel = view.channels.find((c) => c.id === channel) ?? null;

  return (
    <SelectionContext.Provider value={selection}>
      <div className="pattern-workspace">
        {picker}
        <div className="pattern-workspace-content">
          {playlistVisible && <section className="pattern-playlist-shell" aria-label="Playlist">
            <h2>Playlist</h2><p>Your patterns are ready. Timeline editing is not available yet. F5 hides the playlist.</p>
          </section>}
      <div ref={area} className="workspace" style={{ "--split": `${panels.split * 100}%` } as CSSProperties}>
        {panels.rack && (
          <div className={`panel panel-rack${both ? " split" : ""}`}>
            <ChannelRack latest={latest} sampleRate={sampleRate} onError={onError} onOpenRoll={openRoll} />
          </div>
        )}
        {both && (
          <div
            className="splitter"
            role="separator"
            aria-orientation="horizontal"
            aria-label="Resize the channel rack and the piano roll"
            aria-valuenow={Math.round(panels.split * 100)}
            aria-valuemin={0}
            aria-valuemax={100}
            tabIndex={0}
            data-hint="Drag to share the height between the channel rack and the piano roll."
            onPointerDown={onSplitDown}
            onPointerMove={onSplitMove}
            onPointerUp={() => (drag.current = null)}
            onPointerCancel={() => (drag.current = null)}
            onKeyDown={onSplitKey}
          />
        )}
        {panels.roll && (
          <section
            className="panel panel-roll"
            aria-label="Piano roll"
            data-hint={
              rollChannel
                ? `The piano roll for ${rollChannel.name}. F7 closes it, F6 shows or hides the channel rack.`
                : "The piano roll. Add a channel to draw notes for it."
            }
          >
            <header className="rack-head">
              <h2>Piano roll</h2>
              {rollChannel && (
                <span className="chip" style={{ "--ink": `var(--ink-${rollChannel.color + 1})` } as CSSProperties}>
                  <b>{rollChannel.name}</b>
                </span>
              )}
            </header>
            <PianoRoll
              channel={rollChannel?.id ?? null}
              latest={latest}
              sampleRate={sampleRate}
              keyboard={keyboard}
              onError={onError}
              releaseKeys={releaseKeys}
            />
          </section>
        )}
        {!panels.rack && !panels.roll && (
          <p className="workspace-empty">F6 shows the channel rack, F7 the piano roll.</p>
        )}
      </div>
        </div>
      </div>
      <DeviceWindows generation={view.generation} channels={view.channels} onError={onError} onRelink={relink} />
    </SelectionContext.Provider>
  );
}
