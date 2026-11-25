import { expect, test } from "vitest";
import { kindWarns } from "./kind";

test("speakers warn, headphones do not", () => {
  expect(kindWarns("headphones")).toBe(false);
  expect(kindWarns("speakers")).toBe(true);
  expect(kindWarns("unknown")).toBe(false);
});
