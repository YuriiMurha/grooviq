import { expect, test } from "vitest";
import { roundedVelocityFills } from "./velocityFills";

test("enumerates distinct Canvas2D byte fills, including both velocity endpoints", () => {
  expect(roundedVelocityFills([1 / 255, 2 / 255, 0], 0.45, "canvas2d")).toEqual([
    [0, 1, 0], [1, 1, 0], [1, 2, 0],
  ]);
});

test("retains a fill between the old 0.001 strength samples", () => {
  const fills = roundedVelocityFills([147 / 255, 167 / 255, 1], 0.45, "canvas2d");
  expect(fills).toContainEqual([66, 76, 115]);
  expect(fills).toContainEqual([121, 138, 211]);
});

test("covers framebuffer rounding on either side of a shared WebGL2 boundary", () => {
  const fills = roundedVelocityFills([1, 181 / 255, 17 / 255], 0.45, "webgl2");
  expect(fills).toContainEqual([158, 112, 10]); // Chromium
  expect(fills).toContainEqual([157, 112, 11]); // WebKit
  // Within the ±0.1 byte window red may already round to 159 while blue still rounds to 10,
  // but once red reaches 160 (strength ≥ 159.4/255), blue is at least 10.627 and can no
  // longer round down.
  expect(fills).toContainEqual([159, 112, 10]);
  expect(fills).not.toContainEqual([160, 113, 10]);
  // Each channel rounds independently: at strength 0.461 the ideal bytes are
  // [117.555, 83.441, 7.837], so red may round down while green rounds up.
  expect(fills).toContainEqual([117, 84, 8]);
});

test("keeps the byte that Float32 shader arithmetic allows at the quiet endpoint", () => {
  // 148 × 0.45 = 66.6 exactly, but Float32 gives 66.5999990702, so the D3D11 tolerance
  // permits byte 66 as well as 67.
  const fills = roundedVelocityFills([148 / 255, 0, 0], 0.45, "webgl2");
  expect(fills).toContainEqual([66, 0, 0]);
  expect(fills).toContainEqual([67, 0, 0]);
});
