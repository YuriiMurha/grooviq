import { openSync, closeSync, readSync, fstatSync, constants, lstatSync, writeFileSync } from 'node:fs';
import { resolve, sep, dirname } from 'node:path';
import { pathToFileURL } from 'node:url';
const projects = ['webkit', 'chromium'];
const limit = 16 * 1024 * 1024;
function requireThat(ok, message) { if (!ok) throw new Error(message); }
export function readJSON(root, name) {
  const base = resolve(root), path = resolve(base, name);
  requireThat(path.startsWith(base + sep), 'path outside accounting directory');
  for (let p = path; ; p = dirname(p)) {
    requireThat(!lstatSync(p).isSymbolicLink(), 'symlink in accounting path');
    if (p === dirname(p)) break;
  }
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
export function validate(receipts, meta, dependency) {
  requireThat(dependency === 'success', 'browser dependency did not succeed');
  requireThat(receipts.length === 2, 'need both browser receipts');
  let baseline;
  for (const project of projects) {
    const matches = receipts.filter(r => r.project === project);
    requireThat(matches.length === 1, 'missing/duplicate browser receipt');
    const r = matches[0];
    requireThat(r.outcome === 'success', 'external suite step did not succeed');
    for (const field of ['head', 'workflow', 'run', 'attempt']) requireThat(typeof meta[field] === 'string' && meta[field].length > 0 && r[field] === meta[field], `stale ${field}`);
    const inventory = cases(r.inventory);
    const keys = [...inventory.keys()].sort();
    requireThat(projects.every(p => [...inventory.values()].some(c => c.t.projectName === p)), 'discovery must include both projects');
    if (baseline) requireThat(JSON.stringify(keys) === baseline, 'inventories disagree');
    baseline = JSON.stringify(keys);
    const expected = new Set([...inventory].filter(([, c]) => c.t.projectName === project).map(([k]) => k));
    const actual = cases(r.report);
    requireThat(actual.size === expected.size, 'partial/extra report coverage');
    for (const [key, { spec, t }] of actual) {
      requireThat(expected.delete(key), 'extra case');
      requireThat(Array.isArray(t.results) && t.results.length === 1 && t.results[0].retry === 0, 'missing/retried result');
      requireThat(!t.annotations?.some(a => a.type === 'fixme' || a.type === 'fail'), 'fixme/expected failure');
      const skip = project === 'webkit' && spec.file === 'm2-panels.e2e.ts' && spec.title === 'a_dpr_change_redraws_sharp' && t.expectedStatus === 'skipped' && t.annotations?.filter(a => a.type === 'skip').length === 1 && t.annotations.some(a => a.type === 'skip' && a.description === 'changes the ratio through the Chrome DevTools Protocol');
      requireThat((t.status === 'expected' && t.expectedStatus === 'passed' && t.results[0].status === 'passed') || (skip && t.status === 'skipped' && t.results[0].status === 'skipped'), 'non-passing case');
    }
    requireThat(expected.size === 0, 'missing cases');
  }
}
function metadata() {
  return { head: process.env.GITHUB_SHA, workflow: process.env.GITHUB_WORKFLOW_SHA, run: process.env.GITHUB_RUN_ID, attempt: process.env.GITHUB_RUN_ATTEMPT };
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [mode, root, project] = process.argv.slice(2);
  if (mode === 'receipt') {
    requireThat(projects.includes(project), 'invalid project');
    // Missing reports remain missing evidence; the receipt must still record external outcome.
    const r = { ...metadata(), project, outcome: process.env.SUITE_OUTCOME };
    for (const name of ['inventory', 'report']) {
      try { r[name] = readJSON(root, `${name}.json`); } catch (error) { r[`${name}Error`] = error.message; }
    }
    // Check the owned directory itself even when both inputs are missing.
    for (let p = resolve(root); ; p = dirname(p)) {
      requireThat(!lstatSync(p).isSymbolicLink(), 'symlink in receipt path');
      if (p === dirname(p)) break;
    }
    const data = JSON.stringify(r);
    requireThat(Buffer.byteLength(data) <= limit, 'receipt too large');
    writeFileSync(resolve(root, 'receipt.json'), data, { flag: 'wx' });
  } else if (mode === 'aggregate') {
    validate(projects.map(p => readJSON(root, `${p}/receipt.json`)), metadata(), process.env.BROWSER_RESULT);
    console.log('Both browser projects have complete successful coverage.');
  } else throw new Error('expected receipt or aggregate');
}
