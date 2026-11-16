import { expect, test } from "vitest";
import { kindIcon, kindWarns } from "./kind";

test("speakers warn, headphones do not", () => {
  expect([kindIcon("headphones"), kindWarns("headphones")]).toEqual(["🎧", false]);
  expect([kindIcon("speakers"), kindWarns("speakers")]).toEqual(["🔊", true]);
  expect([kindIcon("unknown"), kindWarns("unknown")]).toEqual(["🔈", false]);
});
