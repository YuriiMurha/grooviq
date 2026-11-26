import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { scheduleInitialLoading } from "./initialLoading";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

test("shows a notice only after a noticeable wait", () => {
  const show = vi.fn();
  scheduleInitialLoading(show);

  vi.advanceTimersByTime(199);
  expect(show).not.toHaveBeenCalled();
  vi.advanceTimersByTime(1);
  expect(show).toHaveBeenCalledOnce();
});

test("finishing before the delay cancels the notice", () => {
  const show = vi.fn();
  const cancel = scheduleInitialLoading(show);

  vi.advanceTimersByTime(50);
  cancel();
  vi.runAllTimers();
  expect(show).not.toHaveBeenCalled();
});
