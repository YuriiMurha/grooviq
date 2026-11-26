import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { trackInitialLoad } from "./initialLoading";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

function deferredLoad() {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<void>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

test("shows the notice after the delay and clears it on success", async () => {
  const load = deferredLoad();
  const onState = vi.fn();
  trackInitialLoad(load.promise, onState);

  vi.advanceTimersByTime(199);
  expect(onState).not.toHaveBeenCalled();
  vi.advanceTimersByTime(1);
  expect(onState.mock.calls).toEqual([["slow"]]);

  load.resolve();
  await load.promise;
  expect(onState.mock.calls).toEqual([["slow"], ["settled"]]);
});

test("finishing before the delay never shows the notice", async () => {
  const load = Promise.resolve();
  const onState = vi.fn();
  trackInitialLoad(load, onState);

  await load;
  vi.runAllTimers();
  expect(onState.mock.calls).toEqual([["settled"]]);
});

test("clears the visible notice when the load rejects", async () => {
  const load = deferredLoad();
  const onState = vi.fn();
  trackInitialLoad(load.promise, onState);
  vi.advanceTimersByTime(200);
  expect(onState.mock.calls).toEqual([["slow"]]);

  const error = new Error("Initial view failed");
  load.reject(error);
  await expect(load.promise).rejects.toBe(error);
  expect(onState.mock.calls).toEqual([["slow"], ["settled"]]);
});

test.each([50, 200])("cleanup at %i ms prevents later updates", async (elapsed) => {
  const load = deferredLoad();
  const onState = vi.fn();
  const cancel = trackInitialLoad(load.promise, onState);
  vi.advanceTimersByTime(elapsed);
  cancel();
  onState.mockClear();

  vi.runAllTimers();
  load.resolve();
  await load.promise;
  expect(onState).not.toHaveBeenCalled();
});
