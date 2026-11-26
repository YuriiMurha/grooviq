import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent, type PointerEvent, type RefObject, type ReactNode } from "react";
import { flushSync } from "react-dom";
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
import { acceptedSplit, fitPanelLayout, panelMinimums, type PanelChrome } from "./panelLayout";
import { DEFAULT_PANELS, panelFor, rollTarget, togglePanel, type Panels } from "./panels";
import { SelectionContext, type Selection } from "./selection";

/** What one arrow key moves the splitter by. */
const SPLIT_KEY_STEP = 0.05;

type PanelMeasurement = Readonly<{ height: number; width: number; top: number; left: number; chrome: PanelChrome }>;
type SplitDrag = { pointerId: number; y: number; startHeight: number; lastHeight: number;
  measurement: PanelMeasurement; scrollTop: number; moved: boolean };
const percent = (ratio: number) => Math.round(ratio * 1000) / 10;

function readPanels(viewport: HTMLDivElement | null, area: HTMLDivElement | null): PanelMeasurement | null {
  if (!viewport || !area) return null;
  const box = viewport.getBoundingClientRect();
  const height = (selector: string) => area.querySelector(selector)?.getBoundingClientRect().height ?? 0;
  const body = area.querySelector(".rack-body");
  const padding = body ? getComputedStyle(body) : null;
  return { height: box.height, width: area.getBoundingClientRect().width, top: box.top, left: box.left,
    chrome: { rackHeader: height(".panel-rack .rack-head"), firstRackRow: height(".rack-rows .channel-row"),
      rackPadding: padding ? parseFloat(padding.paddingTop) + parseFloat(padding.paddingBottom) : 0,
      rollHeader: height(".panel-roll > .rack-head"), tools: height(".roll-tools"),
      velocity: height(".roll-lane"), splitter: height(".splitter") } };
}

function sameMeasurement(first: PanelMeasurement, second: PanelMeasurement) {
  return first.height === second.height && first.width === second.width && first.top === second.top && first.left === second.left
    && (Object.keys(first.chrome) as (keyof PanelChrome)[]).every((key) => first.chrome[key] === second.chrome[key]);
}

function usePanelMeasurement(viewport: RefObject<HTMLDivElement | null>, area: RefObject<HTMLDivElement | null>,
  lifetime: string, retireDrag: () => void) {
  const [measurement, setMeasurement] = useState<PanelMeasurement | null>(null);
  useLayoutEffect(() => {
    const root = area.current;
    const container = viewport.current;
    if (!root || !container) return;
    let previous: PanelMeasurement | null = null;
    const measure = (beforePaint: boolean) => {
      const next = readPanels(container, root);
      if (!next || (previous && sameMeasurement(previous, next))) return;
      previous = next;
      retireDrag();
      // ResizeObserver runs before paint. Keep allocation and observed chrome in the
      // same frame without observing the fitted pane/body heights themselves.
      if (beforePaint) flushSync(() => setMeasurement(next));
      else setMeasurement(next);
    };
    const observer = new ResizeObserver(() => measure(true));
    const bind = () => {
      observer.disconnect();
      observer.observe(container);
      for (const node of root.querySelectorAll(".rack-head, .rack-rows > .channel-row:first-child, .roll-tools, .roll-lane, .splitter")) {
        observer.observe(node);
      }
    };
    bind();
    measure(false);
    const onResize = () => measure(true);
    window.addEventListener("resize", onResize);
    // Only structural chrome changes matter: velocity visibility and the first rack
    // row. Note/canvas/selection rendering never enters this observer.
    const structural = new MutationObserver(() => { bind(); measure(true); });
    for (const node of root.querySelectorAll(".roll, .rack-rows")) structural.observe(node, { childList: true });
    return () => { window.removeEventListener("resize", onResize); observer.disconnect(); structural.disconnect(); retireDrag(); };
  }, [viewport, area, lifetime, retireDrag]);
  return measurement;
}

interface Props {
  picker?: ReactNode;
  playlist?: ReactNode;
  latest: RefObject<LiveFrame | null>;
  subscribePlaybackStart: (wake: () => void) => () => void;
  sampleRate: number;
  onError: (message: string) => void;
  /** The typing keyboard is on. */
  keyboard: boolean;
  /** Changes whenever the transport stops or the project is replaced: held keys are let go. */
  releaseKeys: number;
}

/** The channel rack and the piano roll, stacked, each toggled by its key (F6, F7), with a
 * splitter between them; the selected channel they and the typing keyboard share. */
