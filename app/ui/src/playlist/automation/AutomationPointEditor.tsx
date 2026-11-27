import { useCallback, useEffect, useId, useMemo, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";
import type { ArrangementOperation, ArrangementStamp, ClipSummary, CurvePoint } from "../../api/arrangement";
import { registerGestureCancellation } from "../../project/pending";
import { isProjectReplacing, projectGeneration } from "../../project/replacement";
import { modalIsOpen } from "../../project/shortcuts";
import { addPoint, clippedCurve, evaluate, freeTick, movePoint, pointAt, pointPosition, previewSource, windowPoints } from "./curve";
import { pointTarget } from "./target";
import type { AutomationCurve } from "./useAutomationCurve";
import "./automation.css";

type CurveContent = Extract<ClipSummary["content"], { type: "automation" }>;
type Props = {
  clip: { id: number; name: string; content: CurveContent };
  stamp: ArrangementStamp;
  curve: AutomationCurve;
  disabled: boolean;
  onEdit: (stamp: ArrangementStamp, operation: Extract<ArrangementOperation, { kind: "setAutomationPoints" }>) => void;
};
type Drag = {
  id: number;
  host: HTMLDivElement;
  revision: number;
  replacements: number;
  index: number;
  startX: number;
  startY: number;
  width: number;
  height: number;
  moved: boolean;
};

export function AutomationPointEditor({ clip, stamp, curve, disabled, onEdit }: Props) {
  const { generation, revision } = stamp;
  const { id, content } = clip;
  const { offsetTick, lengthTicks } = content;
  const windowRange = useMemo(() => ({ start: offsetTick, end: offsetTick + lengthTicks }), [offsetTick, lengthTicks]);
  const target = useMemo(() => pointTarget(content.target), [content.target]);
  const helpId = useId();

  const source = curve.points;
  const points = useMemo(() => source ? windowPoints(source, windowRange) : [], [source, windowRange]);
  const [selection, setSelection] = useState(0);
  const index = Math.min(selection, points.length - 1);
  const graph = useRef<HTMLDivElement>(null);
  const drag = useRef<Drag | null>(null);
  const [preview, setPreview] = useState<{ revision: number; points: CurvePoint[] } | null>(null);
  const unavailable = disabled || !source || !target;
  const blocked = () => unavailable || isProjectReplacing() || modalIsOpen();
  const visiblePreview = !unavailable && preview?.revision === revision ? preview.points : null;
  const visiblePoints = visiblePreview ?? points;
  const selected = visiblePoints[index];
  const path = useMemo(() => {
    if (!source || !target) return "";
    const shown = visiblePreview ? previewSource(source, visiblePreview, windowRange) : source;
    return clippedCurve(shown, windowRange).map((point, index) => {
      const { x, y } = pointPosition(point, windowRange, target);
      return `${index === 0 ? "M" : "L"}${x * 100} ${y * 100}`;
    }).join(" ");
  }, [source, target, visiblePreview, windowRange]);

  const cancel = useCallback(() => {
    const held = drag.current;
    if (!held) return;
    drag.current = null;
    if (held.host.hasPointerCapture(held.id)) held.host.releasePointerCapture(held.id);
    setPreview(null);
  }, []);
  useEffect(() => {
    const unregister = registerGestureCancellation(cancel);
    const observer = new MutationObserver(() => { if (modalIsOpen()) cancel(); });
    observer.observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["open", "aria-modal"] });
    const escape = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape" || !drag.current || modalIsOpen()) return;
      event.preventDefault();
      event.stopPropagation();
      cancel();
    };
    window.addEventListener("keydown", escape, true);
    window.addEventListener("blur", cancel);
    return () => {
      unregister();
      observer.disconnect();
      window.removeEventListener("keydown", escape, true);
      window.removeEventListener("blur", cancel);
      cancel();
    };
  }, [cancel]);
  useEffect(() => () => cancel(), [cancel, revision, unavailable]);

  const submit = (next: CurvePoint[] | null) => {
    if (!next || blocked() || drag.current) return;
    // The graph survives the stamped reload even when its individual handles disappear.
    if (graph.current?.contains(document.activeElement)) graph.current.focus({ preventScroll: true });
    onEdit({ generation, revision }, { kind: "setAutomationPoints", clip: id, points: next });
  };
  const insert = (point: { tick: number; value: number }) => {
    if (!source || blocked() || drag.current) return;
    const next = addPoint(points, source, point);
    if (!next) return;
    setSelection(next.findIndex(candidate => candidate.tick === point.tick));
    submit(next);
  };
  const remove = (pointIndex: number) => {
    if (!points[pointIndex] || blocked() || drag.current) return;
    setSelection(Math.min(pointIndex, Math.max(0, points.length - 2)));
    submit(points.filter((_, candidate) => candidate !== pointIndex));
  };

  const startDrag = (event: PointerEvent<HTMLButtonElement>, pointIndex: number) => {
    const host = graph.current;
    if (!host || blocked() || drag.current || !event.isPrimary || event.button !== 0 || event.ctrlKey || event.metaKey) return;
    event.preventDefault();
    const { width, height } = host.getBoundingClientRect();
    if (!width || !height) return;
    host.focus({ preventScroll: true });
    host.setPointerCapture(event.pointerId);
    setSelection(pointIndex);
    drag.current = { id: event.pointerId, host, revision, replacements: projectGeneration(), index: pointIndex,
      startX: event.clientX, startY: event.clientY, width, height, moved: false };
  };
  const updateDrag = (event: PointerEvent<HTMLDivElement>, release: boolean) => {
    const held = drag.current;
    if (held?.id !== event.pointerId) return;
    if (held.revision !== revision || held.replacements !== projectGeneration() || blocked() ||
        !held.host.hasPointerCapture(held.id) || !target) { cancel(); return; }
    const offsetX = event.clientX - held.startX, offsetY = event.clientY - held.startY;
    held.moved ||= Math.hypot(offsetX, offsetY) >= 3;
    const base = points[held.index];
    const position = pointPosition(base, windowRange, target);
    const destination = pointAt(position.x + offsetX / held.width, position.y + offsetY / held.height, windowRange, target);
    if (offsetY === 0) destination.value = base.value;
    const next = held.moved ? movePoint(points, held.index, destination, windowRange) : null;
    if (release) {
      cancel();
      submit(next);
    } else {
      setPreview(next ? { revision, points: next } : null);
    }
  };
  const cancelPointer = (event: PointerEvent<HTMLDivElement>) => {
    if (drag.current?.id === event.pointerId) cancel();
  };
  const graphKey = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    const key = event.key;
    if (!["Home", "End", "PageUp", "PageDown", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Delete", "Backspace"].includes(key)) return;
    event.preventDefault();
    event.stopPropagation();
    if (!selected || !target || blocked() || drag.current) return;
    if (key === "Home" || key === "End" || key === "PageUp" || key === "PageDown") {
      if (key === "Home") setSelection(0);
      if (key === "End") setSelection(points.length - 1);
      if (key === "PageUp") setSelection(Math.max(0, index - 1));
      if (key === "PageDown") setSelection(Math.min(points.length - 1, index + 1));
      graph.current?.focus({ preventScroll: true });
      return;
    }
    if (event.repeat) return;
    if (key === "Delete" || key === "Backspace") { remove(index); return; }
    const position = pointPosition(selected, windowRange, target);
    const tickStep = (event.shiftKey ? 10 : 1) / lengthTicks;
    const valueStep = target.stepped ? 1 / (target.max - target.min) : event.shiftKey ? 0.1 : 0.01;
    const nextX = position.x + (key === "ArrowLeft" ? -tickStep : key === "ArrowRight" ? tickStep : 0);
    const nextY = position.y + (key === "ArrowUp" ? -valueStep : key === "ArrowDown" ? valueStep : 0);
    const next = pointAt(nextX, nextY, windowRange, target);
    // Existing stepped curves can contain fractional values; horizontal edits keep them.
    if (nextY === position.y) next.value = selected.value;
    submit(movePoint(points, index, next, windowRange));
  };
  const insertTick = useMemo(() => freeTick(points, windowRange), [points, windowRange]);
  const stopSpace = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key === " " && event.target !== graph.current) event.stopPropagation();
  };

  return <section className="automation-dock" aria-label="Automation point editor" onKeyDown={stopSpace} onKeyUp={stopSpace}>
    <header className="automation-dock-header">
      <div className="automation-identity"><h3>{clip.name}</h3>
        {target && <span>{target.name} · Channel {content.target.channel}</span>}</div>
      <div className="automation-actions">
        <button type="button" aria-disabled={unavailable || insertTick === null} onClick={() => {
          if (insertTick === null || !source || !target) return;
          insert({ tick: insertTick, value: evaluate(source, insertTick) ?? target.default });
        }}>Add point</button>
        <label>Interpolation
          <select aria-label="Interpolation" value={selected?.mode ?? ""} disabled={unavailable || !selected}
            onChange={event => {
              const mode = event.currentTarget.value;
              if (mode === "linear" || mode === "hold") submit(points.map((point, i) => i === index ? { ...point, mode } : point));
            }}>
            <option value="" disabled>—</option>
            <option value="linear">Linear</option>
            <option value="hold">Hold</option>
          </select>
        </label>
        <button type="button" aria-disabled={unavailable || !selected} onClick={() => remove(index)}>Delete point</button>
      </div>
    </header>
    <div className="automation-plot">
      {target && <div className="automation-axis" aria-label="Value range"><span>{target.format(target.max)}</span><span>{target.format(target.min)}</span></div>}
      <div ref={graph} className="automation-graph" role="group" aria-label="Automation curve" tabIndex={0}
        aria-disabled={unavailable} aria-describedby={helpId} onKeyDown={graphKey}
        onPointerDown={event => {
          if (event.target === event.currentTarget && event.button === 0) event.currentTarget.focus({ preventScroll: true });
        }}
        onPointerMove={event => updateDrag(event, false)} onPointerUp={event => updateDrag(event, true)}
        onPointerCancel={cancelPointer} onLostPointerCapture={cancelPointer}
        onDoubleClick={event => {
          if (!target || event.target !== event.currentTarget || blocked()) return;
          const box = event.currentTarget.getBoundingClientRect();
          insert(pointAt((event.clientX - box.left) / box.width, (event.clientY - box.top) / box.height, windowRange, target));
        }}>
        <svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
          <path d={path} vectorEffect="non-scaling-stroke" />
        </svg>
        {target && visiblePoints.map((point, pointIndex) => {
          const { x, y } = pointPosition(point, windowRange, target);
          return <button key={pointIndex} type="button" className="automation-point" tabIndex={-1}
            aria-label={`Point at tick ${point.tick}, ${target.format(point.value)}`}
            aria-pressed={pointIndex === index} aria-disabled={unavailable}
            style={{ left: `${x * 100}%`, top: `${y * 100}%` }}
            onPointerDown={event => startDrag(event, pointIndex)}
            onFocus={() => setSelection(pointIndex)}
            onClick={() => { if (!blocked()) setSelection(pointIndex); }}
            onContextMenu={event => { event.preventDefault(); remove(pointIndex); }} />;
        })}
        {(!target || !source) && <div className="automation-graph-message" role="status">
          {!target ? "Target unavailable" : curve.error ? <>
            <span>{curve.error}</span><button type="button" onClick={() => {
              graph.current?.focus({ preventScroll: true });
              curve.retry();
            }}>Retry curve</button>
          </> : "Loading curve…"}
        </div>}
      </div>
    </div>
    <footer className="automation-dock-footer">
      <span id={helpId}>Double-click to add. Drag or use arrows to move. Page Up/Down selects a point. Escape cancels a drag.</span>
      <span className="automation-value" aria-live={visiblePreview ? "off" : "polite"}>
        {selected && target ? `Tick ${selected.tick} · ${target.format(selected.value)}` : source && "No points in this clip"}
      </span>
    </footer>
  </section>;
}
