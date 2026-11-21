import { expect, it } from 'vitest';
import { TempoCommit } from './tempoCommit';
it('refusal permits retrying the identical tempo', async () => {
  const state = new TempoCommit(120);
  await state.send(130, async () => false);
  let calls = 0;
  await state.send(130, async () => { calls++; return true; });
  expect(calls).toBe(1); expect(state.target).toBe(130);
});
it('late refusal cannot roll back a newer accepted target', async () => {
  const state = new TempoCommit(120);
  let refuse!: (ok: boolean) => void;
  const old = state.send(130, () => new Promise(resolve => { refuse = resolve; }));
  await state.send(140, async () => true);
  refuse(false); await old;
  expect(state.target).toBe(140);
});
it('replacement invalidates old replies and unchanged tempo sends nothing', async () => {
  const state = new TempoCommit(120);
  let accept!: (ok: boolean) => void;
  const old = state.send(130, () => new Promise(resolve => { accept = resolve; }));
  state.reset(100); accept(true); await old;
  expect(state.target).toBe(100);
  await state.send(100, async () => { throw new Error('unchanged'); });
});
