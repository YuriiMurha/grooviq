import { test } from 'node:test';
import assert from 'node:assert/strict';
import { realpathSync, mkdtempSync, readFileSync, rmSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import Reporter from '../app/ui/e2e/diagnosticsReporter.ts';
const source = new URL('../app/ui/e2e/diagnosticsReporter.ts', import.meta.url).href;
const tcase = (id = 'one') => ({ id, titlePath: () => ['project', 'title\nsecond'], parent: { project: () => ({ name: 'webkit' }) } });
const result = { retry: 1, workerIndex: 2, parallelIndex: 0, status: 'failed', duration: 42 };
function fixture(t, options = {}) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'grooviq-diagnostics-')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const reporter = new Reporter({ directory: join(root, 'journal'), sample: () => ({ scope: 'fixture', available: false }), ...options });
  const records = () => readFileSync(join(root, 'journal', 'events.jsonl'), 'utf8').trim().split('\n').map(JSON.parse);
  return { root, reporter, records };
}
test('inventory, begin, retry/end, newline identity and framework failure are exact', async t => {
  const { reporter: r, records } = fixture(t);
  r.onBegin({}, { allTests: () => [tcase(), tcase('never')] });
  r.onTestBegin(tcase(), result); r.onTestEnd(tcase(), result);
  assert.equal(await r.onEnd({ status: 'failed' }), undefined);
  const rows = records();
  assert.deepEqual(rows.filter(x => x.kind === 'inventory').map(x => x.id), ['one', 'never']);
  const begin = rows.find(x => x.kind === 'begin');
  assert.equal(begin.id, 'one'); assert.equal(begin.project, 'webkit');
  assert.deepEqual(begin.title, ['project', 'title\nsecond']);
  assert.equal(begin.retry, 1); assert.equal(begin.workerIndex, 2);
  assert.equal(rows.find(x => x.kind === 'end').status, 'failed');
  assert.equal(rows.at(-1).status, 'failed'); assert.equal(r.activeTimer, false);
});
test('zero, skipped and timedOut retain real outcomes', async t => {
  for (const status of ['passed', 'failed', 'timedout', 'interrupted']) {
    const { reporter: r, records } = fixture(t);
    r.onBegin({}, { allTests: () => [] }); await r.onEnd({ status });
    assert.equal(records()[0].selected, 0); assert.equal(records().at(-1).status, status);
  }
  const { reporter: r, records } = fixture(t);
  r.onBegin({}, { allTests: () => [tcase()] });
  r.onTestEnd(tcase(), { ...result, status: 'skipped' }); await r.onEnd({ status: 'passed' });
  assert.equal(records().find(x => x.kind === 'end').status, 'skipped');
});
test('resource failures explicit; timer stops; caps cannot produce completion', async t => {
  const { reporter: r, records } = fixture(t, { sample: () => { throw Error('private path'); } });
  r.onBegin({}, { allTests: () => [] }); await r.onEnd({ status: 'passed' });
  assert.deepEqual(records().find(x => x.kind === 'resource').observation, { scope: 'sampler', available: false });
  const capped = fixture(t, { maxRecords: 3 });
  capped.reporter.onBegin({}, { allTests: () => [tcase()] });
  assert.deepEqual(await capped.reporter.onEnd({ status: 'passed' }), { status: 'failed' });
  assert.ok(!capped.records().some(x => x.kind === 'terminal')); assert.equal(capped.reporter.activeTimer, false);
});
test('refuses existing directories and symlink ancestors without overwriting', async t => {
  const { root } = fixture(t);
  symlinkSync(root, join(root, 'link'));
  for (const directory of [root, join(root, 'link', 'new')]) {
    const r = new Reporter({ directory }); r.onBegin({}, { allTests: () => [] });
    assert.deepEqual(await r.onEnd({ status: 'passed' }), { status: 'failed' });
  }
});
test('owned child killed after flushed begin leaves incomplete journal', async t => {
  const { root, records } = fixture(t);
  const child = spawn(process.execPath, ['--input-type=module', '-e', `import R from ${JSON.stringify(source)}; const r=new R({directory:${JSON.stringify(join(root, 'journal'))}}); r.onBegin({}, {allTests:()=>[]}); r.onTestBegin({id:'killed',titlePath:()=>['x'],parent:{project:()=>({name:'fixture'})}}, {retry:0,workerIndex:0,parallelIndex:0}); process.stdout.write('ready'); setInterval(()=>{},1000);`], { stdio: ['ignore', 'pipe', 'pipe'] });
  t.after(() => child.kill('SIGKILL'));
  const timeout = setTimeout(() => child.kill('SIGKILL'), 5000);
  try { await once(child.stdout, 'data'); const exited = once(child, 'exit'); child.kill('SIGKILL'); await exited; }
  finally { clearTimeout(timeout); }
  assert.ok(records().some(x => x.kind === 'begin' && x.id === 'killed'));
  assert.ok(!records().some(x => x.kind === 'terminal'));
});
test('byte and individual-record caps are explicit failures', async t => {
  for (const options of [{ maxBytes: 1 }, {}]) {
    const { reporter: r, records } = fixture(t, options);
    r.onBegin({}, { allTests: () => [] });
    r.onTestBegin({ ...tcase(), titlePath: () => ['x'.repeat(70000)] }, result);
    assert.deepEqual(await r.onEnd({ status: 'passed' }), { status: 'failed' });
    if (!options.maxBytes) assert.ok(!records().some(x => x.kind === 'terminal'));
  }
});
test('real resource scope never attributes reporter CPU to browsers', async () => {
  const { resources } = await import('../app/ui/e2e/diagnosticsReporter.ts');
  const sample = resources();
  assert.equal(sample.process.scope, 'reporter-process-only');
  assert.equal(sample.host.scope, 'os-host-view-not-container');
  assert.equal(sample.cgroup.scope, 'cgroup-v2-mounted-root-not-attributed-to-browser');
  assert.equal(typeof sample.process.cpuMicroseconds.user, 'number');
});
test('write failure stays incomplete and preserves failed suite status', async t => {
  const { reporter: r, records } = fixture(t);
  r.onBegin({}, { allTests: () => [] });
  // Force the real descriptor closed: exercise writeSync failure, not a mocked writer.
  const { closeSync } = await import('node:fs');
  closeSync(r.fd);
  r.onTestBegin(tcase(), result);
  assert.deepEqual(await r.onEnd({ status: 'failed' }), { status: 'failed' });
  assert.ok(!records().some(x => x.kind === 'terminal'));
  assert.equal(r.activeTimer, false);
});
test('unref timer cannot keep an otherwise idle child alive', async t => {
  const { root } = fixture(t);
  const child = spawn(process.execPath, ['--input-type=module', '-e', `import R from ${JSON.stringify(source)}; const r=new R({directory:${JSON.stringify(join(root, 'idle'))}}); r.onBegin({}, {allTests:()=>[]});`], { stdio: 'ignore' });
  const timeout = setTimeout(() => child.kill('SIGKILL'), 3000);
  try { const [code, signal] = await once(child, 'exit'); assert.equal(code, 0); assert.equal(signal, null); }
  finally { clearTimeout(timeout); }
});
