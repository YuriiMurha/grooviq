import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs, { realpathSync, mkdtempSync, readFileSync, rmSync, symlinkSync } from 'node:fs';
import os, { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { syncBuiltinESMExports } from 'node:module';
import Reporter from '../app/ui/e2e/diagnosticsReporter.ts';
const source = new URL('../app/ui/e2e/diagnosticsReporter.ts', import.meta.url).href;
const tcase = (id = 'one') => ({ id, titlePath: () => ['project', 'title\nsecond'], parent: { project: () => ({ name: 'webkit' }) } });
const result = { retry: 1, workerIndex: 2, parallelIndex: 0, status: 'failed', duration: 42 };
function patchBuiltin(t, module, name, replacement) {
  t.mock.method(module, name, replacement);
  syncBuiltinESMExports();
  t.after(() => { t.mock.restoreAll(); syncBuiltinESMExports(); });
}
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
  assert.equal(rows.at(-1).kind, 'framework-end');
  assert.equal(rows.at(-1).frameworkStatus, 'failed');
  assert.equal(rows[0].requiresRunnerExitStatus, true);
  assert.equal(r.activeTimer, false);
});
test('zero, skipped and timedOut retain real outcomes', async t => {
  for (const status of ['passed', 'failed', 'timedout', 'interrupted']) {
    const { reporter: r, records } = fixture(t);
    r.onBegin({}, { allTests: () => [] }); await r.onEnd({ status });
    assert.equal(records()[0].selected, 0); assert.equal(records().at(-1).frameworkStatus, status);
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
  assert.ok(!capped.records().some(x => x.kind === 'framework-end')); assert.equal(capped.reporter.activeTimer, false);
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
  assert.ok(!records().some(x => x.kind === 'framework-end'));
});
test('byte and individual-record caps are explicit failures', async t => {
  for (const options of [{ maxBytes: 1 }, {}]) {
    const { reporter: r, records } = fixture(t, options);
    r.onBegin({}, { allTests: () => [] });
    r.onTestBegin({ ...tcase(), titlePath: () => ['x'.repeat(70000)] }, result);
    assert.deepEqual(await r.onEnd({ status: 'passed' }), { status: 'failed' });
    if (!options.maxBytes) assert.ok(!records().some(x => x.kind === 'framework-end'));
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
  assert.ok(!records().some(x => x.kind === 'framework-end'));
  assert.equal(r.activeTimer, false);
});
test('unref timer cannot keep an otherwise idle child alive', async t => {
  const { root } = fixture(t);
  const child = spawn(process.execPath, ['--input-type=module', '-e', `import R from ${JSON.stringify(source)}; const r=new R({directory:${JSON.stringify(join(root, 'idle'))}}); r.onBegin({}, {allTests:()=>[]});`], { stdio: 'ignore' });
  const timeout = setTimeout(() => child.kill('SIGKILL'), 3000);
  try { const [code, signal] = await once(child, 'exit'); assert.equal(code, 0); assert.equal(signal, null); }
  finally { clearTimeout(timeout); }
});
test('final sync and close faults cannot certify diagnostic success', async t => {
  for (const method of ['fsyncSync', 'closeSync']) {
    await t.test(method, async child => {
      const { reporter: r, records } = fixture(child);
      r.onBegin({}, { allTests: () => [] });
      const original = fs[method];
      patchBuiltin(child, fs, method, (...args) => {
        original(...args);
        throw Object.assign(new Error('final I/O fault'), { code: 'EIO' });
      });
      const warnings = [];
      child.mock.method(process.stderr, 'write', text => { warnings.push(String(text)); return true; });
      assert.deepEqual(await r.onEnd({ status: 'passed' }), { status: 'failed' });
      const rows = records();
      assert.equal(rows[0].requiresRunnerExitStatus, true);
      assert.equal(rows.at(-1).kind, 'framework-end');
      assert.equal(rows.at(-1).frameworkStatus, 'passed');
      assert.ok(!rows.some(row => row.kind === 'terminal' || row.status === 'passed'));
      assert.ok(warnings.some(text => text.length > 0));
      assert.equal(r.activeTimer, false);
    });
  }
});
test('each record is flushed after its bytes are written', async t => {
  const { root, reporter: r } = fixture(t);
  const sizesAtFlush = [];
  const flush = fs.fsyncSync;
  patchBuiltin(t, fs, 'fsyncSync', fd => {
    sizesAtFlush.push(fs.fstatSync(fd).size);
    return flush(fd);
  });
  r.onBegin({}, { allTests: () => [tcase()] });
  r.onTestBegin(tcase(), result); r.onTestEnd(tcase(), result);
  assert.equal(await r.onEnd({ status: 'failed' }), undefined);
  const lines = readFileSync(join(root, 'journal', 'events.jsonl'), 'utf8').trimEnd().split('\n');
  let bytes = 0;
  assert.deepEqual(sizesAtFlush, lines.map(line => { bytes += Buffer.byteLength(line + '\n'); return bytes; }));
});
test('small records independently exhaust the total byte budget', async t => {
  const maxBytes = 4096;
  const { root, reporter: r, records } = fixture(t, { maxBytes });
  r.onBegin({}, { allTests: () => [] });
  for (let index = 0; index < 100; index++) r.onTestBegin(tcase(String(index)), result);
  assert.deepEqual(await r.onEnd({ status: 'passed' }), { status: 'failed' });
  assert.ok(fs.statSync(join(root, 'journal', 'events.jsonl')).size <= maxBytes);
  assert.ok(records().every(row => Buffer.byteLength(JSON.stringify(row)) < 1024));
  assert.ok(!records().some(row => row.kind === 'framework-end'));
});
test('onEnd cancels its registered interval callback', async t => {
  const callbacks = new Map();
  t.mock.method(globalThis, 'setInterval', (callback, delay) => {
    assert.equal(delay, 5000);
    const handle = { unref() { return this; } };
    callbacks.set(handle, callback);
    return handle;
  });
  t.mock.method(globalThis, 'clearInterval', handle => callbacks.delete(handle));
  let samples = 0;
  const { reporter: r } = fixture(t, { sample: () => { samples++; return { available: false }; } });
  r.onBegin({}, { allTests: () => [] });
  assert.equal(callbacks.size, 1);
  for (const callback of callbacks.values()) callback();
  assert.equal(samples, 2);
  await r.onEnd({ status: 'passed' });
  assert.equal(callbacks.size, 0);
  for (const callback of callbacks.values()) callback();
  assert.equal(samples, 2);
});
test('Windows load placeholders are unavailable independently of host memory', async t => {
  const descriptor = Object.getOwnPropertyDescriptor(process, 'platform');
  Object.defineProperty(process, 'platform', { value: 'win32' });
  t.after(() => Object.defineProperty(process, 'platform', descriptor));
  patchBuiltin(t, os, 'loadavg', () => [0, 0, 0]);
  const { resources } = await import('../app/ui/e2e/diagnosticsReporter.ts');
  const host = resources().host;
  assert.ok(host.memory, 'memory needs availability independent of load');
  assert.equal(host.memory.available, true);
  assert.equal(typeof host.memory.totalBytes, 'number');
  assert.deepEqual(host.loadAverage, { available: false });
});
