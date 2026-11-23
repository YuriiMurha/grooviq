import { expect, it } from "vitest";
import { ProjectCommandQueue, registerSettlement, settleProjectEdits, TransitionWork } from "./pending";

it("tracks intent before preflight without waiting on itself, and Save waits through publication", async () => {
  const work = new TransitionWork();
  const queue = new ProjectCommandQueue();
  const offWork = registerSettlement(work.settle);
  const offQueue = registerSettlement(() => queue.flush());
  let release!: () => void;
  let reply!: () => void;
  const trace: string[] = [];
  const prior = queue.enqueue(() => new Promise<void>((resolve) => { release = resolve; }));
  try {
    const mutation = work.run(async () => {
      trace.push("intent");
      expect(await work.settleOthers()).toBe(true);
      trace.push("native");
      await new Promise<void>((resolve) => { reply = resolve; });
      trace.push("published");
      return true;
    }, Boolean);
    expect(work.unsettled()).toBe(true);
    const save = settleProjectEdits().then((ok) => { expect(ok).toBe(true); trace.push("save"); });
    await Promise.resolve();
    expect(trace).toEqual(["intent"]);
    release(); await prior;
    for (let i = 0; i < 8; i++) await Promise.resolve();
    expect(trace).toEqual(["intent", "native"]);
    reply(); await mutation; await save;
    expect(trace).toEqual(["intent", "native", "published", "save"]);
    expect(work.unsettled()).toBe(false);
  } finally { offWork(); offQueue(); }
});

it("counts a refused transition once even when it completed before Save", async () => {
  const work = new TransitionWork();
  const off = registerSettlement(work.settle);
  try {
    expect(await work.run(async () => false, Boolean)).toBe(false);
    expect(work.unsettled()).toBe(true);
    expect(await settleProjectEdits()).toBe(false);
    expect(work.unsettled()).toBe(false);
    expect(await settleProjectEdits()).toBe(true);
  } finally { off(); }
});
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

it('submits a draft before freezing and waits for acceptance before snapshot', async () => {
  const { registerProjectDraft, beginProjectTransition, isProjectTransitioning } = await import('./pending');
  const queue = new ProjectCommandQueue();
  let accept!: () => void;
  let snapshot = false;
  const dispose = registerProjectDraft(() => {
    expect(isProjectTransitioning()).toBe(false);
    void queue.enqueue(() => new Promise<void>(resolve => { accept = resolve; }));
  });
  const finish = beginProjectTransition();
  const save = queue.flush().then(ok => { if (ok) snapshot = true; });
  await Promise.resolve(); await Promise.resolve();
  expect(snapshot).toBe(false);
  expect(accept).toBeTypeOf('function');
  accept(); await save;
  expect(snapshot).toBe(true);
  finish(); dispose();
});
