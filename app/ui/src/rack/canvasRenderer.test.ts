import { expect, test } from "vitest";
import { createCanvasRenderer } from "./canvasRenderer";
import { buildInstances, type GridLayout } from "./gridModel";
import type { Palette, Rgba } from "./palette";

const layout: GridLayout = { rows: 1, steps: 1, cell: 28, gap: 6, dpr: 1 };
const surface: Rgba = [0, 0, 0, 1];
const cases: { offset: Palette["offset"]; secondInk: boolean }[] = [
  { offset: [0, 0], secondInk: false },
  { offset: [2, 2], secondInk: true },
  { offset: [2, 0], secondInk: true },
  { offset: [0, 2], secondInk: true },
];

test.each(cases)("draws the second ink only with a nonzero offset ($offset)", ({ offset, secondInk }) => {
  const fills: string[] = [];
  const context = {
    fillStyle: "",
    fill() { fills.push(this.fillStyle); },
    fillRect() { this.fill(); },
    setTransform() {},
    clearRect() {},
    beginPath() {},
    roundRect() {},
    save() {},
    clip() {},
    restore() {},
  };
  const canvas = { width: 28, height: 28, getContext: () => context } as unknown as HTMLCanvasElement;
  const palette: Palette = {
    inks: [[1, 0, 0, 1]],
    inksB: [[0, 1, 0, 1]],
    stepA: surface,
    stepB: surface,
    stepLit: surface,
    focus: surface,
    playhead: surface,
    offset,
    missing: [],
  };

  createCanvasRenderer(canvas).draw(buildInstances(layout, [[1]], [0], null, null), layout, palette);

  const cellFills = ["rgba(0, 0, 0, 1)", "rgba(255, 0, 0, 1)"];
  expect(fills).toEqual(secondInk ? ["rgba(0, 255, 0, 1)", ...cellFills] : cellFills);
});
