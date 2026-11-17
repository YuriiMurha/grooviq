import { describe, expect, it } from "vitest";
import { dragHandle, envPoints, handleAt, type EnvShape } from "./envelope";

const box = { left: 10, top: 10, width: 200, height: 100 };
const shape: EnvShape = { attack: 0.3, decay: 0.5, sustain: 0.6, release: 0.4 };

describe("envelope graph", () => {
  it("dragging_a_point_onto_itself_changes_nothing", () => {
    const p = envPoints(shape, box);
    for (const [h, x, y] of [
      ["attack", p.xA, p.yTop],
      ["decay", p.xD, p.yS],
      ["release", p.xR, p.yBottom],
    ] as const) {
      const after = dragHandle(shape, box, h, x, y);
      for (const k of ["attack", "decay", "sustain", "release"] as const) expect(after[k], `${h} ${k}`).toBeCloseTo(shape[k], 9);
    }
  });

  it("each_point_moves_only_its_own_values_and_clamps", () => {
    const p = envPoints(shape, box);
    const later = dragHandle(shape, box, "attack", p.xA + 20, 0);
    expect(later.attack).toBeGreaterThan(shape.attack);
    expect({ ...later, attack: 0 }).toEqual({ ...shape, attack: 0 });

    const lower = dragHandle(shape, box, "decay", p.xD, p.yS + 30);
    expect(lower.sustain).toBeCloseTo(0.3, 9);
    expect(lower.decay).toBeCloseTo(shape.decay, 9);

    expect(dragHandle(shape, box, "release", 10_000, 0).release).toBe(1);
    expect(dragHandle(shape, box, "release", -10_000, 0).release).toBe(0);
    expect(dragHandle(shape, box, "decay", p.xD, -500).sustain).toBe(1);
  });

  it("the_nearest_point_within_reach_is_grabbed", () => {
    const p = envPoints(shape, box);
    expect(handleAt(shape, box, p.xD + 3, p.yS - 2, 12)).toBe("decay");
    expect(handleAt(shape, box, p.xR - 5, p.yBottom, 12)).toBe("release");
    expect(handleAt(shape, box, (p.xD + p.xS) / 2, box.top + 5, 12)).toBeNull();
  });
});
