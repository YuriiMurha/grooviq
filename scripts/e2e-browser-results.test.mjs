import { test } from 'node:test';
import assert from 'node:assert/strict';
import { validate } from './e2e-browser-results.mjs';
const meta = { head: 'a'.repeat(40), workflow: 'b'.repeat(40), run: '123', attempt: '1' };
const report = (project) => ({ errors: [], suites: [{ title: 'file', specs: [{ id: project, title: 'case', file: 'case.ts', tests: [{ projectName: project, expectedStatus: 'passed', status: 'expected', annotations: [], results: [{ status: 'passed', retry: 0 }] }] }] }] });
const fixture = () => ['webkit','chromium'].map(project => ({ ...meta, project, outcome: 'success', inventory: { errors: [], suites: [report('webkit').suites[0], report('chromium').suites[0]] }, report: report(project) }));
test('both projects complete', () => assert.doesNotThrow(() => validate(fixture(), meta, 'success')));
test('reject missing browser', () => assert.throws(
  () => validate(fixture().slice(0, 1), meta, 'success'), /missing shard.*chromium/,
));
for (const [name, mutate] of Object.entries({
 empty: x => x[0].report.suites = [], duplicate: x => x[0].report.suites.push(x[0].report.suites[0]), extra: x => x[0].report.suites[0].specs.push({...x[0].report.suites[0].specs[0], id:'extra'}),
 filtered: x => x[0].inventory.suites.pop(), stale: x => x[0].attempt='2', source: x => x[0].head='c'.repeat(40),
 staleRun: x => x[0].run='124', staleWorkflow: x => x[0].workflow='d'.repeat(40),
 failed: x => x[0].outcome='failure', cancelled: x => x[0].outcome='cancelled', skipped: x => x[0].outcome='skipped', missingReport: x => delete x[0].report,
 reporterIO: x => x[0].outcome='failure', error: x => x[0].report.errors.push({message:'error'}),
 failure: x => x[0].report.suites[0].specs[0].tests[0].results[0].status='failed',
 retry: x => x[0].report.suites[0].specs[0].tests[0].results[0].retry=1,
 expectedFailure: x => x[0].report.suites[0].specs[0].tests[0].expectedStatus='failed',
 fixme: x => x[0].report.suites[0].specs[0].tests[0].annotations=[{type:'fixme'}],
 skip: x => x[0].report.suites[0].specs[0].tests[0].results[0].status='skipped',
 partial: x => x[0].report.suites[0].specs[0].tests[0].results=[],
})) test(`reject ${name}`, () => { const x=fixture(); mutate(x); assert.throws(() => validate(x,meta,'success')); });
for (const status of ['failure','cancelled','skipped','']) test(`dependency ${status}`, () => assert.throws(() => validate(fixture(),meta,status)));
import { spawnSync } from 'node:child_process';
import { realpathSync, mkdtempSync, mkdirSync, writeFileSync, symlinkSync, rmSync, truncateSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readJSON } from './e2e-browser-results.mjs';
test('bounded owned file reads', () => {
 const dir=mkdtempSync(join(realpathSync(tmpdir()),'e2e-accounting-'));
 try {
  writeFileSync(join(dir,'good.json'),'{}'); assert.deepEqual(readJSON(dir,'good.json'),{});
  assert.throws(()=>readJSON(dir,'../outside.json'),/outside/);
  assert.throws(()=>readJSON(dir,'missing.json'));
  writeFileSync(join(dir,'empty.json'),''); assert.throws(()=>readJSON(dir,'empty.json'),/size/);
  writeFileSync(join(dir,'large.json'),''); truncateSync(join(dir,'large.json'),16*1024*1024+1); assert.throws(()=>readJSON(dir,'large.json'),/size/);
  symlinkSync(join(dir,'good.json'),join(dir,'link.json')); assert.throws(()=>readJSON(dir,'link.json'),/symlink/);
  symlinkSync(dir,join(dir,'parent')); assert.throws(()=>readJSON(join(dir,'parent'),'good.json'),/symlink/);
 } finally {rmSync(dir,{recursive:true});}
});
test('only exact WebKit CDP skip is accepted', () => {
 const x=fixture(); const spec=x[0].report.suites[0].specs[0];
 spec.file='m2-panels.e2e.ts'; spec.title='a_dpr_change_redraws_sharp';
 x.forEach(r=> {r.inventory.suites[0].specs[0].file=spec.file;r.inventory.suites[0].specs[0].title=spec.title;});
 const t=spec.tests[0];t.expectedStatus='skipped'; t.status='skipped'; t.results[0].status='skipped';t.annotations=[{type:'skip',description:'changes the ratio through the Chrome DevTools Protocol'}];
 assert.doesNotThrow(()=>validate(x,meta,'success'));
 t.status='expected';assert.throws(()=>validate(x,meta,'success'));t.status='skipped';
 t.annotations[0].description='other';assert.throws(()=>validate(x,meta,'success'));
});
test('one passing case cannot cover two discovered cases', () => {
 const x=fixture();
 for (const r of x) { const spec=structuredClone(r.inventory.suites[0].specs[0]);spec.title='second case';r.inventory.suites[0].specs.push(spec); }
 assert.throws(()=>validate(x,meta,'success'),/coverage|missing/);
});

