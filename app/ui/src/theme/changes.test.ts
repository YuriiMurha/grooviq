// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { subscribeThemeChange } from "./changes";

const root = document.documentElement;
const flushMutations = () => new Promise<void>((resolve) => queueMicrotask(resolve));

afterEach(() => {
  root.removeAttribute("data-theme");
  root.removeAttribute("data-other");
  vi.restoreAllMocks();
});

describe("theme changes", () => {
  it("notifies once for an effective change and ignores unrelated and same-value writes", async () => {
    root.dataset.theme = "riso";
    const changed = vi.fn();
    const unsubscribe = subscribeThemeChange(changed);
    try {
      root.dataset.other = "probe";
      await flushMutations();
      expect(changed).not.toHaveBeenCalled();
      root.dataset.theme = "riso";
      await flushMutations();
      expect(changed).not.toHaveBeenCalled();

      root.dataset.theme = "probe";
      root.dataset.theme = "probe";
      await flushMutations();
      expect(changed).toHaveBeenCalledTimes(1);

      root.removeAttribute("data-theme");
      await flushMutations();
      expect(changed).toHaveBeenCalledTimes(2);
    } finally {
      unsubscribe();
    }
  });

  it("shares one observer, releases it after the last unsubscribe and starts fresh", async () => {
    const observe = vi.spyOn(MutationObserver.prototype, "observe");
    const disconnect = vi.spyOn(MutationObserver.prototype, "disconnect");
    const first = vi.fn();
    const second = vi.fn();
    const stopFirst = subscribeThemeChange(first);
    const stopSecond = subscribeThemeChange(second);
    try {
      expect(observe).toHaveBeenCalledExactlyOnceWith(root, { attributeFilter: ["data-theme"] });
      stopFirst();
      expect(disconnect).not.toHaveBeenCalled();
      root.dataset.theme = "probe";
      await flushMutations();
      expect(first).not.toHaveBeenCalled();
      expect(second).toHaveBeenCalledTimes(1);

      stopSecond();
      expect(disconnect).toHaveBeenCalledTimes(1);
      root.dataset.theme = "riso";
      await flushMutations();
      expect(second).toHaveBeenCalledTimes(1);

      const stopAgain = subscribeThemeChange(second);
      try {
        expect(observe).toHaveBeenCalledTimes(2);
        root.dataset.theme = "probe";
        await flushMutations();
        expect(second).toHaveBeenCalledTimes(2);
      } finally {
        stopAgain();
      }
    } finally {
      stopFirst();
      stopSecond();
    }
  });
});
