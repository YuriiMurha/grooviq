import { describe, expect, it } from "vitest";
import { envelopeMeter, lfoMeter } from "./meterGeometry";

const box = { left: 10, attack: 30, decay: 60, sustain: 80, right: 110, top: 10, bottom: 90 };
describe("measured synth motion", () => {
  it("keeps each stage inside its own interval and sustain stationary as level changes", () => {
    for (const [stage, left, width, x] of [["attack", 10, 20, 20], ["decay", 30, 30, 45], ["sustain", 60, 20, 70], ["release", 80, 30, 95]] as const) {
      expect(envelopeMeter(stage, 0.25, box, true)).toEqual({ left, width, x, y: 70 });
      expect(envelopeMeter(stage, 0.75, box, true)?.x).toBe(x);
    }
  });
  it("clamps levels and hides idle or inactive measurements", () => {
    expect(envelopeMeter("attack", 2, box, true)?.y).toBe(10);
    expect(envelopeMeter("release", -1, box, true)?.y).toBe(90);
    expect(envelopeMeter("idle", 1, box, true)).toBeNull();
    expect(envelopeMeter("sustain", 1, box, false)).toBeNull();
    expect(envelopeMeter("attack", NaN, box, true)).toBeNull();
  });
  it("uses actual phase within the first cycle and independent smoothed output", () => {
    expect(lfoMeter(0, 0.5, 1, box, true)).toEqual({ x: 10, y: 30 });
    expect(lfoMeter(0.5, 0.5, 1, box, true)).toEqual({ x: 35, y: 30 });
    expect(lfoMeter(1, 0.5, 1, box, true)).toEqual({ x: 10, y: 30 });
    expect(lfoMeter(1.5, -2, 0.5, box, true)).toEqual({ x: 35, y: 70 });
    expect(lfoMeter(0.5, 0, 1, box, false)).toBeNull();
    expect(lfoMeter(NaN, 0, 1, box, true)).toBeNull();
  });
});
