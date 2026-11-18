import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdirSync, rmSync, existsSync, readdirSync, lstatSync, realpathSync } from 'node:fs';
import { resolve, relative, sep, isAbsolute } from 'node:path';
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

export function capArtifacts(out, summary, limit = 128 * 1024 * 1024) {
  const outStat = lstatSync(out);
  if (!outStat.isDirectory() || outStat.isSymbolicLink()) throw Error('evidence directory must be an owned directory');
  const anchor = realpathSync(out);
  let bytes = 0;
  summary.discardedArtifacts = [];
  function inside(path) {
    const rel = relative(anchor, path);
    if (rel === '..' || rel.startsWith(`..${sep}`) || isAbsolute(rel)) throw Error('artifact path escaped evidence');
  }
  function visit(path) {
    inside(resolve(path));
    let stat;
    try { stat = lstatSync(path); } catch (error) {
      if (error.code === 'ENOENT') return;
      throw error;
    }
    if (stat.isSymbolicLink()) {
      // Remove only the owned link; never inspect or upload its target.
      rmSync(path);
      summary.discardedArtifacts.push(path);
      return;
    }
    inside(realpathSync(path));
    if (stat.isDirectory()) {
      for (const name of readdirSync(path).sort()) visit(resolve(path, name));
    } else if (!stat.isFile() || (bytes += stat.size) > limit) {
      rmSync(path);
      summary.discardedArtifacts.push(path);
    }
  }
  visit(resolve(anchor, 'artifacts'));
}

export function run({ ui = resolve('app/ui'), out = resolve('/evidence'), repeats = 20, smoke = false } = {}) {
  if (!Number.isInteger(repeats) || repeats < 1 || (!smoke && repeats !== 20)) throw Error('proof requires twenty full repetitions');
  if (existsSync(out) && lstatSync(out).isSymbolicLink()) throw Error('evidence directory must not be a symlink');
  mkdirSync(out, { recursive: true });
  if (readdirSync(out).length) throw Error('evidence already exists; preserve it and choose a new output directory');
  const config = resolve(ui, 'stability.generated.config.ts');
  // Preserve candidate execution settings; override only full-selection/proof controls.
  const artifactPath = JSON.stringify(resolve(out, 'artifacts'));
  writeFileSync(config, `import base from './playwright.config';
const names = ['webkit', 'chromium'];
if (!Array.isArray(base.projects) || base.projects.length !== 2) throw Error('stability requires exactly WebKit and Chromium projects');
const projects = names.map(name => {
  const matches = base.projects.filter(project => project.name === name);
  if (matches.length !== 1) throw Error('missing or duplicate stability browser project: ' + name);
  const project = matches[0];
  if (project.dependencies?.length || project.teardown) throw Error('unsupported project lifecycle in stability proof');
  const use = { ...base.use, ...project.use };
  if ((use.browserName ?? use.defaultBrowserType ?? 'chromium') !== name) throw Error('project browser does not match its name: ' + name);
  return { ...project, testDir: './e2e', testMatch: /.*\\.e2e\\.ts$/, testIgnore: [], grep: /.*/, grepInvert: undefined, repeatEach: 1, retries: 0, workers: 2, outputDir: ${artifactPath} };
});
export default { ...base, testDir: './e2e', testMatch: /.*\\.e2e\\.ts$/, testIgnore: [], grep: /.*/, grepInvert: undefined, shard: null, repeatEach: 1, retries: 0, workers: 2, forbidOnly: true, maxFailures: 0, projects, outputDir: ${artifactPath}, reporter: [['json']] };
`, { flag: 'wx' });
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
    // Bound only owned artifact paths, without following links.
    capArtifacts(out, summary);
    save(); rmSync(config, { force: true });
  }
}
if (process.argv[1] === fileURLToPath(import.meta.url)) run();
