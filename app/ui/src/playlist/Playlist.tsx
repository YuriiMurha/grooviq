import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type DragEvent, type KeyboardEvent, type PointerEvent } from "react";
import { pickSound } from "../rack/audioFiles";
import { errorText } from "../api/project";
import { dropNotice, missingAudioAssets } from "./audio/media";
import { createWaveformLoader, packWaveforms, visibleWaveforms } from "./audio/waveforms";
import { AudioEditGesture, type AudioEditInput } from "./audio/editGesture";
import { AutomationPointEditor } from "./automation/AutomationPointEditor";
import { selectedAutomationTrack } from "./automation/creation";
import { useAutomationCreation } from "./automation/creationContext";
import { AutomationEditGesture, type AutomationEditInput } from "./automation/editGesture";
import { useAutomationCurve } from "./automation/useAutomationCurve";
import { AudioClipInspector } from "./audio/AudioClipInspector";
import { useAudioDrop } from "./audio/useAudioDrop";
import { arrangementWaveform, type LoopRegion, type ArrangementStamp } from "../api/arrangement";
import type { PatternsView } from "../api/patterns";
import { PATTERN_DRAG_TYPE } from "../patterns/PickerRail";
import { ListPicker } from "../layout/ListPicker";
import { isProjectReplacing, projectGeneration } from "../project/replacement";
import { registerGestureCancellation } from "../project/pending";
import { modalIsOpen } from "../project/shortcuts";
import { localPoint, snapTick, tickAtX, type PatternPreview, type PlaylistView, type Point, type Rect } from "./geometry";
import { PatternGesture, type GestureGuard, type GestureInput } from "./gesture";
import { createPlaylistRenderer, readPlaylistPalette, type PlaylistRenderer, type PlaylistScene } from "./renderer";
import type { PlaylistModel } from "./usePlaylist";
import { extent, hitClip, projectClips, viewport, type RenderClip } from "./viewport";
import { ClipGroupGesture, crossedThreshold, marqueeRect, marqueeSelection, toggleSelection, togglesSelection, type ClipGroupInput, type GroupPreview } from "./groupGesture";
import { buildClipAction, selectedClips, selectionIsMuted, type ClipAction } from "./clipActions";
import { keyboardLoop, keyboardSeek, seekTarget, LoopGesture, rulerTick } from "./ruler";
import "./playlist.css";

type Props = Readonly<{ rulerPosition: { readonly current: number | null }; model: PlaylistModel; patterns: PatternsView | null; bpm: number; beats: number; unit: number; notifyAudioDrop: (message: string) => void }>;
type Active = { gesture: PatternGesture | AudioEditGesture | AutomationEditGesture | ClipGroupGesture; start: Point; moved: boolean; clickId: number | null; pointerId: number; stamp: ArrangementStamp; kind: GestureInput["kind"] };
const DEFAULT_VIEW: PlaylistView = { width: 1, height: 1, scrollTick: 0, scrollTrack: 0, tickWidth: 0.05, rowHeight: 64 };

