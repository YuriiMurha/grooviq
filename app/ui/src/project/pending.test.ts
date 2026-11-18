import { expect, it } from "vitest";
import { ProjectCommandQueue, registerSettlement, settleProjectEdits } from "./pending";
it("a held refused project command aborts save/open/select settlement", async () => {
  const queue = new ProjectCommandQueue();
  const dispose = registerSettlement(() => queue.flush());
  let reject!: (reason: unknown) => void;
  const result = queue.enqueue(() => new Promise<void>((_, fail) => { reject = fail; }));
  void result.catch(() => {});
  await Promise.resolve();
  const settled = settleProjectEdits();
  reject(new Error("engine busy"));
  expect(await settled).toBe(false);
  expect(await settleProjectEdits()).toBe(true);
  dispose();
});
it("a refusal completed before a transition is still reported", async () => {
  const queue = new ProjectCommandQueue();
  await queue.enqueue(() => Promise.reject(new Error("file budget"))).catch(() => {});
  expect(await queue.flush()).toBe(false);
  expect(await queue.flush()).toBe(true);
});

it("only this queue's acknowledgements advance a captured step/undo revision", async () => {
  const { PatternRevisionChain } = await import("./pending");
  const chain = new PatternRevisionChain();
  const origin = { generation: 3, patternId: 8, revision: 1 };
  chain.accepted(origin,2);
  expect(chain.resolve(origin).revision).toBe(2);
  expect(chain.resolve({ ...origin, patternId: 9 }).revision).toBe(1);
  expect(chain.resolve({ ...origin, generation: 4 }).revision).toBe(1);
  // A recording edit arriving at revision 3 is not an acknowledgement by this queue.
  expect(chain.resolve(origin).revision).not.toBe(3);
});

it("a transition gate stays active through asynchronous settlement and releases once", async () => {
  const { beginProjectTransition, isProjectTransitioning } = await import("./pending");
  const finish = beginProjectTransition();
  expect(isProjectTransitioning()).toBe(true);
  await Promise.resolve();
  expect(isProjectTransitioning()).toBe(true);
  finish(); finish();
  expect(isProjectTransitioning()).toBe(false);
});

it("beginning a transition cancels held previews before freezing new gestures", async () => {
  const { registerGestureCancellation, beginProjectTransition, isProjectTransitioning } = await import("./pending");
  let held = true;
  const dispose = registerGestureCancellation(() => { expect(isProjectTransitioning()).toBe(false); held = false; });
  const finish = beginProjectTransition();
  expect(held).toBe(false); expect(isProjectTransitioning()).toBe(true);
  finish(); dispose();
});
