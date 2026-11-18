import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdirSync, rmSync, existsSync, readdirSync, statSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { cpus, release } from 'node:os';

export function cases(report) {
  const found = [];
  function visit(suites) {
    for (const suite of suites ?? []) {
      for (const spec of suite.specs ?? []) for (const test of spec.tests ?? []) {
        found.push({ key: JSON.stringify([spec.id, spec.file, spec.line, spec.column, spec.title, test.projectName]), spec, test });
      }
      visit(suite.suites);
    }
  }
  visit(report.suites);
  return found;
}

export function account(inventory, report, exitCode, allowedSkips = []) {
  const expected = cases(inventory), actual = cases(report);
  if (exitCode !== 0 || report.errors?.length || !report.stats?.startTime || !Number.isFinite(report.stats.duration)) throw Error('incomplete or unsuccessful Playwright run');
  const keys = expected.map(c => c.key).sort();
  if (!keys.length || new Set(keys).size !== keys.length || JSON.stringify(keys) !== JSON.stringify(actual.map(c => c.key).sort())) throw Error('selection differs from inventory');
  for (const project of ['webkit', 'chromium']) if (!expected.some(c => c.test.projectName === project)) throw Error(`missing ${project}`);
  for (const { spec, test } of actual) {
    if (test.results?.length !== 1 || test.results[0].retry !== 0) throw Error('missing attempt or retry');
    const result = test.results[0];
    if (test.annotations?.some(a => a.type === 'fail')) throw Error('test.fail annotation is forbidden');
    if (result.status === 'skipped') {
      const reasons = (test.annotations ?? []).filter(a => a.type === 'skip').map(a => a.description);
      if (test.expectedStatus !== 'skipped' || !allowedSkips.some(a => a.file === spec.file && a.title === spec.title && a.project === test.projectName && reasons.includes(a.reason))) throw Error(`unapproved skip: ${spec.title}`);
    } else if (test.expectedStatus !== 'passed' || result.status !== 'passed' || test.status !== 'expected' || result.errors?.length || test.annotations?.some(a => a.type === 'fail')) throw Error(`non-pass: ${spec.title}`);
  }
  return { passed: actual.filter(c => c.test.results[0].status === 'passed').length, skipped: actual.filter(c => c.test.results[0].status === 'skipped').length };
}

export function run({ ui = resolve('app/ui'), out = resolve('/evidence'), repeats = 20, smoke = false } = {}) {
  if (!Number.isInteger(repeats) || repeats < 1 || (!smoke && repeats !== 20)) throw Error('proof requires twenty full repetitions');
  mkdirSync(out, { recursive: true });
  if (existsSync(resolve(out, 'summary.json'))) throw Error('evidence already exists; preserve it and choose a new output directory');
  const config = resolve(ui, 'stability.generated.config.ts');
  // Override selection independently of the ordinary config (including grep/project drops).
  writeFileSync(config, `import base from './playwright.config';\nimport { devices } from '@playwright/test';\nexport default { ...base, testDir: './e2e', testMatch: /.*\\.e2e\\.ts$/, testIgnore: [], grep: /.*/, grepInvert: undefined, shard: null, repeatEach: 1, retries: 0, workers: 2, forbidOnly: true, maxFailures: 0, projects: ['webkit','chromium'].map(name => ({ name, use: { ...devices[name === 'webkit' ? 'Desktop Safari' : 'Desktop Chrome'], viewport: {width:1280,height:800} } })), outputDir: '${out}/artifacts', reporter: [['json']] };\n`);
  const summary = { containerImage: process.env.STABILITY_CONTAINER_IMAGE, sha: process.env.STABILITY_SHA, image: 'mcr.microsoft.com/playwright:v1.64.1-noble', node: process.version, platform: process.platform, arch: process.arch, kernel: release(), cpuModel: cpus()[0]?.model, projects: ['webkit', 'chromium'], cpus: 2, memory: '4g', workers: 2, retries: 0, repeats, shards: '1/1', smoke, runs: [], complete: false };
  const save = () => writeFileSync(resolve(out, 'summary.json'), JSON.stringify(summary, null, 2));
  save();
  function invoke(name, extra = []) {
    const path = resolve(out, `${name}.json`);
    const started = Date.now();
    const result = spawnSync(process.execPath, ['node_modules/@playwright/test/cli.js', 'test', '--config', config, ...extra], { cwd: ui, env: { ...process.env, PLAYWRIGHT_JSON_OUTPUT_FILE: path }, encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 });
    writeFileSync(resolve(out, `${name}.log`), `${result.stdout ?? ''}\n${result.stderr ?? ''}\n${result.error ?? ''}`);
    summary.lastInvocation = { name, exitCode: result.status, signal: result.signal, error: result.error?.message, durationMs: Date.now() - started };
    writeFileSync(resolve(out, `${name}-process.json`), JSON.stringify(summary.lastInvocation, null, 2));
    save();
    return { report: JSON.parse(readFileSync(path)), code: result.status };
  }
  try {
    if (!smoke && (!/^[0-9a-f]{40}$/.test(summary.sha ?? '') || process.platform !== 'linux' || !process.version.startsWith('v24.'))) throw Error('requires exact SHA, Linux and Node 24');
    const lock = JSON.parse(readFileSync(resolve(ui, 'package-lock.json')));
    if (lock.packages['node_modules/@playwright/test'].version !== '1.64.1') throw Error('SDK/image mismatch');
    const inventory = invoke('inventory', ['--list']);
    if (inventory.code !== 0 || inventory.report.errors?.length) throw Error('inventory failed');
    const allowed = [{ file: 'm2-panels.e2e.ts', title: 'a_dpr_change_redraws_sharp', project: 'webkit', reason: 'changes the ratio through the Chrome DevTools Protocol' }];
    for (let i = 1; i <= repeats; i++) {
      summary.currentRepetition = i;
      save();
      const started = Date.now();
      const result = invoke(`run-${i}`);
      const entry = { repetition: i, exitCode: result.code, durationMs: Date.now() - started, statuses: {} };
      for (const c of cases(result.report)) for (const attempt of c.test.results ?? []) entry.statuses[attempt.status] = (entry.statuses[attempt.status] ?? 0) + 1;
      summary.runs.push(entry);
      save();
      Object.assign(entry, account(inventory.report, result.report, result.code, allowed));
      // Keep only the failing repetition's traces. JSON retains all attempt outcomes/reasons.
      rmSync(resolve(out, 'artifacts'), { recursive: true, force: true });
      save();
    }
    summary.complete = summary.runs.length === repeats;
  } catch (error) {
    summary.error = String(error);
    throw error;
  } finally {
    // One failed repetition only; cap retained diagnostics even for a suite-wide failure.
    let bytes = 0;
    summary.discardedArtifacts = [];
    function bound(dir) {
      if (!existsSync(dir)) return;
      for (const name of readdirSync(dir).sort()) {
        const path = resolve(dir, name), stat = statSync(path);
        if (stat.isDirectory()) bound(path);
        else if ((bytes += stat.size) > 128 * 1024 * 1024) {
          summary.discardedArtifacts.push(path);
          rmSync(path);
        }
      }
    }
    bound(resolve(out, 'artifacts'));
    save(); rmSync(config, { force: true });
  }
}
if (process.argv[1] === fileURLToPath(import.meta.url)) run();