function shardedFixture() {
  const receipts = fixture();
  for (const receipt of receipts) {
    for (const suite of receipt.inventory.suites) {
      const second = structuredClone(suite.specs[0]);
      second.title = 'second case';
      suite.specs.push(second);
    }
    receipt.shard = { index: 1, total: 2 };
  }
  return receipts.flatMap(first => {
    const second = structuredClone(first);
    second.shard.index = 2;
    second.report.suites[0].specs[0].title = 'second case';
    return [first, second];
  });
}

test('each browser can report two disjoint shards in any order', () => {
  assert.doesNotThrow(() => validate(shardedFixture().reverse(), meta, 'success'));
});

test('each browser can have a different shard total', () => {
  const [webkit1, webkit2, chromium] = shardedFixture();
  delete chromium.shard;
  chromium.report.suites[0].specs.push(chromium.inventory.suites[1].specs[1]);
  assert.doesNotThrow(() => validate([webkit1, webkit2, chromium], meta, 'success'));
});

for (const [name, mutate, error] of [
  ['missing shard despite complete case coverage', x => {
    x[0].report.suites[0].specs.push(x[1].report.suites[0].specs[0]);
    x.splice(1, 1);
  }, /missing shard.*webkit/],
  ['duplicate shard index', x => { x[1].shard.index = 1; }, /duplicate shard index.*webkit/],
  ['mixed totals', x => { x[0].shard.total = 1; }, /mixed shard totals.*webkit/],
  ['duplicate case across shards', x => {
    x[1].report.suites[0].specs.push(x[0].report.suites[0].specs[0]);
  }, /duplicate case/],
  ['missing case across shards', x => {
    for (const r of x) {
      const third = structuredClone(r.inventory.suites[0].specs[0]);
      third.title = 'third case';
      r.inventory.suites[0].specs.push(third);
    }
  }, /missing cases/],
  ['extra case in a shard', x => {
    x[1].report.suites[0].specs[0].title = 'undiscovered case';
  }, /extra case/],
  ['different inventories within a browser', x => {
    x[1].inventory.suites[1].specs[1].title = 'different Chromium case';
  }, /inventories disagree/],
  ['different inventories across browsers', x => {
    for (const r of x.slice(2)) r.inventory.suites[0].specs[1].title = 'different WebKit case';
  }, /inventories disagree/],
  ['stale run in a shard', x => { x[1].run = '124'; }, /stale run/],
  ['stale attempt in a shard', x => { x[1].attempt = '2'; }, /stale attempt/],
  ['failed shard outcome', x => { x[1].outcome = 'failure'; }, /external suite step did not succeed/],
  ['cancelled shard outcome', x => { x[1].outcome = 'cancelled'; }, /webkit shard 2\/2.*cancelled/],
  ['unexpected browser receipt', x => x.push({ ...x[0], project: 'firefox' }), /unexpected project/],
]) test(`reject ${name}`, () => {
  const receipts = shardedFixture();
  mutate(receipts);
  assert.throws(() => validate(receipts, meta, 'success'), error);
});

