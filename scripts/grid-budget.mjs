#!/usr/bin/env node
import assert from "node:assert/strict";
import { performance } from "node:perf_hooks";
import { buildInstances, STRIDE } from "../app/ui/src/rack/gridModel.ts";

// Same 256x256 workload and one warm-up as the original unit timing test.
// Standalone V8 avoids Vitest worker contention; CPU time excludes descheduling.
// This is process CPU, not instruction counting or a browser/rendering benchmark.
const layout = { rows: 256, steps: 256, cell: 28, gap: 6, dpr: 2 };
const cells = Array.from({ length: 256 }, (_, r) =>
  Array.from({ length: 256 }, (_, s) => (r + s) % 3 === 0 ? 0.8 : null));
const inks = Array.from({ length: 256 }, (_, r) => r % 6);
buildInstances(layout, cells, inks, { row: 3, step: 3 }, 7);
const wallStart = performance.now();
const cpuStart = process.cpuUsage();
const data = buildInstances(layout, cells, inks, { row: 3, step: 3 }, 7);
const used = process.cpuUsage(cpuStart);
const wallMs = performance.now() - wallStart;
const cpuMs = (used.user + used.system) / 1000;
assert.equal(data.length, 256 * 256 * STRIDE);
assert.equal(data[5], Math.fround(0.8));
assert.equal(data.at(-2), Math.fround(0.8));
console.log(`256x256 grid: CPU ${cpuMs.toFixed(3)} ms, wall ${wallMs.toFixed(3)} ms; CPU budget <50 ms`);
assert.ok(cpuMs < 50, `largest grid used ${cpuMs} ms CPU (budget <50 ms)`);
