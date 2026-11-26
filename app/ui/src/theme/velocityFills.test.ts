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
  expect(fills).not.toContainEqual([159, 112, 10]);
});
