import { useCallback, useEffect, useMemo, useRef, useState, type DragEvent, type KeyboardEvent, type PointerEvent } from "react";
import type { ArrangementStamp } from "../api/arrangement";
import type { PatternsView } from "../api/patterns";
import { PATTERN_DRAG_TYPE } from "../patterns/PickerRail";
import { isProjectReplacing, projectGeneration } from "../project/replacement";
import { registerGestureCancellation } from "../project/pending";
import { modalIsOpen } from "../project/shortcuts";
import { localPoint, snapTick, tickAtX, type PatternPreview, type PlaylistView } from "./geometry";
import { PatternGesture, type GestureInput } from "./gesture";
import { createPlaylistRenderer, readPlaylistPalette, type PlaylistRenderer, type PlaylistScene } from "./renderer";
import type { PlaylistModel } from "./usePlaylist";
import { extent, hitClip, projectClips, viewport, type RenderClip } from "./viewport";
import "./playlist.css";

type Props = Readonly<{ model: PlaylistModel; patterns: PatternsView | null; bpm: number; beats: number; unit: number }>;
type Active = { gesture: PatternGesture; pointerId: number; stamp: ArrangementStamp; index: number };
const DEFAULT_VIEW: PlaylistView = { width: 1, height: 1, scrollTick: 0, scrollTrack: 0, tickWidth: 0.05, rowHeight: 64 };

