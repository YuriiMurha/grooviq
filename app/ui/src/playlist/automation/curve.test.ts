import { expect, test } from "vitest";
import type { CurvePoint } from "../../api/arrangement";
import { addPoint, clippedCurve, evaluate, freeTick, movePoint, pointAt, pointPosition, windowPoints } from "./curve";

const point = (tick: number, value: number, mode: CurvePoint["mode"] = "linear"): CurvePoint => ({ tick, value, mode });
const window = { start: 10, end: 20 };
const range = { min: -1, max: 1, stepped: false };

test("maps the graph onto integer source ticks and clamps to the target's range", () => {
  expect(pointAt(-0.2, 1.2, window, range)).toEqual({ tick: 10, value: -1 });
  expect(pointAt(1, -1, window, range)).toEqual({ tick: 19, value: 1 });
  expect(pointAt(0.45, 0.25, window, range)).toEqual({ tick: 15, value: 0.5 });
  expect(pointPosition(point(15, 0.5), window, range)).toEqual({ x: 0.5, y: 0.25 });
  expect(pointAt(0.5, 0.25, window, { min: -2, max: 4, stepped: true }).value).toBe(3);
  expect(pointAt(1, 0.5, { start: 9, end: 10 }, range).tick).toBe(9);
});

test("uses the last duplicate and selects only points in the half-open window", () => {
  const points = [point(9, 0), point(10, 0), point(10, 1, "hold"), point(20, -1)];
  expect(windowPoints(points, window)).toEqual([point(10, 1, "hold")]);
  expect(evaluate(points, 10)).toBe(1);
  expect(clippedCurve(points, window)).toEqual([{ tick: 10, value: 1 }, { tick: 20, value: 1 }]);
});

test("draws through trims using the full source, with hold jumps only inside the window", () => {
  const points = [point(0, 0), point(12, 0.75, "hold"), point(16, 0.5), point(24, 1)];
  expect(clippedCurve(points, window)).toEqual([
    { tick: 10, value: 0.625 }, { tick: 12, value: 0.75 },
    { tick: 16, value: 0.75 }, { tick: 16, value: 0.5 }, { tick: 20, value: 0.75 },
  ]);
  expect(clippedCurve(points, { start: 13, end: 15 })).toEqual([{ tick: 13, value: 0.75 }, { tick: 15, value: 0.75 }]);
  expect(clippedCurve([point(20, 1)], window)).toEqual([{ tick: 10, value: 1 }, { tick: 20, value: 1 }]);
  expect(clippedCurve([], window)).toEqual([]);
});

test("splits linear and hold segments using the original outgoing mode and keeps the source intact", () => {
  const source = [point(0, 0), point(12, 0.75, "hold"), point(24, 1)];
  const visible = windowPoints(source, window);
  expect(addPoint(visible, source, { tick: 10, value: evaluate(source, 10) ?? 0 })).toEqual([point(10, 0.625), ...visible]);
  expect(addPoint(visible, source, { tick: 16, value: evaluate(source, 16) ?? 0 })).toEqual([...visible, point(16, 0.75, "hold")]);
  expect(addPoint(visible, source, { tick: 12, value: -1 })).toBeNull();
  expect(source).toEqual([point(0, 0), point(12, 0.75, "hold"), point(24, 1)]);
});

test("clamps drags between neighbors without overwriting or reordering them", () => {
  const points = [point(11, 0), point(15, 0, "hold"), point(18, 0)];
  expect(movePoint(points, 1, { tick: 100, value: 0.5 }, window)).toEqual([points[0], point(17, 0.5, "hold"), points[2]]);
  expect(movePoint(points, 0, { tick: -100, value: 0 }, window)?.[0]).toEqual(point(10, 0));
  expect(movePoint(points, 2, { tick: 100, value: 0 }, window)?.[2]).toEqual(point(19, 0));
  expect(movePoint(points, 1, points[1], window)).toBeNull();
});

test("keyboard insertion chooses the middle of the largest available gap", () => {
  expect(freeTick([point(10, 0), point(13, 0)], window)).toBe(16);
  expect(freeTick([], { start: 6, end: 7 })).toBe(6);
  expect(freeTick([point(6, 0)], { start: 6, end: 7 })).toBeNull();
});
