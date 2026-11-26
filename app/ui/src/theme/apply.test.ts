// @vitest-environment jsdom
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { applyTheme } from "./apply";
import { THEME_COLOR_TOKENS } from "./contract";
import { THEME_PREFERENCE_KEY } from "./preference";

const root = document.documentElement;

beforeEach(() => {
  const style = document.createElement("style");
  style.textContent = `:root {
    ${THEME_COLOR_TOKENS.map((name) => `${name}: #112233;`).join("\n")}
    --reg-x: 0px; --reg-y: 0px;
    --radius-s: 2px; --radius-m: 4px; --radius-l: 8px;
  }
  :root[data-theme="luminous"] { --ink-1: color-mix(in srgb, red, blue); }`;
  document.head.append(style);
});

afterEach(() => {
  root.removeAttribute("data-theme");
  document.head.querySelector("style")?.remove();
  window.localStorage.clear();
  vi.restoreAllMocks();
});

test("applies and stores an available theme", () => {
  expect(applyTheme("taste")).toEqual({ theme: "taste", persisted: true });
  expect(root.dataset.theme).toBe("taste");
  expect(window.localStorage.getItem(THEME_PREFERENCE_KEY)).toBe("taste");
});

test("a throwing writer leaves the chosen theme visible for this session", () => {
  const write = () => { throw new DOMException("Storage unavailable", "SecurityError"); };
  expect(applyTheme("console", write)).toEqual({ theme: "console", persisted: false });
  expect(root.dataset.theme).toBe("console");
});

test("an unknown theme falls back without overwriting the preference", () => {
  window.localStorage.setItem(THEME_PREFERENCE_KEY, "future-theme");
  expect(applyTheme("future-theme")).toEqual({ theme: "riso", persisted: false });
  expect(root.dataset.theme).toBe("riso");
  expect(window.localStorage.getItem(THEME_PREFERENCE_KEY)).toBe("future-theme");
});

test("incompatible tokens trigger one validation and a warned fallback without a write", () => {
  const computedStyle = vi.spyOn(window, "getComputedStyle");
  const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
  const write = vi.fn();
  expect(applyTheme("luminous", write)).toEqual({ theme: "riso", persisted: false });
  expect(root.dataset.theme).toBe("riso");
  expect(computedStyle).toHaveBeenCalledTimes(1);
  expect(warn).toHaveBeenCalledExactlyOnceWith("Theme has missing or incompatible tokens:", "--ink-1");
  expect(write).not.toHaveBeenCalled();
});
