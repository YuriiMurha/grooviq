import type { CurvePoint } from "../../api/arrangement";

export type CurveWindow = { start: number; end: number };
export type TargetRange = { min: number; max: number; stepped: boolean };
type Position = { tick: number; value: number };

const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value));

export function pointAt(x: number, y: number, window: CurveWindow, range: TargetRange): Position {
  const tick = clamp(Math.round(window.start + x * (window.end - window.start)), window.start, window.end - 1);
  const value = range.min + clamp(1 - y, 0, 1) * (range.max - range.min);
  return { tick, value: Math.fround(range.stepped ? Math.round(value) : value) };
}

export function pointPosition(point: Position, window: CurveWindow, range: TargetRange) {
  return {
    x: (point.tick - window.start) / (window.end - window.start),
    y: 1 - clamp((point.value - range.min) / (range.max - range.min), 0, 1),
  };
}

// Native curves are sorted. Legacy duplicates play the last point at that tick.
function normalized(points: readonly CurvePoint[]): CurvePoint[] {
  return points.filter((point, index) => points[index + 1]?.tick !== point.tick);
}

export function windowPoints(points: readonly CurvePoint[], window: CurveWindow): CurvePoint[] {
  return normalized(points.filter(point => point.tick >= window.start && point.tick < window.end));
}

export function previewSource(source: readonly CurvePoint[], edited: readonly CurvePoint[], window: CurveWindow): CurvePoint[] {
  return [
    ...source.filter(point => point.tick < window.start),
    ...edited,
    ...source.filter(point => point.tick >= window.end),
  ];
}

function valueAt(points: readonly CurvePoint[], tick: number, fromLeft = false): number | undefined {
  const nextIndex = points.findIndex(point => fromLeft ? point.tick >= tick : point.tick > tick);
  if (nextIndex === 0) return points[0].value;
  if (nextIndex === -1) return points.at(-1)?.value;
  const previous = points[nextIndex - 1];
  const next = points[nextIndex];
  if (previous.mode === "hold") return previous.value;
  const fraction = (tick - previous.tick) / (next.tick - previous.tick);
  return Math.fround(previous.value + (next.value - previous.value) * fraction);
}

export function evaluate(points: readonly CurvePoint[], tick: number): number | undefined {
  return valueAt(normalized(points), tick);
}

export function clippedCurve(source: readonly CurvePoint[], window: CurveWindow): Position[] {
  const points = normalized(source);
  const start = valueAt(points, window.start);
  const end = valueAt(points, window.end, true);
  if (start === undefined || end === undefined) return [];

  const path = [{ tick: window.start, value: start }];
  for (let index = 0; index < points.length; index++) {
    const { tick, value } = points[index];
    if (tick <= window.start) continue;
    if (tick >= window.end) break;
    const previous = points[index - 1];
    if (previous?.mode === "hold") path.push({ tick, value: previous.value });
    path.push({ tick, value });
  }
  path.push({ tick: window.end, value: end });
  return path;
}

export function addPoint(points: readonly CurvePoint[], source: readonly CurvePoint[], position: Position): CurvePoint[] | null {
  if (points.some(point => point.tick === position.tick)) return null;
  const next = source.findIndex(point => point.tick >= position.tick);
  const previous = next === -1 ? source.at(-1) : source[next - 1];
  const mode = previous?.mode ?? "linear";
  const index = points.findIndex(point => point.tick > position.tick);
  const insert = index === -1 ? points.length : index;
  return [...points.slice(0, insert), { ...position, mode }, ...points.slice(insert)];
}

export function movePoint(points: readonly CurvePoint[], index: number, position: Position, window: CurveWindow): CurvePoint[] | null {
  const original = points[index];
  if (!original) return null;
  const start = index === 0 ? window.start : points[index - 1].tick + 1;
  const end = index === points.length - 1 ? window.end - 1 : points[index + 1].tick - 1;
  const tick = clamp(position.tick, start, end);
  if (tick === original.tick && position.value === original.value) return null;
  return points.map((point, current) => current === index ? { ...point, tick, value: position.value } : point);
}

export function freeTick(points: readonly CurvePoint[], window: CurveWindow): number | null {
  let start = window.start;
  let widest = 0;
  let tick: number | null = null;
  for (const end of [...points.map(point => point.tick), window.end]) {
    const width = end - start;
    if (width > widest) {
      widest = width;
      tick = start + Math.floor((width - 1) / 2);
    }
    start = end + 1;
  }
  return tick;
}