for (const shard of [null, {}, { index: '2', total: 2 }, { index: 1.5, total: 2 },
  { index: 0, total: 2 }, { index: 3, total: 2 }, { index: 2, total: 0 },
  { index: 2, total: Number.MAX_SAFE_INTEGER + 1 }]) {
  test(`reject invalid shard ${JSON.stringify(shard)}`, () => {
    const receipts = shardedFixture();
    receipts[1].shard = shard;
    assert.throws(() => validate(receipts, meta, 'success'), /invalid shard/);
  });
}

test('reject three shards even when all three reports cover the inventory', () => {
  const receipts = shardedFixture();
  const third = structuredClone(receipts[0].report.suites[0].specs[0]);
  third.title = 'third case';
  for (const r of receipts) {
    r.inventory.suites[0].specs.push(third);
    if (r.project === 'webkit') r.shard.total = 3;
  }
  const extraShard = structuredClone(receipts[0]);
  extraShard.shard.index = 3;
  extraShard.report.suites[0].specs = [third];
  receipts.push(extraShard);
  assert.throws(() => validate(receipts, meta, 'success'), /invalid shard/);
});

const script = fileURLToPath(new URL('./e2e-browser-results.mjs', import.meta.url));
function cli(...args) {
  return spawnSync(process.execPath, [script, ...args], {
    encoding: 'utf8',
    env: {
      ...process.env, GITHUB_SHA: meta.head, GITHUB_WORKFLOW_SHA: meta.workflow,
      GITHUB_RUN_ID: meta.run, GITHUB_RUN_ATTEMPT: meta.attempt,
      BROWSER_RESULT: 'success', SUITE_OUTCOME: 'success',
    },
  });
}

function accountingDir(t) {
  const root = mkdtempSync(join(realpathSync(tmpdir()), 'e2e-accounting-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
}

for (const sharded of [false, true]) {
  test(`receipt and aggregate CLI accept ${sharded ? 'downloaded shards' : 'legacy browser directories'}`, t => {
    const root = accountingDir(t);
    for (const { project, shard, inventory, report: results } of sharded ? shardedFixture() : fixture()) {
      const dir = shard ? join(root, project, `${shard.index}-of-${shard.total}`) : join(root, project);
      mkdirSync(dir, { recursive: true });
      writeFileSync(join(dir, 'inventory.json'), JSON.stringify(inventory));
      writeFileSync(join(dir, 'report.json'), JSON.stringify(results));
      const result = cli('receipt', dir, project, ...(shard ? [shard.index, shard.total].map(String) : []));
      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(readJSON(dir, 'receipt.json').shard, shard);
    }
    const result = cli('aggregate', root);
    assert.equal(result.status, 0, result.stderr);
  });
}

test('receipt CLI rejects incomplete or invalid shard arguments', t => {
  const root = accountingDir(t);
  for (const args of [['2'], ['3', '2'], ['0x2', '2'], ['1', '3']]) {
    const result = cli('receipt', root, 'webkit', ...args);
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, /invalid shard/);
  }
});

test('aggregate refuses a symlink at a downloaded shard directory', t => {
  const root = accountingDir(t);
  const target = accountingDir(t);
  writeFileSync(join(target, 'receipt.json'), JSON.stringify(shardedFixture()[0]));
  mkdirSync(join(root, 'webkit'));
  symlinkSync(target, join(root, 'webkit', '1-of-2'), 'dir');
  const result = cli('aggregate', root);
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /symlink/);
});

test('aggregate rejects a shard receipt downloaded into a mismatched directory', t => {
  const root = accountingDir(t);
  for (const r of shardedFixture()) {
    const index = r.project === 'webkit' ? 3 - r.shard.index : r.shard.index;
    const dir = join(root, r.project, `${index}-of-2`);
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, 'receipt.json'), JSON.stringify(r));
  }
  const result = cli('aggregate', root);
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /receipt does not match.*webkit\/1-of-2/);
});