export function Playlist({ model, patterns, bpm, beats, unit, rulerPosition, notifyAudioDrop }: Props) {
  const { state, mutate, reload, seek } = model;
  const snapshot = state.snapshot;
  const barTicks = Math.max(1, Math.floor(3840 / Math.max(1, unit))) * Math.max(1, beats);
  const quantum = Math.max(1, Math.round(barTicks / 4));
  const [pixelsPerBar, setPixelsPerBar] = useState(192);
  const [view, setView] = useState<PlaylistView>(DEFAULT_VIEW);
  const [tool, setTool] = useState<"draw" | "select" | "slice">("draw");
  const [selectedState, setSelected] = useState<{ generation: number; ids: number[] } | null>(null);
  const liveIds = useMemo(() => new Set(snapshot?.clips.map(clip => clip.id)), [snapshot]);
  // Only live IDs the user selected (or the copies a duplicate just made) are ever selected.
  // Never fall back to other clips: Delete acts on this selection.
  const selectedIds = useMemo(() => selectedState && selectedState.generation === snapshot?.generation
    ? selectedState.ids.filter(id => liveIds.has(id)) : [], [selectedState, snapshot, liveIds]);
  const selection = useMemo(() => new Set(selectedIds), [selectedIds]);
  const actionClips = useMemo(() => selectedClips(snapshot?.clips ?? [], selection), [snapshot, selection]);
  const setAutomationSelection = useAutomationCreation()?.setSelection;
  useLayoutEffect(() => {
    if (!setAutomationSelection || !snapshot) return;
    setAutomationSelection({
      stamp: snapshot,
      track: selectedAutomationTrack(actionClips, snapshot.tracks),
      selectCreated: (generation, clip) => setSelected({ generation, ids: [clip] }),
    });
    return () => setAutomationSelection(null);
  }, [actionClips, setAutomationSelection, snapshot]);
  const unmuteSelection = selectionIsMuted(actionClips);
  const clipLabel = selectedIds.length > 1 ? "clips" : "clip";
  const selected = selectedIds.length === 1 ? selectedIds[0] : null;
  const marquee = useRef<{ pointerId: number; start: Point; clientStart: Point; moved: boolean; stamp: ArrangementStamp; replacements: number } | null>(null);
  const [selectionRect, setSelectionRect] = useState<Rect | null>(null);
  const [groupPreview, setGroupPreview] = useState<GroupPreview | null>(null);
  const [capturedMembers, setCapturedMembers] = useState<ReadonlySet<number> | null>(null);
  const previewMembers = capturedMembers ?? selection;
  const [preview, setPreview] = useState<RenderClip | null>(null);
  const [loopPreview, setLoopPreview] = useState<LoopRegion | null>(null);
  const ruler = useRef<HTMLDivElement>(null);
  const loopActive = useRef<{ gesture: LoopGesture; pointerId: number; stamp: ArrangementStamp } | null>(null);
  const seekRuler = useRef<HTMLDivElement>(null);
  const seekActive = useRef<GestureGuard | null>(null);
  const seekPreview = useRef<number | null>(null);
  const seekPlayhead = useRef<HTMLDivElement>(null);
  const [previewIndex, setPreviewIndex] = useState(-1);
  const [sliceGuide, setSliceGuide] = useState<{ tick: number; trackIndex: number } | null>(null);
  const [rendererKind, setRendererKind] = useState<string>("Loading display");
  const dropPreview = useRef<RenderClip | null>(null);
  const [relinkError, setRelinkError] = useState<{ generation: number; message: string } | null>(null);
  const [displayError, setDisplayError] = useState<string | null>(null);
  const canvas = useRef<HTMLCanvasElement>(null), scroll = useRef<HTMLDivElement>(null);
  const restoreCancelFocus = useRef(false);
  useEffect(() => {
    if (state.media !== null || !restoreCancelFocus.current) return;
    restoreCancelFocus.current = false;
    if (document.activeElement === document.body && !isProjectReplacing() && !modalIsOpen()) canvas.current?.focus();
  }, [state.media]);
  const renderer = useRef<PlaylistRenderer | null>(null), active = useRef<Active | null>(null);
  const draw = useRef<() => void>(() => {});
  const projection = useMemo(() => {
    if (!snapshot) return { clips: [] as RenderClip[], error: null };
    try { return { clips: projectClips(snapshot, bpm), error: null }; }
    catch (error) { return { clips: [] as RenderClip[], error: String(error) }; }
  }, [snapshot, bpm]);
  const clips = projection.clips;
  const clipSummaries = useMemo(() => new Map(snapshot?.clips.map(clip => [clip.id, clip])), [snapshot]);
  const audioAssets = useMemo(() => new Map(snapshot?.audioAssets.map(asset => [asset.id, asset])), [snapshot]);
  const selectedClip = selected === null ? null : clipSummaries.get(selected);
  const selectedAutomation = selectedClip?.content.type === "automation" ? { ...selectedClip, content: selectedClip.content } : null;
  const automationCurve = useAutomationCurve(snapshot, selectedAutomation?.id ?? null, selectedAutomation?.content.pointCount ?? null);
  const selectedPattern = selectedClip?.content.type === "pattern" ? selectedClip : null;
  const missingAssets = useMemo(() => missingAudioAssets(snapshot?.warnings ?? []), [snapshot]);
  const selectedAudio = selectedClip?.content.type === "audio" ? { ...selectedClip, content: selectedClip.content } : null;
  const selectedAsset = selectedAudio ? audioAssets.get(selectedAudio.content.asset) : null;
  const missingAsset = selectedAsset && missingAssets.has(selectedAsset.id) ? selectedAsset.id : null;
  const selectedIndex = useMemo(() => selected === null ? -1 : clips.findIndex(clip => clip.id === selected), [clips, selected]);
  const cancel = useCallback(() => {
    if (dropPreview.current) { dropPreview.current = null; draw.current(); }
    const loop = loopActive.current; loopActive.current = null; loop?.gesture.cancel(); setLoopPreview(null);
    if (loop && ruler.current?.hasPointerCapture(loop.pointerId)) ruler.current.releasePointerCapture(loop.pointerId);
    const seeking = seekActive.current; seekActive.current = null; seekPreview.current = null;
    if (seeking && seekRuler.current?.hasPointerCapture(seeking.pointerId)) seekRuler.current.releasePointerCapture(seeking.pointerId);
    const owned = active.current;
    const pointerId = owned?.pointerId ?? marquee.current?.pointerId;
    marquee.current = null; setSelectionRect(null); setGroupPreview(null); setCapturedMembers(null);
    active.current = null; owned?.gesture.cancel(); setPreview(null); setPreviewIndex(-1); setSliceGuide(null);
    const element = canvas.current;
    if (pointerId !== undefined && element?.hasPointerCapture(pointerId)) element.releasePointerCapture(pointerId);
  }, []);
  useEffect(() => registerGestureCancellation(cancel), [cancel]);
  useEffect(() => {
    // A project prompt must own pointer input before its buttons can receive a click.
    const observer = new MutationObserver(() => { if (modalIsOpen()) cancel(); });
    observer.observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["open"] });
    return () => observer.disconnect();
  }, [cancel]);
  useEffect(() => {
    const captured = active.current ?? marquee.current ?? loopActive.current ?? seekActive.current;
    const stale = !snapshot || isProjectReplacing() || Boolean(captured &&
      (captured.stamp.generation !== snapshot.generation || captured.stamp.revision !== snapshot.revision));
    if (!stale) return;
    const frame = requestAnimationFrame(cancel);
    return () => cancelAnimationFrame(frame);
  }, [snapshot, cancel]);
  useEffect(() => {
    const end = () => cancel(); window.addEventListener("blur", end);
    return () => { window.removeEventListener("blur", end); cancel(); };
  }, [cancel]);
  useEffect(() => {
    const element = seekPlayhead.current;
    if (!element) return;
    let frame: number;
    let previousX: number | undefined;
    const drawPlayhead = () => {
      const tick = seekPreview.current ?? rulerPosition.current;
      const x = tick === null ? -4 : (tick - view.scrollTick) * view.tickWidth;
      if (x !== previousX) {
        element.style.transform = `translateX(${x}px)`;
        previousX = x;
      }
      frame = requestAnimationFrame(drawPlayhead);
    };
    drawPlayhead();
    return () => cancelAnimationFrame(frame);
  }, [rulerPosition, view.scrollTick, view.tickWidth]);
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
  useEffect(() => { renderer.current?.setGroupSelection(capturedMembers ?? (selection.size > 1 ? selection : new Set())); }, [capturedMembers, selection]);
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
    const render = () => {
      const element = canvas.current, display = renderer.current; if (!element || !display) return;
      const dpr = Math.max(1, window.devicePixelRatio || 1);
      const width = Math.round(view.width * dpr), height = Math.round(view.height * dpr);
      if (element.width !== width) element.width = width; if (element.height !== height) element.height = height;
      const scene: PlaylistScene = { view, dpr, barTicks, selected: selectedIndex, preview: preview ? { index: previewIndex, clip: preview } : null, group: groupPreview, drop: dropPreview.current };
      display.draw(scene, readPlaylistPalette(element));
    };
    draw.current = render;
    const frame = requestAnimationFrame(render);
    return () => cancelAnimationFrame(frame);
  }, [clips, selectedIndex, preview, previewIndex, view, barTicks, groupPreview, previewMembers]);
  useEffect(() => {
    const observer = new MutationObserver(() => draw.current()); observer.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "data-theme", "style"] });
    const redraw = () => draw.current(); window.addEventListener("resize", redraw);
    return () => { observer.disconnect(); window.removeEventListener("resize", redraw); };
  }, []);
  const waveforms = useRef<ReturnType<typeof createWaveformLoader> | null>(null);
  useEffect(() => {
    const loader = createWaveformLoader(arrangementWaveform, rows => {
      renderer.current?.setWaveforms(packWaveforms(rows));
      draw.current();
    });
    waveforms.current = loader;
    return () => { loader.update(-1, new Map(), [], []); waveforms.current = null; };
  }, []);
  // The cached waveform belongs to the committed source and coordinates.
  const original = clips[previewIndex];
  const trimmedAudio = preview?.kind === "audio" && original &&
    (preview.startTick !== original.startTick || preview.lengthTicks !== original.lengthTicks) ? preview.id : null;
  const movingClips = groupPreview?.kind === "move" && (groupPreview.deltaTicks !== 0 || groupPreview.deltaTracks !== 0)
    ? capturedMembers : null;
  useEffect(() => {
    const visible = snapshot ? visibleWaveforms(snapshot, clips, view, missingAssets) : [];
    const targets = trimmedAudio === null && movingClips === null ? visible : visible.filter(({ clip }) =>
      clip.id !== null && clip.id !== trimmedAudio && !movingClips?.has(clip.id));
    waveforms.current?.update(snapshot?.generation ?? -1, state.waveformVersions, snapshot?.audioAssets ?? [], targets);
  }, [snapshot, state.waveformVersions, clips, view, missingAssets, trimmedAudio, movingClips]);
  const dimensions = useMemo(() => extent(clips, snapshot?.tracks.length ?? 0, view, barTicks), [clips, snapshot, view, barTicks]);
  const busy = state.busy || state.media !== null || state.requiresRefresh || !snapshot || view.width <= 1 || view.height <= 1 || rendererKind === "Loading display" || Boolean(displayError || projection.error);
  useAudioDrop({
    canvas, snapshot, view, quantum, busy,
    showPreview: clip => {
      const previous = dropPreview.current;
      if (previous === clip || (previous && clip && previous.startTick === clip.startTick &&
          previous.trackIndex === clip.trackIndex && previous.lengthTicks === clip.lengthTicks)) return;
      dropPreview.current = clip;
      draw.current();
    },
    report: notifyAudioDrop,
    commit: (stamp, input, skipped) => {
      setRelinkError(null);
      const replacements = projectGeneration();
      void model.importAudio(stamp, input).then(result => {
        if (replacements !== projectGeneration()) return;
        const id = result.status === "applied" ? result.reply.createdIds.at(-1) : undefined;
        if (id !== undefined) setSelected({ generation: stamp.generation, ids: [id] });
        const notice = dropNotice(result.status, skipped);
        const files = skipped === 1 ? "1 more file was skipped" : `${skipped} more files were skipped`;
        if (notice === "retry") notifyAudioDrop("The playlist couldn’t accept audio. Try dropping it again.");
        else if (notice === "imported-and-skipped") notifyAudioDrop(`Imported ${input.name}. ${files}. Drop one audio file at a time.`);
        else if (notice === "skipped-only") notifyAudioDrop(`${files}. Drop one audio file at a time.`);
      });
    },
  });
  const pickingRelink = useRef(false);
  const relink = async () => {
    if (busy || !snapshot || missingAsset === null || pickingRelink.current || isProjectReplacing() || modalIsOpen()) return;
    const replacements = projectGeneration();
    const current = () => replacements === projectGeneration() && !isProjectReplacing();
    pickingRelink.current = true;
    setRelinkError(null);
    try {
      const path = await pickSound("Find the missing audio file");
      if (path && current() && !modalIsOpen()) await model.relinkAudio(snapshot, missingAsset, path);
    } catch (error) {
      if (current()) setRelinkError({ generation: snapshot.generation, message: errorText(error) });
    } finally {
      pickingRelink.current = false;
    }
  };
  const coordinates = (event: { clientX: number; clientY: number }) => {
    const rect = canvas.current?.getBoundingClientRect(); if (!rect) throw new Error("Playlist input area is unavailable");
    const point = localPoint({ x: event.clientX, y: event.clientY }, rect, view);
    return { point, tick: tickAtX(point.x, view), row: Math.max(0, Math.floor(point.y / view.rowHeight + view.scrollTrack)) };
  };
  const showClipPreview = (gesture: PatternGesture | AudioEditGesture | AutomationEditGesture) => {
    const clip = gesture.preview;
    const renderClip: RenderClip = "content" in clip
      ? { ...clip, kind: "pattern", lengthTicks: clip.content.lengthTicks, ink: snapshot?.tracks[clip.trackIndex]?.color ?? 0 }
      : clip;
    setPreview(renderClip);
    setSliceGuide(gesture.sliceTick === null ? null : { tick: gesture.sliceTick, trackIndex: clip.trackIndex });
  };
  const begin = (gesture: Active["gesture"], input: GestureGuard & { kind: Active["kind"] }, index: number, start: Point, clickId: number | null) => {
    const element = canvas.current; if (!element) return;
    active.current = { gesture, start, clickId, moved: input.kind !== "move" && input.kind !== "copy", pointerId: input.pointerId, stamp: { ...input.stamp }, kind: input.kind };
    try { element.setPointerCapture(input.pointerId); } catch { cancel(); return; }
    if (gesture instanceof ClipGroupGesture) setCapturedMembers(gesture.memberIds);
    else { showClipPreview(gesture); setPreviewIndex(index); }
  };
  const commit = async (stamp: ArrangementStamp, operations: Parameters<typeof mutate>[1], selectCreated = true, createdCount = 1) => {
    const result = await mutate(stamp, operations, operations[0]?.kind === "setAutomationClip" ? () => !modalIsOpen() : undefined);
    if (result.status === "applied" && selectCreated) {
      const id = result.reply.createdIds.at(-1);
      if (id !== undefined) setSelected({ generation: result.reply.generation, ids: result.reply.createdIds.slice(-createdCount) });
    }
  };
  const selectedAction = async (action: ClipAction | "makeUnique") => {
    if (busy || !snapshot || isProjectReplacing() || modalIsOpen()) return;
    const operations: Parameters<typeof mutate>[1] = action === "makeUnique"
      ? selectedPattern ? [{ kind: "makeUnique", clip: selectedPattern.id }] : []
      : buildClipAction(action, actionClips, audioAssets, bpm);
    if (!operations.length) return;
    // A key action during a held preview owns that input: its later release sends nothing.
    cancel();
    const result = await mutate({ generation: snapshot.generation, revision: snapshot.revision }, operations);
    if (result.status !== "applied") return;
    if (action === "duplicate" && result.reply.createdIds.length) {
      setSelected(current => current === selectedState ? {
        generation: result.reply.generation, ids: [...result.reply.createdIds],
      } : current);
    } else if (action === "delete" && selectedClip) {
      // A single-clip delete closes its inspector (#449). A group delete keeps its IDs so
      // one Undo re-selects the restored group.
      setSelected(current => current === selectedState ? null : current);
    }
  };
  const down = (event: PointerEvent<HTMLCanvasElement>) => {
    if (event.button !== 0 || busy || !snapshot || isProjectReplacing() || modalIsOpen() ||
        (tool !== "select" && (event.ctrlKey || event.metaKey))) return;
    event.preventDefault(); cancel(); event.currentTarget.focus();
    const { point, tick, row } = coordinates(event);
    const hit = hitClip(clips, point.x, point.y, view);
    if (hit) {
      const { id, trackIndex } = hit.clip;
      if (id === null) return;
      const clip = clipSummaries.get(id);
      if (tool === "select" && togglesSelection(event)) {
        if (clip) setSelected({ generation: snapshot.generation, ids: toggleSelection(selectedIds, id) });
        return;
      }
      const keepGroup = tool !== "draw" && selection.has(id);
      const ids = keepGroup ? selectedIds : [id];
      if (!keepGroup) setSelected({ generation: snapshot.generation, ids });
      if (!clip || (tool === "slice" && ids.length > 1)) return;
      const kind = tool === "slice" ? "slice" : event.altKey ? "copy" : ids.length === 1 && selected === id && hit.edge ? hit.edge === "start" ? "resizeStart" : "resizeEnd" : "move";
      const timing = { pointerId: event.pointerId, stamp: snapshot, replacements: projectGeneration(), originTick: tick, originTrack: row, quantum, tracks: snapshot.tracks };
      const start = { x: event.clientX, y: event.clientY };
      const index = kind === "copy" ? -1 : clips.indexOf(hit.clip);
      if ((kind === "move" || kind === "copy") && (ids.length > 1 || clip.content.type !== "pattern")) {
        // Native copies append in batch order, which also determines playlist paint order.
        const members = ids.length > 1 ? clips.filter(member => member.id !== null && selection.has(member.id)) : [hit.clip];
        const input: ClipGroupInput = { ...timing, kind };
        begin(new ClipGroupGesture(input, members), input, index, start, id);
      } else if (clip.content.type === "pattern") {
        const { name, startTick, muted, content } = clip;
        const input: GestureInput = { ...timing, kind, sourceTicks: snapshot.sourcePeriods[content.pattern], clip: { id, name, startTick, muted, trackIndex, content } };
        begin(new PatternGesture(input), input, index, start, id);
      } else if (clip.content.type === "automation") {
        const geometry = { ...timing, clip: { ...hit.clip, id } };
        if (kind === "slice") {
          const input: AutomationEditInput = { ...geometry, kind };
          begin(new AutomationEditGesture(input), input, index, start, id);
        } else if ((kind === "resizeStart" || kind === "resizeEnd") && automationCurve.points) {
          const { type, target, priority, offsetTick, lengthTicks } = clip.content;
          const input: AutomationEditInput = { ...geometry, kind,
            content: { type, target, priority, offsetTick, lengthTicks, points: automationCurve.points } };
          begin(new AutomationEditGesture(input), input, index, start, id);
        }
      } else if (kind === "resizeStart" || kind === "resizeEnd" || kind === "slice") {
        const asset = audioAssets.get(clip.content.asset);
        if (!asset) return;
        const input: AudioEditInput = { ...timing, kind, clip: { ...hit.clip, id }, content: clip.content, sourceRate: asset.sourceRate, sourceFrames: asset.sourceFrames, bpm };
        begin(new AudioEditGesture(input), input, index, start, id);
      }
      return;
    }
    setSelected(null);
    if (tool === "select") {
      marquee.current = { pointerId: event.pointerId, start: point, clientStart: { x: event.clientX, y: event.clientY }, moved: false,
        stamp: { generation: snapshot.generation, revision: snapshot.revision }, replacements: projectGeneration() };
      try { event.currentTarget.setPointerCapture(event.pointerId); } catch { cancel(); }
      return;
    }
    if (tool !== "draw" || patterns?.generation !== snapshot.generation) return;
    const source = patterns.patterns.find(pattern => pattern.id === patterns.selected); if (!source) return;
    const start = snapTick(tick, quantum, 0, 38_400_000 - barTicks);
    const clip: PatternPreview = { id: null, trackIndex: Math.min(row, Math.min(255, snapshot.tracks.length)), name: source.name, startTick: start, muted: false,
      content: { type: "pattern", pattern: source.id, offsetTick: 0, lengthTicks: barTicks } };
    const input: GestureInput = { kind: "place", pointerId: event.pointerId, stamp: snapshot, replacements: projectGeneration(), originTick: tick, originTrack: row, quantum, tracks: snapshot.tracks, clip };
    begin(new PatternGesture(input), input, -1, { x: event.clientX, y: event.clientY }, null);
  };
  const move = (event: PointerEvent<HTMLCanvasElement>, released = false) => {
    const box = marquee.current;
    if (box && box.pointerId === event.pointerId) {
      if (!snapshot || isProjectReplacing() || modalIsOpen() || !event.currentTarget.hasPointerCapture(event.pointerId) || box.replacements !== projectGeneration() || box.stamp.generation !== snapshot.generation || box.stamp.revision !== snapshot.revision) { cancel(); return; }
      box.moved ||= crossedThreshold(box.clientStart, { x: event.clientX, y: event.clientY });
      if (box.moved) {
        const rect = marqueeRect(box.start, coordinates(event).point);
        if (released) setSelected({ generation: snapshot.generation, ids: marqueeSelection(clips, rect, view) });
        else setSelectionRect(rect);
      }
      return;
    }
    const owned = active.current; if (!owned || owned.pointerId !== event.pointerId) return;
    if (isProjectReplacing() || modalIsOpen() || !event.currentTarget.hasPointerCapture(owned.pointerId)) { cancel(); return; }
    if (!owned.moved && !crossedThreshold(owned.start, { x: event.clientX, y: event.clientY })) return;
    owned.moved = true;
    const { tick, row } = coordinates(event); owned.gesture.update(tick, row);
    if (owned.gesture instanceof ClipGroupGesture) setGroupPreview(owned.gesture.preview);
    else showClipPreview(owned.gesture);
  };
  const up = (event: PointerEvent<HTMLCanvasElement>) => {
    if (marquee.current?.pointerId === event.pointerId) { move(event, true); cancel(); return; }
    const owned = active.current; if (!owned || owned.pointerId !== event.pointerId || !snapshot) return;
    // Pending capture can be released before got/lostpointercapture is dispatched.
    if (isProjectReplacing() || modalIsOpen() || !event.currentTarget.hasPointerCapture(owned.pointerId)) { cancel(); return; }
    if (!owned.moved && !crossedThreshold(owned.start, { x: event.clientX, y: event.clientY })) {
      if (owned.clickId !== null) setSelected({ generation: snapshot.generation, ids: [owned.clickId] });
      cancel(); return;
    }
    const { tick, row } = coordinates(event); owned.gesture.update(tick, row);
    const batch = owned.gesture.take({ pointerId: event.pointerId, stamp: snapshot, replacements: projectGeneration() });
    const stamp = owned.stamp; cancel();
    // Slice keeps the original left fragment selected; copy/place select their new ID.
    if (batch?.length) void commit(stamp, batch, owned.kind !== "slice", owned.gesture instanceof ClipGroupGesture ? batch.length : 1);
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
    if ((event.key === "Delete" || event.key === "Backspace") && !event.shiftKey && !event.metaKey && !event.ctrlKey && !event.altKey) {
      // This focused editor owns the key even while busy: a repeated Backspace must not
      // become WebKit history navigation after the first press admitted an edit.
      event.preventDefault(); event.stopPropagation();
      if (!event.repeat) void selectedAction("delete");
      return;
    }
    if (busy || !snapshot || !selectedIds.length || isProjectReplacing() || modalIsOpen() || event.metaKey || event.ctrlKey || event.altKey || event.repeat) return;
    if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
      event.preventDefault(); cancel();
      const members = clips.filter(member => member.id !== null && selection.has(member.id));
      if (!members.length) return;
      const gesture = new ClipGroupGesture({ kind: "move", pointerId: 0, stamp: snapshot, replacements: projectGeneration(), originTick: 0, originTrack: 0, quantum, tracks: snapshot.tracks }, members);
      gesture.update(event.key === "ArrowLeft" ? -quantum : quantum, 0);
      const batch = gesture.take({ pointerId: 0, stamp: snapshot, replacements: projectGeneration() });
      if (batch?.length) void commit(snapshot, batch, false);
    }
  };
  const tickAtRuler = (event: PointerEvent<HTMLDivElement>) =>
    rulerTick(event.clientX, event.currentTarget.getBoundingClientRect(), event.currentTarget.clientWidth, view);
  const loopDown = (event: PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 || busy || !snapshot || isProjectReplacing() || modalIsOpen() || event.ctrlKey || event.metaKey) return;
    cancel(); event.currentTarget.focus();
    loopActive.current = { gesture: new LoopGesture({ pointerId: event.pointerId, stamp: snapshot, replacements: projectGeneration() }, tickAtRuler(event), quantum, event.clientX, snapshot.loopRegion?.enabled ?? true), pointerId: event.pointerId, stamp: snapshot };
    try { event.currentTarget.setPointerCapture(event.pointerId); } catch { cancel(); }
  };
  const loopMove = (event: PointerEvent<HTMLDivElement>, release = false) => {
    const owned = loopActive.current;
    if (!owned || owned.pointerId !== event.pointerId) return;
    if (busy || !snapshot || isProjectReplacing() || modalIsOpen() || !event.currentTarget.hasPointerCapture(event.pointerId)) { cancel(); return; }
    setLoopPreview(owned.gesture.update(tickAtRuler(event), event.clientX));
    if (!release) return;
    const batch = owned.gesture.take({ pointerId: event.pointerId, stamp: snapshot, replacements: projectGeneration() });
    cancel(); if (batch) void commit(owned.stamp, batch, false);
  };
  const seekDown = (event: PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 || busy || !snapshot || isProjectReplacing() || modalIsOpen() || event.ctrlKey || event.metaKey) return;
    cancel();
    event.currentTarget.focus();
    seekActive.current = { pointerId: event.pointerId, stamp: snapshot, replacements: projectGeneration() };
    try {
      event.currentTarget.setPointerCapture(event.pointerId);
    } catch {
      cancel();
      return;
    }
    seekPreview.current = seekTarget(tickAtRuler(event), quantum);
  };
  const seekMove = (event: PointerEvent<HTMLDivElement>, release = false) => {
    const owned = seekActive.current;
    if (!owned || owned.pointerId !== event.pointerId) return;
    if (busy || !snapshot || isProjectReplacing() || modalIsOpen()
      || !event.currentTarget.hasPointerCapture(event.pointerId)
      || owned.replacements !== projectGeneration()
      || owned.stamp.generation !== snapshot.generation || owned.stamp.revision !== snapshot.revision) {
      cancel();
      return;
    }
    const tick = seekTarget(tickAtRuler(event), quantum);
    seekPreview.current = tick;
    if (release) {
      cancel();
      void seek(owned.stamp, tick);
    }
  };
  const seekKey = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") { event.preventDefault(); cancel(); return; }
    if (event.shiftKey || event.ctrlKey || event.metaKey || event.altKey) return;
    const target = keyboardSeek(event.key, rulerPosition.current, quantum);
    if (target === null) return;
    event.preventDefault();
    event.stopPropagation();
    if (!busy && snapshot && !isProjectReplacing() && !modalIsOpen() && !event.repeat) {
      cancel();
      void seek(snapshot, target);
    }
  };
  const loopKey = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") { event.preventDefault(); cancel(); return; }
    if (!event.shiftKey || event.ctrlKey || event.metaKey || event.altKey) return;
    if (!["Home", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)) return;
    event.preventDefault();
    event.stopPropagation();
    if (busy || !snapshot || isProjectReplacing() || modalIsOpen() || event.repeat) return;
    const previous = snapshot.loopRegion;
    const region = keyboardLoop(event.key, previous, view.scrollTick, quantum);
    if (!region || (previous?.startTick === region.startTick && previous.endTick === region.endTick)) return;
    cancel();
    void commit(snapshot, [{ kind: "setLoop", region }], false);
  };
  const shownLoop = loopPreview ?? snapshot?.loopRegion;
  const visibleLabels = useMemo(() => {
    const labels: RenderClip[] = [];
    const single = preview;
    const include = (clip: RenderClip, offset?: GroupPreview) => {
      if (labels.length >= 1024) return;
      const startTick = clip.startTick + (offset?.deltaTicks ?? 0);
      const trackIndex = clip.trackIndex + (offset?.deltaTracks ?? 0);
      const x = (startTick - view.scrollTick) * view.tickWidth;
      const y = (trackIndex - view.scrollTrack) * view.rowHeight + 18;
      if (x < view.width && x + Math.max(1, clip.lengthTicks * view.tickWidth) > 0 && y < view.height && y + 42 > 0) {
        labels.push(offset ? { ...clip, id: offset.kind === "copy" ? null : clip.id, startTick, trackIndex } : clip);
      }
    };
    for (let index = 0; index < clips.length && labels.length < 1024; index++) {
      const clip = clips[index];
      const offset = groupPreview?.kind === "move" && clip.id !== null && previewMembers.has(clip.id) ? groupPreview : undefined;
      include(index === previewIndex && single ? single : clip, offset);
    }
    if (single && previewIndex === -1) include(single);
    if (groupPreview?.kind === "copy" && labels.length < 1024) {
      for (const clip of clips) {
        if (clip.id !== null && previewMembers.has(clip.id)) include(clip, groupPreview);
        if (labels.length >= 1024) break;
      }
    }
    return labels;
  }, [clips, preview, previewIndex, view, groupPreview, previewMembers]);
  const visibleIds = useMemo(() => new Set(visibleLabels.map(clip => clip.id)), [visibleLabels]);
  const clipOptions = useMemo(() => [
    { id: "", name: "None" },
    ...clips.filter(clip => clip.id !== null && (clip.id === selected || visibleIds.has(clip.id))).map(clip => ({
      id: String(clip.id),
      name: `${clip.name} · Track ${clip.trackIndex + 1} · ${clip.startTick} ticks`,
    })),
  ], [clips, selected, visibleIds]);
  // With a group selected, choosing None must still clear the selection.
  const pickerValue = selectedIds.length === 0 ? "" : selected?.toString() ?? null;
  const message = state.message ?? (relinkError?.generation === snapshot?.generation ? relinkError?.message : null) ?? displayError ?? projection.error;
  let status = `${snapshot?.total ?? 0} clips`;
  if (state.media === "import") status = "Importing audio…";
  else if (state.media === "relink") status = "Relinking audio…";
  else if (state.busy) status = "Updating playlist…";
  else if (state.cancelledMedia === "import") status = "Audio import cancelled";
  else if (state.cancelledMedia === "relink") status = "Audio relink cancelled";
  return <section className="playlist" aria-label="Playlist" aria-busy={state.busy || state.media !== null}>
    <header className="playlist-tools"><h2>Playlist</h2>
      <div role="group" aria-label="Playlist tool"><button type="button" aria-pressed={tool === "draw"} onClick={() => { cancel(); setTool("draw"); }}>Draw</button><button type="button" aria-pressed={tool === "select"} onClick={() => { cancel(); setTool("select"); }}>Select</button><button type="button" aria-pressed={tool === "slice"} onClick={() => { cancel(); setTool("slice"); }}>Slice</button></div>
      <button type="button" aria-pressed={snapshot?.loopRegion?.enabled ?? false} disabled={busy || !snapshot?.loopRegion} onClick={() => {
        if (!snapshot?.loopRegion || busy || isProjectReplacing() || modalIsOpen()) return;
        cancel(); void commit(snapshot, [{ kind: "setLoop", region: { ...snapshot.loopRegion, enabled: !snapshot.loopRegion.enabled } }], false);
      }}>Loop {snapshot?.loopRegion?.enabled ? "on" : "off"}</button>
      <span className="playlist-snap">Snap ¼ bar</span>
      <button type="button" disabled={busy || !actionClips.length} onClick={() => { void selectedAction("duplicate"); }}>Duplicate {clipLabel}</button>
      <label>Zoom<input type="range" aria-label="Playlist zoom" min="96" max="768" step="24" value={pixelsPerBar} onChange={event => { cancel(); setPixelsPerBar(Number(event.target.value)); }} /></label>
      <button type="button" disabled={state.busy || state.media !== null} onClick={() => { cancel(); void reload(); }}>Refresh playlist</button>
    </header>
    <div ref={seekRuler} className="playlist-ruler playlist-seek-ruler" role="group" aria-label="Playlist seek ruler" tabIndex={0} aria-disabled={busy}
      aria-description="Click or drag to seek. Home seeks to zero; arrow keys seek one snap from the live position."
      data-hint="Click or drag to set the playhead. Home seeks to zero; arrow keys move one snap."
      onPointerDown={seekDown} onPointerMove={event => seekMove(event)} onPointerUp={event => seekMove(event, true)}
      onPointerCancel={cancel} onLostPointerCapture={cancel} onKeyDown={seekKey}>
      <span className="playlist-seek-label" aria-hidden="true">Seek</span>
      <div ref={seekPlayhead} className="playlist-seek-playhead" aria-hidden="true" />
    </div>
    <div ref={ruler} className="playlist-ruler" role="group" aria-label="Playlist loop ruler" tabIndex={0} aria-disabled={busy}
      aria-description="Drag to set a loop. Shift+Home sets a loop. Hold Shift and use up or down to adjust its start, or left or right to adjust its end."
      data-hint="Drag to set a loop. Shift+Home sets a loop. With Shift held, up/down adjusts the start and left/right adjusts the end."
      onPointerDown={loopDown} onPointerMove={event => loopMove(event)} onPointerUp={event => loopMove(event, true)} onPointerCancel={cancel} onLostPointerCapture={cancel}
      onKeyDown={loopKey}>
      {shownLoop && <div className="playlist-loop" data-enabled={shownLoop.enabled} style={{ left: (shownLoop.startTick - view.scrollTick) * view.tickWidth, width: (shownLoop.endTick - shownLoop.startTick) * view.tickWidth }} /> }{Array.from({ length: Math.min(128, Math.ceil(view.width / pixelsPerBar) + 2) }, (_, index) => {
      const bar = Math.floor(view.scrollTick / barTicks) + index; return <span key={bar} style={{ left: (bar * barTicks - view.scrollTick) * view.tickWidth }}>{bar + 1}</span>;
    })}</div>
    <div ref={scroll} className="playlist-scroll"><div className="playlist-space" style={{ width: dimensions.width, height: dimensions.height }}>
      <div className="playlist-camera" style={{ left: view.scrollTick * view.tickWidth, top: view.scrollTrack * view.rowHeight, width: view.width, height: view.height }}>
        <canvas ref={canvas} tabIndex={0} role="application" aria-label="Playlist clips" aria-disabled={busy} aria-describedby="playlist-help" data-renderer={rendererKind}
          data-hint="Draw places the selected pattern. Drag moves a clip, Alt-drag copies it; select a clip to resize its edges. Slice cuts inside a clip on release. Escape cancels." onPointerDown={down} onPointerMove={event => move(event)} onPointerUp={up} onPointerCancel={cancel} onLostPointerCapture={cancel}
          onDragOver={event => { if (!busy && event.dataTransfer.types.includes(PATTERN_DRAG_TYPE)) { event.preventDefault(); event.dataTransfer.dropEffect = "copy"; } }} onDrop={drop} onKeyDown={key} />
        <div className="playlist-labels" aria-hidden="true">{selectionRect && <span className="playlist-marquee" style={{ position: "absolute", left: selectionRect.x, top: selectionRect.y, width: selectionRect.width, height: selectionRect.height, border: "1px solid var(--select)", background: "color-mix(in srgb, var(--select) 15%, transparent)" }} />}{snapshot?.tracks.map((track, index) => <span className="playlist-track-name" key={track.id} style={{ left: 4, top: (index - view.scrollTrack) * view.rowHeight + 1 }}>{track.name}</span>)}{visibleLabels.map((clip, index) => <span key={clip.id ?? `preview-${index}`} style={{ left: Math.max(0, (clip.startTick - view.scrollTick) * view.tickWidth) + 6, top: (clip.trackIndex - view.scrollTrack) * view.rowHeight + 21,
          maxWidth: Math.max(0, Math.min(clip.lengthTicks * view.tickWidth - 12, view.width)) }}>{clip.name}{clip.kind !== "pattern" ? ` · ${clip.kind}` : ""}</span>)}{sliceGuide && <div className="playlist-slice-guide" style={{ left: (sliceGuide.tick - view.scrollTick) * view.tickWidth, top: (sliceGuide.trackIndex - view.scrollTrack) * view.rowHeight + 18, height: Math.min(42, Math.max(1, view.rowHeight - 22)) }} />}</div>
      </div>
    </div></div>
    <div className="playlist-status">
      <span id="playlist-help">In Select, modifier-click toggles clips and a drag from empty space selects a box. Drag a group to move it or Alt-drag to copy it. Escape cancels.</span>
      <span aria-live="polite">{selectedIds.length} selected</span>
      <span role="status">{status}</span>
      {state.media && <button type="button" disabled={state.cancelRequested} aria-label={state.media === "import" ? "Cancel audio import" : "Cancel audio relink"} onClick={event => {
        restoreCancelFocus.current = event.detail === 0;
        void model.cancelMedia();
      }}>Cancel</button>}
    </div>
    <div className="playlist-selection">
      <span>Selected clip</span>
      <ListPicker
        label="Selected playlist clip"
        options={clipOptions}
        value={pickerValue}
        placeholder="None"
        disabled={state.busy || state.media !== null}
        onCommit={id => {
          cancel();
          setSelected(id ? { generation: snapshot?.generation ?? 0, ids: [Number(id)] } : null);
        }}
      />
      <button type="button" aria-label={`${unmuteSelection ? "Unmute" : "Mute"} selected ${clipLabel}`} disabled={busy || !actionClips.length} onClick={() => { void selectedAction("mute"); }}>{unmuteSelection ? "Unmute" : "Mute"}</button>
      <button type="button" aria-label="Make selected clip unique" disabled={busy || !selectedPattern} onClick={() => { void selectedAction("makeUnique"); }}>Make unique</button>
      {missingAsset !== null && <button type="button" disabled={busy} onClick={() => { void relink(); }}>Relink missing audio</button>}
      <button type="button" aria-label={`Delete selected ${clipLabel}`} disabled={busy || !actionClips.length} onClick={() => { void selectedAction("delete"); }}>Delete</button>
    </div>
    {snapshot && selectedAudio && selectedAsset && <AudioClipInspector key={`${snapshot.generation}:${selectedAudio.id}`}
      clip={selectedAudio} asset={selectedAsset} stamp={snapshot} disabled={busy} bpm={bpm} beats={beats} unit={unit}
      onEdit={(stamp, operation) => { cancel(); void commit(stamp, [operation], false); }} />}
    {snapshot && selectedAutomation &&
      <AutomationPointEditor key={`${snapshot.generation}:${selectedAutomation.id}`}
        clip={selectedAutomation} stamp={snapshot} curve={automationCurve} disabled={busy}
        onEdit={(stamp, operation) => { cancel(); void commit(stamp, [operation], false); }} />}
    {message && <p role="alert" className="playlist-error">{message}</p>}
    {missingAsset !== null && <p className="playlist-readonly">The audio file for this clip is missing.</p>}
    {!state.busy && state.media === null && snapshot?.total === 0 && <p className="playlist-empty">Draw a pattern or drop an audio file onto a track.</p>}
  </section>;
}
