import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { aggregate, receiptPath, renderSummary } from './ci-sharded.mjs';

const meta = { head: 'a'.repeat(40), workflow: 'b'.repeat(40), run: '123', attempt: '2' };
const browsers = ['webkit', 'chromium'];

function browserReport(project, shards) {
  return {
    errors: [],
    suites: [{
      title: 'case.e2e.ts',
      specs: shards.flatMap(shard => [1, 2].map(number => ({
        title: `case ${shard}.${number}`,
        file: 'case.e2e.ts',
        line: shard * 10 + number,
        column: 1,
        tests: [{
          projectName: project, expectedStatus: 'passed', status: 'expected',
          annotations: [], results: [{ status: 'passed', retry: 0 }],
        }],
      }))),
    }],
  };
}

function fixture(t, change = receipt => receipt) {
  const root = mkdtempSync(join(realpathSync(tmpdir()), 'sharded-stability-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const inventory = { errors: [], suites: browsers.flatMap(project => browserReport(project, [1, 2]).suites) };
  for (const repetition of [1, 2]) for (const project of browsers) for (const index of [1, 2]) {
    const receipt = change({
      ...meta, project, shard: { index, total: 2 }, outcome: 'success',
      inventory, report: browserReport(project, [index]),
    }, repetition);
    if (!receipt) continue;
    const path = join(root, receiptPath(project, index, meta, repetition));
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, JSON.stringify(receipt));
  }
  return root;
}

test('accounts for the two browsers separately in each repetition', t => {
  const result = aggregate(fixture(t), meta, 2, 'success');
  assert.equal(result.ok, true);
  for (const repetition of [1, 2]) for (const project of browsers) {
    assert.ok(renderSummary(result, meta).includes(`| ${repetition} | ${project} | 4 | 4 | 0 | 0 |`));
  }
});

test('counts the allowlisted WebKit skip as skipped, not passed', t => {
  const allowlisted = spec => Object.assign(spec, { file: 'm2-panels.e2e.ts', title: 'a_dpr_change_redraws_sharp' });
  const root = fixture(t, receipt => {
    const changed = structuredClone(receipt);
    // The inventory lists both browsers; its first suite is WebKit's.
    allowlisted(changed.inventory.suites[0].specs[0]);
    if (changed.project === 'webkit' && changed.shard.index === 1) {
      const spec = allowlisted(changed.report.suites[0].specs[0]);
      Object.assign(spec.tests[0], {
        expectedStatus: 'skipped', status: 'skipped',
        annotations: [{ type: 'skip', description: 'changes the ratio through the Chrome DevTools Protocol' }],
        results: [{ status: 'skipped', retry: 0 }],
      });
    }
    return changed;
  });
  const result = aggregate(root, meta, 2, 'success');
  assert.equal(result.ok, true, renderSummary(result, meta));
  for (const repetition of [1, 2]) {
    assert.ok(renderSummary(result, meta).includes(`| ${repetition} | webkit | 4 | 3 | 1 | 0 |`));
  }
});

test('rejects a missing case in repetition 2 despite complete coverage in repetition 1', t => {
  const root = fixture(t, (receipt, repetition) => {
    if (repetition === 2 && receipt.project === 'webkit' && receipt.shard.index === 2) {
      receipt.report.suites[0].specs.pop();
    }
    return receipt;
  });
  const result = aggregate(root, meta, 2, 'success');
  assert.equal(result.ok, false);
  assert.match(renderSummary(result, meta), /Repetition 2: missing cases in webkit \(got 3 of 4\)/);
});

test('rejects overlapping shard reports even when their combined count matches inventory', t => {
  const root = fixture(t, (receipt, repetition) => {
    if (repetition === 2 && receipt.project === 'chromium' && receipt.shard.index === 2) {
      receipt.report.suites[0].specs[0] = browserReport('chromium', [1]).suites[0].specs[0];
    }
    return receipt;
  });
  const result = aggregate(root, meta, 2, 'success');
  assert.equal(result.ok, false);
  assert.match(renderSummary(result, meta), /Repetition 2: duplicate case across shards in chromium/);
});

for (const [name, change, error] of [
  ['missing receipt', () => null, /Repetition 2, webkit shard 2\/2: .*receipt\.json/],
  ['wrong shard', receipt => ({ ...receipt, shard: { index: 1, total: 2 } }), /Repetition 2, webkit shard 2\/2: receipt does not match/],
  ['stale SHA', receipt => ({ ...receipt, head: 'c'.repeat(40) }), /Repetition 2: stale head/],
  ['empty inventory', receipt => ({ ...receipt, inventory: { errors: [], suites: [] } }), /Repetition 2: empty report/],
  ['cancelled shard', receipt => ({ ...receipt, outcome: 'cancelled' }), /Repetition 2: webkit shard 2\/2: .*cancelled/],
]) {
  test(`rejects ${name}`, t => {
    const root = fixture(t, (receipt, repetition) => (
      repetition === 2 && receipt.project === 'webkit' && receipt.shard.index === 2 ? change(receipt) : receipt
    ));
    const result = aggregate(root, meta, 2, 'success');
    assert.equal(result.ok, false);
    assert.match(renderSummary(result, meta), error);
  });
}

test('fails if the matrix was cancelled after writing otherwise valid receipts', t => {
  assert.equal(aggregate(fixture(t), meta, 2, 'cancelled').ok, false);
});

test('lists every failing case with its repetition and shard even if the matrix failed', t => {
  const root = fixture(t, (receipt, repetition) => {
    if (receipt.project === 'chromium' && receipt.shard.index === repetition) {
      receipt.outcome = 'failure';
      receipt.report.suites[0].specs[0].tests[0].results[0].status = 'timedOut';
      receipt.report.suites[0].specs[1].tests[0].results.push({ retry: 1, status: 'passed' });
    }
    return receipt;
  });
  const result = aggregate(root, meta, 2, 'failure');
  assert.equal(result.ok, false);
  const summary = renderSummary(result, meta);
  for (const repetition of [1, 2]) {
    assert.match(summary, new RegExp(`Repetition ${repetition}, chromium shard ${repetition}/2: .*case ${repetition}\\.1.*timedOut`));
    assert.match(summary, new RegExp(`Repetition ${repetition}, chromium shard ${repetition}/2: .*case ${repetition}\\.2.*retry`));
    assert.ok(summary.includes(`| ${repetition} | chromium | 4 | 2 | 0 | 2 |`));
  }
});
