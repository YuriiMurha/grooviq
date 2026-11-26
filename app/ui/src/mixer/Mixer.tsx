import { useCallback, useEffect, useLayoutEffect, useMemo, useReducer, useRef, useState, type CSSProperties } from "react";
import { Inspector } from "./Inspector";
import { MixerGestures, navTarget, renderIndices, revealScroll, unsupportedReason, visibleWindow, type MixerProps, type MixerTrack, type ParamTarget, type Window } from "./mixerModel";
import { TrackStrip, type StripActions } from "./TrackStrip";
import "./mixer.css";

/** Strip width in CSS px, shared with the windowed row. */
const PITCH = 80;
/** Strips drawn beyond each edge of the view. */
const OVERSCAN = 2;
/** The meter's floor: quieter reads as empty. */
const FLOOR_DB = -60;

function barLength(peak: number): number {
  if (!(peak > 0)) return 0;
  return Math.min(1, Math.max(0, (20 * Math.log10(peak) - FLOOR_DB) / -FLOOR_DB));
}

/** The mixer: windowed vertical strips, the pinned master and the selected track's inspector. A
 * controlled component: the host's props are the truth, and every change is a command. */
export function Mixer(props: MixerProps) {
  const { tracks, master, effects, selectedId, error, meters, unsupported } = props;
  const latest = useRef(props);
  const byId = useMemo(() => {
    const m = new Map<string, MixerTrack>(tracks.map((t) => [t.id, t]));
    m.set(master.id, master);
    return m;
  }, [tracks, master]);
  const latestNodes = useRef(byId);
  const [, repaint] = useReducer((n: number) => n + 1, 0);

  const [gestures] = useState(() => new MixerGestures());

  // Host updates first reach the gestures: one on a removed target ends before anything else runs.
  useLayoutEffect(() => {
    latest.current = props;
    latestNodes.current = byId;
    gestures.connect({
      emit: (c) => props.onCommand({ type: "param", ...c }),
      stored: (t: ParamTarget) => {
        const node = byId.get(t.nodeId);
        if (!node) return undefined;
        if (t.kind === "send") return node.sends.find((s) => s.id === t.sendId)?.amount;
        return t.kind === "level" ? node.level : node.pan;
      },
      locked: (t) => !!byId.get(t.nodeId)?.pending,
      changed: repaint,
    });
    gestures.retain();
  });
  useEffect(() => {
    gestures.resume();
    const keydown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && gestures.cancelAll()) {
        event.preventDefault();
        event.stopPropagation();
      }
    };
    const blur = () => { gestures.cancelAll(); };
    window.addEventListener("keydown", keydown, true);
    window.addEventListener("blur", blur);
    return () => {
      window.removeEventListener("keydown", keydown, true);
      window.removeEventListener("blur", blur);
      gestures.dispose();
    };
  }, [gestures]);

  // ---------------- windowing ----------------
  const scroller = useRef<HTMLDivElement>(null);
  const [win, setWin] = useState<Window>({ start: 0, end: Math.min(tracks.length, 12) });
  const measure = useCallback(() => {
    const el = scroller.current;
    if (!el) return;
    const next = visibleWindow({ count: latest.current.tracks.length, scroll: el.scrollLeft, viewport: el.clientWidth, pitch: PITCH, overscan: OVERSCAN });
    setWin((w) => (w.start === next.start && w.end === next.end ? w : next));
  }, []);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [measure]);
  useLayoutEffect(measure, [measure, tracks.length]);

  // ---------------- focus and keyboard ----------------
  const [focusId, setFocusId] = useState<string | null>(null);
  const focusNext = useRef<string | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const indexOf = useMemo(() => new Map(tracks.map((t, i) => [t.id, i])), [tracks]);

  useLayoutEffect(() => {
    const id = focusNext.current;
    if (id === null) return;
    focusNext.current = null;
    const strip = [...(root.current?.querySelectorAll<HTMLElement>(".mx-strip[data-node]") ?? [])].find((el) => el.dataset.node === id);
    strip?.querySelector<HTMLElement>(".mx-pick")?.focus({ preventScroll: true });
  });

  // ---------------- meters ----------------
  const meterEls = useRef(new Map<string, HTMLElement>());
  useEffect(() => {
    const peaks = new Float32Array(2);
    let raf = 0;
    const draw = () => {
      for (const [id, el] of meterEls.current) {
        const has = meters?.read(id, peaks);
        const bars = el.children;
        for (let c = 0; c < 2; c++) {
          const bar = bars[c] as HTMLElement | undefined;
          if (!bar) continue;
          const v = has ? peaks[c] : 0;
          bar.style.transform = `scaleY(${barLength(v)})`;
          bar.classList.toggle("clip", v >= 1);
        }
      }
      if (meters) raf = requestAnimationFrame(draw);
    };
    raf = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(raf);
  }, [meters]);

  const [actions] = useState<StripActions>(() => ({
    command: (c) => {
      const track = latestNodes.current.get(c.type === "param" ? c.target.nodeId : c.nodeId);
      const viewing = c.type === "select" || c.type === "openInsert";
      if (!track || (track.pending && !viewing) || unsupportedReason(latest.current.unsupported, c.type) !== undefined) return;
      void latest.current.onCommand(c);
    },
    gestures,
    focused: (id) => setFocusId(id),
    meter: (id, el) => {
      if (el) meterEls.current.set(id, el);
      else meterEls.current.delete(id);
    },
    navigate: (fromId, key) => {
      const { tracks: list, master: m } = latest.current;
      const order = [...list.map((t) => t.id), m.id];
      const to = navTarget(order, fromId, key);
      if (to === null) return false;
      focusNext.current = to;
      setFocusId(to);
      const i = list.findIndex((t) => t.id === to);
      const el = scroller.current;
      if (el && i >= 0) el.scrollLeft = revealScroll(i, el.scrollLeft, el.clientWidth, PITCH);
      // The strip may be outside the drawn window until the scroll settles: draw it now.
      repaint();
      return true;
    },
  }));

  const pinned = [focusId, selectedId].map((id) => (id === null ? -1 : (indexOf.get(id) ?? -1)));
  const drawn = renderIndices(win, pinned, tracks.length);
  const selected = selectedId === null ? null : (byId.get(selectedId) ?? null);
  const tabId = focusId !== null && byId.has(focusId) ? focusId : selected ? selected.id : (tracks[0]?.id ?? master.id);
  return (
    <div className="mx" ref={root} style={{ "--mx-pitch": `${PITCH}px` } as CSSProperties}>
      <div className="mx-console">
        <div className="mx-scroll" ref={scroller} onScroll={measure} role="group" aria-label={`Mixer tracks (${tracks.length})`}>
          {tracks.length === 0 ? (
            <p className="mx-empty">No tracks yet. The master is always here.</p>
          ) : (
            <div className="mx-row" style={{ width: tracks.length * PITCH }}>
              {drawn.map((i) => {
                const t = tracks[i];
                return (
                  <TrackStrip
                    key={t.id}
                    track={t}
                    actions={actions}
                    nodes={byId}
                    unsupported={unsupported}
                    index={i + 1}
                    level={gestures.shown({ kind: "level", nodeId: t.id }) ?? t.level}
                    pan={gestures.shown({ kind: "pan", nodeId: t.id }) ?? t.pan}
                    selected={t.id === selectedId}
                    tabStop={t.id === tabId}
                    left={i * PITCH}
                  />
                );
              })}
            </div>
          )}
        </div>
        <TrackStrip
          track={master}
          actions={actions}
          masterOutput={master.outputLabel}
          nodes={byId}
          unsupported={unsupported}
          level={gestures.shown({ kind: "level", nodeId: master.id }) ?? master.level}
          pan={gestures.shown({ kind: "pan", nodeId: master.id }) ?? master.pan}
          selected={master.id === selectedId}
          tabStop={master.id === tabId}
        />
      </div>
      <Inspector
        key={selected?.id ?? ""}
        track={selected}
        master={selected?.id === master.id}
        masterOutput={master.outputLabel}
        removed={selectedId !== null && !selected}
        effects={effects}
        error={error ?? null}
        actions={actions}
        nodes={byId}
        unsupported={unsupported}
      />
    </div>
  );
}
