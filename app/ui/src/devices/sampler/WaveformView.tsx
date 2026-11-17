import { useEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";
import { drawnScale, useBoxSize, useLatest } from "../controls/gesture";
import { useDeviceScale } from "../deviceScale";
import type { Gesture } from "../paramGestures";
import { dragMarker, markerAt, markerPosition, markerRange, nudgeMarker, peaksToPoints, type Marker, type Region } from "./waveform";

/** One arrow key press moves a region marker this much of the sample (Shift: a fifth of it). */
const KEY_STEP = 0.001;
/** ... and the loop marker this much of the region. */
const LOOP_KEY_STEP = 0.01;
const PAGE = 10;
const FINE = 0.2;

export interface MarkerInfo {
  id: Marker;
  /** The accessible name ("Region start"). */
  name: string;
  /** What it does (one sentence). */
  does: string;
  format: (value: number) => string;
  default: number;
}

interface Props {
  /** The channel's name, for the accessible names. */
  name: string;
  /** `null`: there is no sample to draw (the markers still work). */
  peaks: Float32Array | null;
  region: Region;
  /** The loop is on: its marker and the looped tail are drawn bright. */
  loop: boolean;
  markers: Record<Marker, MarkerInfo>;
  /** A gesture of the editor's owner on a marker, and a one-shot change (a key). */
  begin: (id: Marker) => Gesture;
  set: (id: Marker, value: number) => void;
  /** Drawn over the waveform (what to do when there is no sample). */
  children?: ReactNode;
}

const NO_PEAKS = new Float32Array(0);
const regionValue = (r: Region, m: Marker) => (m === "loop_start" ? r.loopStart : r[m]);

/** The sample's waveform in a canvas, drawn again (from the peaks it has) whenever its drawn
 * size or the region changes, with the region's start, end and loop markers as handles to drag
 * or move with the keys. Outside the played region the sample is dimmed. */
export function WaveformView({ name, peaks, region, loop, markers, begin, set, children }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const { w, h } = useBoxSize(host);
  // Drawn at the scale the window is shown at (it scales itself to fit the app window).
  const scale = useDeviceScale() * (window.devicePixelRatio || 1);
  const [dragging, setDragging] = useState<Marker | null>(null);
  const drag = useRef<{ id: Marker; offset: number; scale: number; moved: boolean; last: number; gesture: Gesture } | null>(null);
  const latest = useLatest(region);

  const lo = Math.min(region.start, region.end);
  const hi = Math.max(region.start, region.end);
  const loopAt = markerPosition("loop_start", region);
  useEffect(() => {
    const c = canvas.current;
    const el = host.current;
    if (!c || !el || w <= 0 || h <= 0) return;
    const W = Math.max(1, Math.round(w * scale));
    const H = Math.max(1, Math.round(h * scale));
    if (c.width !== W) c.width = W;
    if (c.height !== H) c.height = H;
    const g = c.getContext("2d");
    if (!g) return;
    const style = getComputedStyle(el);
    const ink = style.color;
    const text = style.getPropertyValue("--text").trim() || "#fff";
    g.clearRect(0, 0, W, H);

    const pad = Math.round(6 * scale);
    const body = H - 2 * pad;
    const pts = peaksToPoints(peaks ?? NO_PEAKS, W, body);
    const outline = () => {
      g.beginPath();
      for (let i = 0; i < pts.length; i += 2) {
        if (i === 0) g.moveTo(pts[0], pad + pts[1]);
        else g.lineTo(pts[i], pad + pts[i + 1]);
      }
      g.closePath();
    };
    // The centre line, under a sample.
    g.fillStyle = ink;
    g.globalAlpha = 0.25;
    if (peaks) g.fillRect(0, Math.round(H / 2), W, Math.max(1, Math.round(scale)));
    // Dimmed everywhere, bright inside the played region.
    const x0 = lo * W;
    const x1 = hi * W;
    g.globalAlpha = 0.28;
    outline();
    g.fill();
    g.save();
    g.beginPath();
    g.rect(x0, 0, Math.max(1, x1 - x0), H);
    g.clip();
    g.globalAlpha = 0.9;
    outline();
    g.fill();
    g.lineWidth = Math.max(1, scale);
    g.strokeStyle = ink;
    g.stroke();
    g.restore();
    // The looped tail, from the loop marker to the end it plays towards.
    if (loop) {
      const a = loopAt * W;
      const b = region.reverse ? x0 : x1;
      g.globalAlpha = 0.12;
      g.fillStyle = ink;
      g.fillRect(Math.min(a, b), 0, Math.abs(b - a), H);
    }
    // The markers' lines.
    g.globalAlpha = 1;
    g.fillStyle = text;
    const line = Math.max(1, Math.round(1.5 * scale));
    for (const x of [region.start * W, region.end * W]) g.fillRect(Math.round(x - line / 2), 0, line, H);
    g.fillStyle = ink;
    g.globalAlpha = loop ? 1 : 0.45;
    const dash = Math.round(4 * scale);
    for (let y = 0; y < H; y += 2 * dash) g.fillRect(Math.round(loopAt * W - line / 2), y, line, dash);
    g.globalAlpha = 1;
  }, [peaks, w, h, scale, lo, hi, loopAt, loop, region.start, region.end, region.reverse]);

  const local = (e: PointerEvent<HTMLElement>) => {
    const r = host.current!.getBoundingClientRect();
    const s = drawnScale(host.current!);
    return { x: (e.clientX - r.left) / s, s };
  };
  const stop = () => {
    const d = drag.current;
    drag.current = null;
    setDragging(null);
    return d;
  };
  const onDown = (id: Marker) => (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.currentTarget.focus({ preventScroll: true });
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* no pointer capture here */
    }
    const { x, s } = local(e);
    drag.current?.gesture.cancel();
    const value = regionValue(region, id);
    // Where the pointer grabbed the marker, from its line, so a grab never moves it.
    drag.current = { id, offset: x - markerPosition(id, region) * w, scale: s, moved: false, last: value, gesture: begin(id) };
    setDragging(id);
  };
  const onMove = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d) return;
    const { x } = local(e);
    const v = dragMarker(d.id, markerAt(x - d.offset, w), latest.current);
    if (v === d.last) return;
    d.last = v;
    d.moved = true;
    // The editor ended it (Escape, focus lost): stop following the pointer.
    if (!d.gesture.input(v)) stop();
  };
  const onUp = () => {
    const d = stop();
    if (!d) return;
    if (d.moved) d.gesture.commit(d.last);
    else d.gesture.cancel();
  };
  const onCancel = () => stop()?.gesture.cancel();

  const onKeyDown = (id: Marker) => (e: KeyboardEvent<HTMLDivElement>) => {
    const value = regionValue(region, id);
    const step = (id === "loop_start" ? LOOP_KEY_STEP : KEY_STEP) * (e.shiftKey ? FINE : 1);
    const dir = { ArrowUp: 1, ArrowRight: 1, ArrowDown: -1, ArrowLeft: -1, PageUp: PAGE, PageDown: -PAGE, Home: -Infinity, End: Infinity }[e.key];
    if (dir === undefined) return;
    e.preventDefault();
    const next = nudgeMarker(id, region, value, dir * step);
    if (next !== value) set(id, next);
  };

  const handle = (id: Marker) => {
    const m = markers[id];
    const value = regionValue(region, id);
    // No place leaves a frame: the marker stays put until the other one moves.
    const range = markerRange(id, region);
    const [min, max] = range ?? [value, value];
    const off = id === "loop_start" && !loop;
    return (
      <div
        key={id}
        className={`wmark ${id === "loop_start" ? "loop" : "edge"}${off ? " off" : ""}${dragging === id ? " dragging" : ""}`}
        style={{ "--x": `${(markerPosition(id, region) * 100).toFixed(4)}%` } as CSSProperties}
        role="slider"
        tabIndex={0}
        aria-label={`${name} ${m.name.toLowerCase()}`}
        aria-orientation="horizontal"
        aria-valuemin={Number(min.toFixed(4))}
        aria-valuemax={Number(max.toFixed(4))}
        aria-valuenow={Number(value.toFixed(4))}
        aria-valuetext={m.format(value)}
        data-param={id}
        data-hint={`${m.does} Drag it along the waveform (it stops at the other markers), or use the arrow keys; double-click resets it.`}
        onPointerDown={onDown(id)}
        onPointerMove={onMove}
        onPointerUp={onUp}
        onPointerCancel={onCancel}
        onLostPointerCapture={onCancel}
        onKeyDown={onKeyDown(id)}
        onDoubleClick={() => {
          if (!range) return;
          const v = Math.min(max, Math.max(min, m.default));
          if (v !== value) set(id, v);
        }}
      >
        <span className="wmark-tab">{id === "start" ? "S" : id === "end" ? "E" : "L"}</span>
      </div>
    );
  };

  return (
    <div ref={host} className={`dwell sample-wave${dragging ? " dragging" : ""}`}>
      {peaks ? (
        <canvas
          ref={canvas}
          role="img"
          aria-label={`${name} waveform`}
          data-hint={`The sample ${name} plays. Drag the S and E markers to set the region, L for where the loop begins.`}
        />
      ) : (
        <canvas ref={canvas} aria-hidden="true" />
      )}
      {handle("start")}
      {handle("end")}
      {handle("loop_start")}
      {children}
    </div>
  );
}
