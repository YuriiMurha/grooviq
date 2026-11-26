import assert from 'node:assert/strict';
import test from 'node:test';
import { account, filterArgs, selectionFromEnv } from './run.mjs';

function spec(project, title, id = title) {
  return {
    id, title, file: 'tiny.e2e.ts', line: 5, column: 1,
    tests: [{ projectName: project, expectedStatus: 'passed', status: 'expected',
      results: [{ retry: 0, status: 'passed', errors: [] }] }],
  };
}

function report(...specs) {
  return { suites: [{ title: 'tiny.e2e.ts', specs }], errors: [],
    stats: { startTime: '2026-11-26T00:00:00Z', duration: 1 } };
}

const webkit = { projects: ['webkit'], repeatEach: 2 };

test('preserves the two-browser, twenty-suite defaults', () => {
  assert.deepEqual(selectionFromEnv({}), {
    projects: ['webkit', 'chromium'], specs: [], grep: '', repeats: 20, repeatEach: 1,
  });
});

test('parses a focused selection and passes literal spec paths to Playwright', () => {
  const selection = selectionFromEnv({
    STABILITY_PROJECT: 'webkit', STABILITY_REPEATS: '3', STABILITY_REPEAT_EACH: '2',
    STABILITY_SPECS: 'app/ui/e2e/first.e2e.ts, e2e/nested/second (test).e2e.ts\nthird.e2e.ts',
    STABILITY_GREP: '-literal.*subject',
  });
  assert.deepEqual(selection, {
    projects: ['webkit'], specs: ['first.e2e.ts', 'nested/second (test).e2e.ts', 'third.e2e.ts'],
    grep: '-literal.*subject', repeats: 3, repeatEach: 2,
  });
  const [grep, ...files] = filterArgs(selection, '/work/app/ui');
  assert.equal(grep, '--grep=-literal.*subject');
  const second = new RegExp(files[1].slice(1, -1));
  assert.ok(second.test('/work/app/ui/e2e/nested/second (test).e2e.ts'));
  assert.ok(!second.test('/work/app/ui/e2e/nested/second test.e2e.ts'));
  assert.ok(!second.test('/work/app/ui/e2e/other/nested/second (test).e2e.ts'));
});

test('rejects invalid projects, counts and out-of-tree specs', () => {
  for (const [key, value] of [
    ['STABILITY_PROJECT', 'bench'], ['STABILITY_REPEATS', '0'], ['STABILITY_REPEAT_EACH', '1.5'],
    ['STABILITY_SPECS', '../outside.e2e.ts'], ['STABILITY_SPECS', '/outside.e2e.ts'],
  ]) {
    assert.throws(() => selectionFromEnv({ [key]: value }));
  }
});

test('counts Playwright repeat entries with different IDs for one selected case', () => {
  const inventory = report(spec('webkit', 'selected'));
  const actual = report(spec('webkit', 'selected'), spec('webkit', 'selected', 'repeat-id'));
  assert.deepEqual(account(inventory, actual, 0, [], webkit), { passed: 2, skipped: 0 });
});

test('rejects equal totals when a case loses a repeat and another gains one', () => {
  const inventory = report(spec('webkit', 'first'), spec('webkit', 'second'));
  const actual = report(spec('webkit', 'first'), spec('webkit', 'first', 'repeat-1'),
    spec('webkit', 'first', 'repeat-2'), spec('webkit', 'second'));
  assert.throws(() => account(inventory, actual, 0, [], webkit), /selection differs/);
});

test('accounts for both projects when Playwright merges their spec entries', () => {
  const webkitSpec = spec('webkit', 'selected');
  const chromiumSpec = spec('chromium', 'selected');
  const inventory = report(webkitSpec, chromiumSpec);
  const merged = structuredClone(webkitSpec);
  merged.tests.push(chromiumSpec.tests[0], structuredClone(chromiumSpec.tests[0]));
  const actual = report(merged, spec('webkit', 'selected', 'repeat-id'));
  assert.deepEqual(account(inventory, actual, 0, [], { repeatEach: 2 }), { passed: 4, skipped: 0 });
});

test('rejects empty and partial browser inventories', () => {
  assert.throws(() => account(report(), report(), 0, [], webkit), /empty stability inventory/);
  const onlyWebKit = report(spec('webkit', 'selected'));
  assert.throws(() => account(onlyWebKit, onlyWebKit, 0), /missing chromium/);
});

test('rejects duplicate cases in the unrepeated inventory', () => {
  const inventory = report(spec('webkit', 'selected'), spec('webkit', 'selected', 'other-id'));
  assert.throws(() => account(inventory, inventory, 0, [], webkit), /duplicate case/);
});

test('rejects extra projects and retried attempts in focused suites', () => {
  const inventory = report(spec('webkit', 'selected'));
  const extra = report(spec('webkit', 'selected'), spec('chromium', 'selected'));
  assert.throws(() => account(inventory, extra, 0, [], webkit), /selection differs/);
  const retried = spec('webkit', 'selected', 'repeat-id');
  retried.tests[0].results.push({ retry: 1, status: 'passed' });
  const actual = report(spec('webkit', 'selected'), retried);
  assert.throws(() => account(inventory, actual, 0, [], webkit), /missing attempt or retry/);
});

test('counts only the exact allowed skip for every repeat', () => {
  const allowed = [{ file: 'tiny.e2e.ts', title: 'selected', project: 'webkit', reason: 'CDP only' }];
  const inventory = report(spec('webkit', 'selected'));
  const skipped = spec('webkit', 'selected');
  Object.assign(skipped.tests[0], { expectedStatus: 'skipped', status: 'skipped',
    annotations: [{ type: 'skip', description: 'CDP only' }], results: [{ retry: 0, status: 'skipped' }] });
  const actual = report(skipped, structuredClone(skipped));
  assert.deepEqual(account(inventory, actual, 0, allowed, webkit), { passed: 0, skipped: 2 });
  actual.suites[0].specs[1].tests[0].annotations.push({ type: 'fixme' });
  assert.throws(() => account(inventory, actual, 0, allowed, webkit), /fixme/);
});