export function Workspace({ latest, subscribePlaybackStart, sampleRate, onError, keyboard, releaseKeys, picker, playlist }: Props) {
  const { view, runFor, stamp } = useProject();
  const [panels, setPanels] = useState<Panels>(DEFAULT_PANELS);
  const [playlistVisible, setPlaylistVisible] = useState(false);
  const [chosen, setChosen] = useState<number | null>(null);
  const viewport = useRef<HTMLDivElement>(null);
  const area = useRef<HTMLDivElement>(null);
  const rackPane = useRef<HTMLDivElement>(null);
  const drag = useRef<SplitDrag | null>(null);
  const retireDrag = useCallback(() => { drag.current = null; }, []);

  const ids = useMemo(() => (view?.channels ?? []).map((c) => c.id), [view?.channels]);
  const measurement = usePanelMeasurement(viewport, area,
    `${view?.generation}:${panels.rack}:${panels.roll}:${ids.length}:${playlistVisible}`, retireDrag);
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
  const layout = measurement ? fitPanelLayout({ viewportHeight: measurement.height, requestedRatio: panels.split, chrome: measurement.chrome }) : null;
  const minimum = measurement ? panelMinimums(measurement.chrome) : null;
  const contentHeight = both ? layout?.contentHeight : measurement && minimum
    ? Math.max(measurement.height, panels.roll ? minimum.roll : panels.rack ? minimum.rack : 0) : undefined;
  const onSplitDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const current = readPanels(viewport.current, area.current);
    const height = rackPane.current?.getBoundingClientRect().height;
    if (!current || height === undefined) return;
    drag.current = { pointerId: e.pointerId, y: e.clientY, startHeight: height, lastHeight: height,
      measurement: current, scrollTop: viewport.current?.scrollTop ?? 0, moved: false };
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* no pointer capture here */
    }
  };
  const onSplitMove = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d || d.pointerId !== e.pointerId) return;
    const current = readPanels(viewport.current, area.current);
    if (!current || !sameMeasurement(d.measurement, current) || d.scrollTop !== viewport.current?.scrollTop) {
      retireDrag();
      return;
    }
    const dy = e.clientY - d.y;
    if (dy === 0 && !d.moved) return;
    const currentLayout = fitPanelLayout({ viewportHeight: current.height, requestedRatio: panels.split, chrome: current.chrome });
    const split = acceptedSplit(currentLayout, (d.startHeight + dy) / currentLayout.contentHeight);
    const target = fitPanelLayout({ viewportHeight: current.height, requestedRatio: split, chrome: current.chrome });
    if (target.rackHeight === d.lastHeight) return;
    d.moved = true;
    d.lastHeight = target.rackHeight;
    setPanels((p) => ({ ...p, split }));
  };
  const onSplitKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const dir = e.key === "ArrowUp" ? -1 : e.key === "ArrowDown" ? 1 : 0;
    if (!dir) return;
    e.preventDefault();
    retireDrag();
    const current = readPanels(viewport.current, area.current);
    const height = rackPane.current?.getBoundingClientRect().height;
    if (!current || height === undefined) return;
    const currentLayout = fitPanelLayout({ viewportHeight: current.height, requestedRatio: panels.split, chrome: current.chrome });
    const split = acceptedSplit(currentLayout, height / currentLayout.contentHeight + dir * SPLIT_KEY_STEP);
    const target = fitPanelLayout({ viewportHeight: current.height, requestedRatio: split, chrome: current.chrome });
    if (target.rackHeight !== height) setPanels((p) => ({ ...p, split }));
  };

  if (!view) return null;
  const rollChannel = view.channels.find((c) => c.id === channel) ?? null;

  return (
    <SelectionContext.Provider value={selection}>
      <div className="pattern-workspace">
        {picker}
        <div className="pattern-workspace-content">
          {playlistVisible && (playlist ?? <section className="pattern-playlist-shell" aria-label="Playlist">
            <h2>Playlist</h2><p>Your patterns are ready. Timeline editing is not available yet. F5 hides the playlist.</p>
          </section>)}
      <div ref={viewport} className="workspace-viewport" onScroll={retireDrag}>
      <div ref={area} className="workspace" style={{ "--split": `${panels.split * 100}%`,
        "--workspace-content-height": contentHeight === undefined ? "100%" : `${contentHeight}px`,
        "--rack-height": layout ? `${layout.rackHeight}px` : undefined } as CSSProperties}>
        {panels.rack && (
          <div ref={rackPane} className={`panel panel-rack${both ? " split" : ""}`}>
            <ChannelRack latest={latest} subscribePlaybackStart={subscribePlaybackStart} sampleRate={sampleRate} onError={onError} onOpenRoll={openRoll} />
          </div>
        )}
        {both && (
          <div
            className="splitter"
            role="separator"
            aria-orientation="horizontal"
            aria-label="Resize the channel rack and the piano roll"
            aria-valuenow={percent(layout?.effectiveRatio ?? panels.split)}
            aria-valuemin={percent(layout?.lowerRatio ?? 0)}
            aria-valuemax={percent(layout?.upperRatio ?? 1)}
            aria-valuetext={layout ? `Channel rack ${percent(layout.effectiveRatio)}%, piano roll ${percent(layout.rollHeight / layout.contentHeight)}%` : undefined}
            tabIndex={0}
            data-hint="Drag to share the height between the channel rack and the piano roll."
            onPointerDown={onSplitDown}
            onPointerMove={onSplitMove}
            onPointerUp={() => (drag.current = null)}
            onPointerCancel={() => (drag.current = null)}
            onLostPointerCapture={() => (drag.current = null)}
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
                <span className="meta-chip" style={{ "--ink": `var(--ink-${rollChannel.color + 1})` } as CSSProperties}>
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
      </div>
      <DeviceWindows generation={view.generation} channels={view.channels} onError={onError} onRelink={relink} />
    </SelectionContext.Provider>
  );
}