export function Playlist({ model, patterns, bpm, beats, unit }: Props) {
  const { state, mutate, reload } = model;
  const snapshot = state.snapshot;
  const barTicks = Math.max(1, Math.floor(3840 / Math.max(1, unit))) * Math.max(1, beats);
  const quantum = Math.max(1, Math.round(barTicks / 4));
  const [pixelsPerBar, setPixelsPerBar] = useState(192);
  const [view, setView] = useState<PlaylistView>(DEFAULT_VIEW);
  const [tool, setTool] = useState<"draw" | "select">("draw");
  const [selectedState, setSelected] = useState<{ generation: number; id: number } | null>(null);
  const selected = selectedState?.generation === snapshot?.generation ? selectedState?.id ?? null : null;
  const [preview, setPreview] = useState<PatternPreview | null>(null);
  const [previewIndex, setPreviewIndex] = useState(-1);
  const [rendererKind, setRendererKind] = useState<string>("Loading display");
  const [displayError, setDisplayError] = useState<string | null>(null);
  const canvas = useRef<HTMLCanvasElement>(null), scroll = useRef<HTMLDivElement>(null);
  const renderer = useRef<PlaylistRenderer | null>(null), active = useRef<Active | null>(null);
  const draw = useRef<() => void>(() => {});
  const projection = useMemo(() => {
    if (!snapshot) return { clips: [] as RenderClip[], error: null };
    try { return { clips: projectClips(snapshot, bpm), error: null }; }
    catch (error) { return { clips: [] as RenderClip[], error: String(error) }; }
  }, [snapshot, bpm]);
  const clips = projection.clips;
  const patternsOnly = useMemo(() => snapshot?.clips.flatMap(clip => {
    if (clip.content.type !== "pattern") return [];
    return [{ id: clip.id, name: clip.name, startTick: clip.startTick, muted: clip.muted, trackIndex: snapshot.tracks.findIndex(track => track.id === clip.track), content: { ...clip.content } }];
  }) ?? [], [snapshot]);
  const selectedClip = patternsOnly.find(clip => clip.id === selected) ?? null;
  const cancel = useCallback(() => {
    const owned = active.current;
    active.current = null; owned?.gesture.cancel(); setPreview(null); setPreviewIndex(-1);
    const element = canvas.current;
    if (owned && element?.hasPointerCapture(owned.pointerId)) element.releasePointerCapture(owned.pointerId);
  }, []);
  useEffect(() => registerGestureCancellation(cancel), [cancel]);
  useEffect(() => {
    // A project prompt must own pointer input before its buttons can receive a click.
    const observer = new MutationObserver(() => { if (modalIsOpen()) cancel(); });
    observer.observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["open"] });
    return () => observer.disconnect();
  }, [cancel]);
  useEffect(() => {
    const stale = !snapshot || isProjectReplacing() || Boolean(active.current &&
      (active.current.stamp.generation !== snapshot.generation || active.current.stamp.revision !== snapshot.revision));
    if (!stale) return;
    const frame = requestAnimationFrame(cancel);
    return () => cancelAnimationFrame(frame);
  }, [snapshot, cancel]);
  useEffect(() => {
    const end = () => cancel(); window.addEventListener("blur", end);
    return () => { window.removeEventListener("blur", end); cancel(); };
  }, [cancel]);
  useEffect(() => {
    const element = canvas.current; if (!element) return;
    const display = createPlaylistRenderer(element); renderer.current = display;
    if (!display) { setDisplayError("The playlist display is unavailable. Close and reopen it with F5."); return; }
    setRendererKind(display.kind); setDisplayError(null);
    const lost = (event: Event) => { event.preventDefault(); cancel(); setDisplayError("The playlist display was interrupted. Close and reopen it with F5."); };
    element.addEventListener("webglcontextlost", lost);
    return () => { element.removeEventListener("webglcontextlost", lost); display.dispose(); renderer.current = null; };
  }, [cancel]);
  useEffect(() => { renderer.current?.setClips(clips); }, [clips]);
  useEffect(() => {
    const container = scroll.current; if (!container) return;
    const measure = () => {
      if (container.clientWidth > 0 && container.clientHeight > 0) setView(viewport(container.clientWidth, container.clientHeight, container.scrollLeft, container.scrollTop, pixelsPerBar, barTicks));
    };
    const observer = new ResizeObserver(measure); observer.observe(container); measure();
    container.addEventListener("scroll", measure, { passive: true });
    return () => { observer.disconnect(); container.removeEventListener("scroll", measure); };
  }, [pixelsPerBar, barTicks]);
  useEffect(() => {
    const transformed: RenderClip | null = preview ? { id: preview.id, name: preview.name, kind: "pattern", trackIndex: preview.trackIndex,
      startTick: preview.startTick, lengthTicks: preview.content.lengthTicks, ink: snapshot?.tracks[preview.trackIndex]?.color ?? 0, muted: preview.muted } : null;
    const render = () => {
      const element = canvas.current, display = renderer.current; if (!element || !display) return;
      const dpr = Math.max(1, window.devicePixelRatio || 1);
      const width = Math.round(view.width * dpr), height = Math.round(view.height * dpr);
      if (element.width !== width) element.width = width; if (element.height !== height) element.height = height;
      const scene: PlaylistScene = { view, dpr, barTicks, selected: clips.findIndex(clip => clip.id === selected), preview: transformed ? { index: previewIndex, clip: transformed } : null };
      display.draw(scene, readPlaylistPalette(element));
    };
    draw.current = render;
    const frame = requestAnimationFrame(render);
    return () => cancelAnimationFrame(frame);
  }, [clips, selected, preview, previewIndex, snapshot, view, barTicks]);
  useEffect(() => {
    const observer = new MutationObserver(() => draw.current()); observer.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "data-theme", "style"] });
    const redraw = () => draw.current(); window.addEventListener("resize", redraw);
    return () => { observer.disconnect(); window.removeEventListener("resize", redraw); };
  }, []);
  const dimensions = extent(clips, snapshot?.tracks.length ?? 0, view, barTicks);
  const busy = state.busy || state.requiresRefresh || !snapshot || view.width <= 1 || view.height <= 1 || rendererKind === "Loading display" || Boolean(displayError || projection.error);
  const coordinates = (event: { clientX: number; clientY: number }) => {
    const rect = canvas.current?.getBoundingClientRect(); if (!rect) throw new Error("Playlist input area is unavailable");
    const point = localPoint({ x: event.clientX, y: event.clientY }, rect, view);
    return { point, tick: tickAtX(point.x, view), row: Math.max(0, Math.floor(point.y / view.rowHeight + view.scrollTrack)) };
  };
  const begin = (input: GestureInput, index: number) => {
    const element = canvas.current; if (!element) return;
    const gesture = new PatternGesture(input);
    active.current = { gesture, pointerId: input.pointerId, stamp: { ...input.stamp }, index };
    try { element.setPointerCapture(input.pointerId); } catch { cancel(); return; }
    setPreview(gesture.preview); setPreviewIndex(index);
  };
  const commit = async (stamp: ArrangementStamp, operations: Parameters<typeof mutate>[1]) => {
    const result = await mutate(stamp, operations);
    if (result.status === "applied") {
      const id = result.reply.createdIds.at(-1);
      if (id !== undefined) setSelected({ generation: result.reply.generation, id });
    }
  };
  const down = (event: PointerEvent<HTMLCanvasElement>) => {
    if (event.button !== 0 || busy || !snapshot || isProjectReplacing() || modalIsOpen() || event.ctrlKey || event.metaKey) return;
    event.preventDefault(); event.currentTarget.focus();
    const { point, tick, row } = coordinates(event);
    const hit = hitClip(clips, point.x, point.y, view);
    if (hit) {
      const id = hit.clip.id; if (id === null) return;
      setSelected({ generation: snapshot.generation, id });
      const clip = patternsOnly.find(clip => clip.id === id); if (!clip) return;
      const kind = event.altKey ? "copy" : selected === id && hit.edge ? hit.edge === "start" ? "resizeStart" : "resizeEnd" : "move";
      begin({ kind, pointerId: event.pointerId, stamp: snapshot, replacements: projectGeneration(), originTick: tick, originTrack: row, quantum, tracks: snapshot.tracks,
        sourceTicks: snapshot.sourcePeriods[clip.content.pattern], clip }, kind === "copy" ? -1 : clips.findIndex(c => c.id === id));
      return;
    }
    setSelected(null);
    if (tool !== "draw" || patterns?.generation !== snapshot.generation) return;
    const source = patterns.patterns.find(pattern => pattern.id === patterns.selected); if (!source) return;
    const start = snapTick(tick, quantum, 0, 38_400_000 - barTicks);
    const clip: PatternPreview = { id: null, trackIndex: Math.min(row, Math.min(255, snapshot.tracks.length)), name: source.name, startTick: start, muted: false,
      content: { type: "pattern", pattern: source.id, offsetTick: 0, lengthTicks: barTicks } };
    begin({ kind: "place", pointerId: event.pointerId, stamp: snapshot, replacements: projectGeneration(), originTick: tick, originTrack: row, quantum, tracks: snapshot.tracks, clip }, -1);
  };
  const move = (event: PointerEvent<HTMLCanvasElement>) => {
    const owned = active.current; if (!owned || owned.pointerId !== event.pointerId) return;
    if (isProjectReplacing() || modalIsOpen()) { cancel(); return; }
    const { tick, row } = coordinates(event); owned.gesture.update(tick, row); setPreview(owned.gesture.preview);
  };
  const up = (event: PointerEvent<HTMLCanvasElement>) => {
    const owned = active.current; if (!owned || owned.pointerId !== event.pointerId || !snapshot) return;
    if (isProjectReplacing() || modalIsOpen()) { cancel(); return; }
    const { tick, row } = coordinates(event); owned.gesture.update(tick, row);
    const batch = owned.gesture.take({ pointerId: event.pointerId, stamp: snapshot, replacements: projectGeneration() });
    const stamp = owned.stamp; cancel();
    if (batch?.length) void commit(stamp, batch);
  };
  const drop = (event: DragEvent<HTMLCanvasElement>) => {
    if (busy || !snapshot || isProjectReplacing() || modalIsOpen()) return;
    const raw = event.dataTransfer.getData(PATTERN_DRAG_TYPE); if (!raw || raw.length > 512) return;
    let payload: unknown; try { payload = JSON.parse(raw); } catch { return; }
    if (!payload || typeof payload !== "object" || !("generation" in payload) || !("pattern" in payload) || payload.generation !== snapshot.generation) return;
    const source = patterns?.patterns.find(pattern => pattern.id === payload.pattern); if (!source || patterns?.generation !== snapshot.generation) return;
    event.preventDefault();
    const { tick, row } = coordinates(event); const start = snapTick(tick, quantum, 0, 38_400_000 - barTicks);
    const gesture = new PatternGesture({ kind: "place", pointerId: 0, stamp: snapshot, replacements: projectGeneration(), originTick: start, originTrack: row, quantum, tracks: snapshot.tracks,
      clip: { id: null, name: source.name, trackIndex: Math.min(row, Math.min(255, snapshot.tracks.length)), startTick: start, muted: false, content: { type: "pattern", pattern: source.id, offsetTick: 0, lengthTicks: barTicks } } });
    const batch = gesture.take({ pointerId: 0, stamp: snapshot, replacements: projectGeneration() }); if (batch) void commit(snapshot, batch);
  };
  const key = (event: KeyboardEvent<HTMLCanvasElement>) => {
    if (event.key === "Escape") { event.preventDefault(); cancel(); return; }
    if (busy || !snapshot || !selectedClip || isProjectReplacing() || modalIsOpen() || event.metaKey || event.ctrlKey || event.altKey || event.repeat) return;
    if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
      event.preventDefault(); const direction = event.key === "ArrowLeft" ? -quantum : quantum;
      const start = Math.max(0, Math.min(38_400_000 - selectedClip.content.lengthTicks, selectedClip.startTick + direction));
      if (start !== selectedClip.startTick) void commit(snapshot, [{ kind: "move", clip: selectedClip.id, track: snapshot.tracks[selectedClip.trackIndex].id, startTick: start }]);
    }
  };
  const labelPreview: RenderClip | null = preview ? { id: preview.id, name: preview.name, kind: "pattern", trackIndex: preview.trackIndex, startTick: preview.startTick, lengthTicks: preview.content.lengthTicks, ink: 0, muted: preview.muted } : null;
  const displayClips = clips.map((clip, index) => previewIndex === index && labelPreview ? labelPreview : clip);
  if (labelPreview && previewIndex === -1) displayClips.push(labelPreview);
  const visibleLabels = displayClips.filter(clip => (clip.startTick - view.scrollTick) * view.tickWidth < view.width && (clip.startTick + clip.lengthTicks - view.scrollTick) * view.tickWidth > 0 && (clip.trackIndex - view.scrollTrack) * 64 > -64 && (clip.trackIndex - view.scrollTrack) * 64 < view.height).slice(0, 1024);
  return <section className="playlist" aria-label="Playlist" aria-busy={state.busy}>
    <header className="playlist-tools"><h2>Playlist</h2>
      <div role="group" aria-label="Playlist tool"><button type="button" aria-pressed={tool === "draw"} onClick={() => { cancel(); setTool("draw"); }}>Draw</button><button type="button" aria-pressed={tool === "select"} onClick={() => { cancel(); setTool("select"); }}>Select</button></div>
      <span className="playlist-snap">Snap ¼ bar</span>
      <button type="button" disabled={busy || !selectedClip} onClick={() => { if (snapshot && selectedClip) void commit(snapshot, [{ kind: "copy", clip: selectedClip.id, track: snapshot.tracks[selectedClip.trackIndex].id, startTick: Math.min(38_400_000 - selectedClip.content.lengthTicks, selectedClip.startTick + selectedClip.content.lengthTicks) }]); }}>Duplicate clip</button>
      <label>Zoom<input type="range" aria-label="Playlist zoom" min="96" max="768" step="24" value={pixelsPerBar} onChange={event => { cancel(); setPixelsPerBar(Number(event.target.value)); }} /></label>
      <button type="button" disabled={state.busy} onClick={() => { cancel(); void reload(); }}>Refresh playlist</button>
    </header>
    <div className="playlist-ruler" aria-hidden="true">{Array.from({ length: Math.min(128, Math.ceil(view.width / pixelsPerBar) + 2) }, (_, index) => {
      const bar = Math.floor(view.scrollTick / barTicks) + index; return <span key={bar} style={{ left: (bar * barTicks - view.scrollTick) * view.tickWidth }}>{bar + 1}</span>;
    })}</div>
    <div ref={scroll} className="playlist-scroll"><div className="playlist-space" style={{ width: dimensions.width, height: dimensions.height }}>
      <div className="playlist-camera" style={{ left: view.scrollTick * view.tickWidth, top: view.scrollTrack * view.rowHeight, width: view.width, height: view.height }}>
        <canvas ref={canvas} tabIndex={0} role="application" aria-label="Playlist clips" aria-disabled={busy} aria-describedby="playlist-help" data-renderer={rendererKind}
          data-hint="Draw places the selected pattern. Drag moves a clip, Alt-drag copies it; select a clip to resize its edges. Escape cancels." onPointerDown={down} onPointerMove={move} onPointerUp={up} onPointerCancel={cancel} onLostPointerCapture={cancel}
          onDragOver={event => { if (!busy && event.dataTransfer.types.includes(PATTERN_DRAG_TYPE)) { event.preventDefault(); event.dataTransfer.dropEffect = "copy"; } }} onDrop={drop} onKeyDown={key} />
        <div className="playlist-labels" aria-hidden="true">{snapshot?.tracks.map((track, index) => <span className="playlist-track-name" key={track.id} style={{ left: 4, top: (index - view.scrollTrack) * view.rowHeight + 1 }}>{track.name}</span>)}{visibleLabels.map(clip => <span key={clip.id} style={{ left: Math.max(0, (clip.startTick - view.scrollTick) * view.tickWidth) + 6, top: (clip.trackIndex - view.scrollTrack) * view.rowHeight + 21,
          maxWidth: Math.max(0, Math.min(clip.lengthTicks * view.tickWidth - 12, view.width)) }}>{clip.name}{clip.kind !== "pattern" ? ` · ${clip.kind}` : ""}</span>)}</div>
      </div>
    </div></div>
    <div className="playlist-status"><span id="playlist-help">Drag patterns here. F5 closes the playlist; Escape cancels a drag.</span><span role="status">{state.busy ? "Updating playlist…" : `${snapshot?.total ?? 0} clips`}</span></div>
    <label className="playlist-selection">Selected clip<select aria-label="Selected playlist clip" value={selected ?? ""} disabled={state.busy} onChange={event => { cancel(); setSelected(event.target.value ? { generation: snapshot?.generation ?? 0, id: Number(event.target.value) } : null); }}><option value="">None</option>{clips.filter(clip => clip.id === selected || visibleLabels.some(visible => visible.id === clip.id)).map(clip => <option key={clip.id} value={clip.id ?? ""}>{clip.name} · Track {clip.trackIndex + 1} · {clip.startTick} ticks</option>)}</select></label>
    {(state.message || displayError || projection.error) && <p role="alert" className="playlist-error">{state.message ?? displayError ?? projection.error}</p>}
    {selected !== null && !selectedClip && <p className="playlist-readonly">This clip is visible here; its editing controls are not available yet.</p>}
    {!state.busy && snapshot?.total === 0 && <p className="playlist-empty">Choose a pattern in the rail, then draw it on the timeline.</p>}
  </section>;
}
