import { spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync, rmSync, symlinkSync, copyFileSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import assert from 'node:assert/strict';
import { account, cases, run } from './run.mjs';
const dir = resolve('app/ui/stability-smoke');
mkdirSync(dir, { recursive: true });
const config = resolve(dir, 'playwright.config.mjs');
writeFileSync(config, `export default { testDir: '.', testMatch: '*.spec.mjs', workers: 2, retries: 0, reporter: 'json', projects: [{name:'webkit',use:{browserName:'webkit'}},{name:'chromium',use:{browserName:'chromium'}}] };`);
const cli = resolve('app/ui/node_modules/@playwright/test/cli.js');
function execute(source, list = false, extra = []) {
  writeFileSync(resolve(dir, 'tiny.spec.mjs'), `import { test, expect } from '@playwright/test';\n${source}`);
  const result = spawnSync(process.execPath, [cli, 'test', '-c', config, ...(list ? ['--list'] : []), ...extra], { encoding: 'utf8', maxBuffer: 4 * 1024 * 1024 });
  return { report: JSON.parse(result.stdout), code: result.status };
}
const results = [];
try {
  const source = `test('tiny', async ({page}) => { await page.setContent('<p>ready</p>'); await expect(page.locator('p')).toHaveText('ready'); });`;
  const inventory = execute(source, true).report;
  for (let repeat = 0; repeat < 2; repeat++) {
    const actual = execute(source);
    assert.deepEqual(account(inventory, actual.report, actual.code), {passed:2,skipped:0});
    results.push(`both browsers repetition ${repeat + 1}: passed`);
  }
  for (const [name, body] of [
    ['expected failure', `test('tiny', () => { test.fail(); expect(1).toBe(2); });`],
    ['unexpected skip', `test('tiny', () => { test.skip(true, 'new skip'); });`],
    ['failure', `test('tiny', () => { expect(1).toBe(2); });`],
    ['fixme', `test('tiny', () => { test.fixme(true, 'deferred'); });`],
  ]) {
    const actual = execute(body), selected = execute(body, true);
    assert.throws(() => account(selected.report, actual.report, actual.code));
    results.push(`${name}: rejected`);
  }
  const flakySource = `test('tiny', () => { expect(test.info().retry).toBe(1); });`;
  const flaky = execute(flakySource, false, ['--retries=1']);
  assert.equal(flaky.code, 0);
  assert.throws(() => account(execute(flakySource, true).report, flaky.report, flaky.code));
  results.push('actual retried flaky success: rejected');
  for (const [name, extra] of [['CLI grep selects nothing', ['--grep', 'no-such-case']], ['CLI drops WebKit', ['--project', 'chromium']]]) {
    const filtered = execute(source, false, extra);
    assert.throws(() => account(inventory, filtered.report, filtered.code));
    results.push(`${name}: rejected`);
  }
  const actual = execute(source);
  for (const [name, mutate] of [
    ['zero tests', r => { r.suites = []; }],
    ['missing browser', r => { for (const c of cases(r)) c.test.projectName = 'chromium'; }],
    ['retry', r => { cases(r)[0].test.results[0].retry = 1; }],
    ['missing attempt', r => { cases(r)[0].test.results = []; }],
    ['interrupted', r => { cases(r)[0].test.results[0].status = 'interrupted'; }],
    ['truncated completion', r => { delete r.stats; }],
  ]) {
    const report = structuredClone(actual.report);
    mutate(report);
    assert.throws(() => account(inventory, report, 0), name);
    results.push(`${name}: rejected`);
  }
  const body = `test('tiny', () => { test.skip(true, 'known platform'); });`;
  const skipped = execute(body), selected = execute(body, true);
  const allowed = cases(skipped.report).map(c => ({file:c.spec.file,title:c.spec.title,project:c.test.projectName,reason:'known platform'}));
  assert.deepEqual(account(selected.report, skipped.report, 0, allowed), {passed:0,skipped:2});
  results.push('explicit skips: recorded separately');
  mkdirSync(resolve(dir, 'e2e'));
  writeFileSync(resolve(dir, 'e2e/tiny.e2e.ts'), `import {test,expect} from '@playwright/test'; test('orchestration', () => {expect(1).toBe(1)});`);
  writeFileSync(resolve(dir, 'playwright.config.ts'), "export default {projects:[{name:'chromium'}],grep:/no-such-case/,testMatch:'nothing'};");
  symlinkSync(resolve('app/ui/node_modules'), resolve(dir, 'node_modules'));
  copyFileSync(resolve('app/ui/package-lock.json'), resolve(dir, 'package-lock.json'));
  run({ui:dir, out:'/evidence/orchestration', repeats:2, smoke:true});
  const summary = JSON.parse(readFileSync('/evidence/orchestration/summary.json'));
  assert.equal(summary.complete, true);
  assert.equal(summary.runs.length, 2);
  assert.ok(summary.runs.every(r => r.passed === 2 && r.skipped === 0));
  assert.throws(() => run({ui:dir, out:'/evidence/orchestration', repeats:2, smoke:true}));
  results.push('two-repeat orchestration and evidence overwrite guard: passed');
} finally {
  mkdirSync('/evidence', {recursive:true});
  writeFileSync('/evidence/smoke.json', JSON.stringify(results, null, 2));
  rmSync(dir, {recursive:true,force:true});
}
console.log(results.join('\n'));
