import { openSync, closeSync, readSync, fstatSync, constants, lstatSync, readdirSync, writeFileSync } from 'node:fs';
import { resolve, sep, dirname } from 'node:path';
import { pathToFileURL } from 'node:url';
const projects = ['webkit', 'chromium'];
const limit = 16 * 1024 * 1024;
function requireThat(ok, message) { if (!ok) throw new Error(message); }
function rejectSymlinks(path, message) {
  for (let p = path; ; p = dirname(p)) {
    requireThat(!lstatSync(p).isSymbolicLink(), message);
    if (p === dirname(p)) break;
  }
}
export function readJSON(root, name) {
  const base = resolve(root), path = resolve(base, name);
  requireThat(path.startsWith(base + sep), 'path outside accounting directory');
  rejectSymlinks(path, 'symlink in accounting path');
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = fstatSync(fd);
    requireThat(stat.isFile() && stat.size > 0 && stat.size <= limit, 'invalid accounting file size/type');
    const bytes = Buffer.alloc(limit + 1);
    let length = 0, count;
    while ((count = readSync(fd, bytes, length, bytes.length - length, null)) > 0) length += count;
    requireThat(length > 0 && length <= limit, 'accounting file grew beyond limit');
    return JSON.parse(bytes.subarray(0, length).toString('utf8'));
  } finally { closeSync(fd); }
}
function cases(report) {
  requireThat(report && Array.isArray(report.errors) && report.errors.length === 0 && Array.isArray(report.suites), 'missing report or framework errors');
  const found = new Map();
  function walk(suite, titles = []) {
    const ancestors = [...titles, suite.title];
    for (const spec of suite.specs ?? []) for (const t of spec.tests ?? []) {
      requireThat(projects.includes(t.projectName), 'unexpected project');
      const key = JSON.stringify([t.projectName, spec.file, spec.line, spec.column, ancestors, spec.title]);
      requireThat(!found.has(key), 'duplicate case');
      found.set(key, { spec, t });
    }
    for (const child of suite.suites ?? []) walk(child, ancestors);
  }
  for (const suite of report.suites) walk(suite);
  requireThat(found.size > 0, 'empty report');
  return found;
}
function checkedShard(shard) {
  requireThat((shard?.total === 1 || shard?.total === 2) && Number.isSafeInteger(shard.index) &&
    shard.index >= 1 && shard.index <= shard.total, 'invalid shard index or total (expected 1 or 2 shards)');
  return shard;
}
function receiptShard(receipt) {
  return Object.hasOwn(receipt, 'shard') ? checkedShard(receipt.shard) : { index: 1, total: 1 };
}
function validateCase(project, spec, t) {
  requireThat(Array.isArray(t.results) && t.results.length === 1 && t.results[0].retry === 0, 'missing/retried result');
  requireThat(!t.annotations?.some(a => a.type === 'fixme' || a.type === 'fail'), 'fixme/expected failure');
  const skip = project === 'webkit' && spec.file === 'm2-panels.e2e.ts' && spec.title === 'a_dpr_change_redraws_sharp' && t.expectedStatus === 'skipped' && t.annotations?.filter(a => a.type === 'skip').length === 1 && t.annotations.some(a => a.type === 'skip' && a.description === 'changes the ratio through the Chrome DevTools Protocol');
  requireThat((t.status === 'expected' && t.expectedStatus === 'passed' && t.results[0].status === 'passed') || (skip && t.status === 'skipped' && t.results[0].status === 'skipped'), 'non-passing case');
}
export function validate(receipts, meta, dependency) {
  requireThat(dependency === 'success', 'browser dependency did not succeed');
  requireThat(receipts.every(r => projects.includes(r?.project)), 'unexpected project in browser receipt');
  let baseline;
  for (const project of projects) {
    const matches = receipts.filter(r => r.project === project);
    requireThat(matches.length > 0, `missing shard receipts for ${project}`);
    const shards = matches.map(receiptShard);
    const { total } = shards[0];
    requireThat(shards.every(s => s.total === total), `mixed shard totals for ${project}`);
    requireThat(new Set(shards.map(s => s.index)).size === shards.length, `duplicate shard index for ${project}`);
    requireThat(shards.length === total, `missing shard receipts for ${project} (got ${shards.length} of ${total})`);

    let expected;
    const seen = new Set();
    for (const [position, r] of matches.entries()) {
      const { index } = shards[position];
      requireThat(r.outcome === 'success',
        `${project} shard ${index}/${total}: external suite step did not succeed (outcome: ${r.outcome ?? 'missing'})`);
      for (const field of ['head', 'workflow', 'run', 'attempt']) requireThat(typeof meta[field] === 'string' && meta[field].length > 0 && r[field] === meta[field], `stale ${field}`);
      const inventory = cases(r.inventory);
      const keys = JSON.stringify([...inventory.keys()].sort());
      requireThat(projects.every(p => [...inventory.values()].some(c => c.t.projectName === p)), 'discovery must include both projects');
      baseline ??= keys;
      requireThat(keys === baseline, 'inventories disagree');
      expected ??= new Set([...inventory].filter(([, c]) => c.t.projectName === project).map(([key]) => key));

      for (const [key, { spec, t }] of cases(r.report)) {
        requireThat(expected.has(key), `extra case in ${project}: ${key}`);
        requireThat(!seen.has(key), `duplicate case across shards in ${project}: ${key}`);
        seen.add(key);
        validateCase(project, spec, t);
      }
    }
    requireThat(seen.size === expected.size, `missing cases in ${project} (got ${seen.size} of ${expected.size})`);
  }
}
function readReceipts(root) {
  const base = resolve(root);
  rejectSymlinks(base, 'symlink in accounting path');
  const downloaded = readdirSync(base);
  const receipts = [];
  // Download e2e-accounting-<browser>-<i>-of-<n>-<run>-<attempt> into <browser>/<i>-of-<n>/.
  for (const project of projects.filter(p => downloaded.includes(p))) {
    const directory = resolve(base, project);
    rejectSymlinks(directory, 'symlink in accounting path');
    for (const name of readdirSync(directory)) {
      const legacy = name === 'receipt.json';
      if (!legacy && !/^\d+-of-\d+$/.test(name)) continue;
      const file = legacy ? `${project}/receipt.json` : `${project}/${name}/receipt.json`;
      const receipt = readJSON(base, file);
      requireThat(receipt?.project === project, `receipt does not match ${file}`);
      const { index, total } = receiptShard(receipt);
      requireThat((legacy ? '1-of-1' : name) === `${index}-of-${total}`, `receipt does not match ${file}`);
      receipts.push(receipt);
    }
  }
  return receipts;
}
function metadata() {
  return { head: process.env.GITHUB_SHA, workflow: process.env.GITHUB_WORKFLOW_SHA, run: process.env.GITHUB_RUN_ID, attempt: process.env.GITHUB_RUN_ATTEMPT };
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [mode, root, project, ...shardArgs] = process.argv.slice(2);
  if (mode === 'receipt') {
    requireThat(projects.includes(project), 'invalid project');
    // Missing reports remain missing evidence; the receipt must still record external outcome.
    const r = { ...metadata(), project, outcome: process.env.SUITE_OUTCOME };
    if (shardArgs.length) {
      requireThat(shardArgs.length === 2 && shardArgs.every(arg => /^[0-9]+$/.test(arg)), 'invalid shard arguments');
      const [index, total] = shardArgs.map(Number);
      r.shard = checkedShard({ index, total });
    }
    for (const name of ['inventory', 'report']) {
      try { r[name] = readJSON(root, `${name}.json`); } catch (error) { r[`${name}Error`] = error.message; }
    }
    // Check the owned directory itself even when both inputs are missing.
    rejectSymlinks(resolve(root), 'symlink in receipt path');
    const data = JSON.stringify(r);
    requireThat(Buffer.byteLength(data) <= limit, 'receipt too large');
    writeFileSync(resolve(root, 'receipt.json'), data, { flag: 'wx' });
  } else if (mode === 'aggregate') {
    validate(readReceipts(root), metadata(), process.env.BROWSER_RESULT);
    console.log('Both browser projects have complete successful coverage.');
  } else throw new Error('expected receipt or aggregate');
}
