import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

export const allowed = new Set([
  'MIT', 'Apache-2.0', 'Apache-2.0 WITH LLVM-exception', 'BlueOak-1.0.0', 'CC-BY-4.0', 'BSD-2-Clause', 'BSD-3-Clause', '0BSD', 'ISC',
  'MPL-2.0', 'Zlib', 'Unicode-3.0', 'Unicode-DFS-2016',
  'LGPL-2.1-only', 'LGPL-2.1-or-later', 'LGPL-3.0-only', 'LGPL-3.0-or-later',
  'GPL-2.0-only', 'GPL-2.0-or-later', 'GPL-3.0-only', 'GPL-3.0-or-later', 'OFL-1.1',
]);

// Parse the complete expression even when an OR branch already passes.
// Unapproved SPDX exceptions and non-SPDX metadata fail closed.
export function licenseAllowed(expression) {
  if (typeof expression !== 'string') return false;
  const tokens = expression.match(/\(|\)|[^\s()]+/g) ?? [];
  let position = 0;
  function atom() {
    const token = tokens[position++];
    if (token === '(') {
      const value = or();
      if (tokens[position++] !== ')') throw new Error('Unclosed expression');
      return value;
    }
    if (!token || ['AND', 'OR', ')'].includes(token)) throw new Error('Expected licence');
    let license = token;
    if (tokens[position] === 'WITH') {
      position++;
      const exception = tokens[position++];
      if (!exception || ['AND', 'OR', 'WITH', '(', ')'].includes(exception)) throw new Error('Expected exception');
      license += ` WITH ${exception}`;
    }
    return allowed.has(license);
  }
  function and() {
    let value = atom();
    while (tokens[position] === 'AND') { position++; const right = atom(); value = value && right; }
    return value;
  }
  function or() {
    let value = and();
    while (tokens[position] === 'OR') { position++; const right = and(); value = value || right; }
    return value;
  }
  try { const value = or(); return position === tokens.length && value; } catch { return false; }
}

export function licenseFailures(lock) {
  if (lock.lockfileVersion !== 3 || !lock.packages || !Object.keys(lock.packages).length) {
    throw new Error('Expected a nonempty npm v3 packages lockfile');
  }
  return Object.entries(lock.packages).filter(([, pkg]) => !licenseAllowed(pkg.license))
    .map(([path, pkg]) => `${path || '(root)'}@${pkg.version}: ${pkg.license ?? 'UNKNOWN'}`);
}

export function auditFailures(report, status) {
  if (report.error || !report.metadata?.vulnerabilities || !report.vulnerabilities ||
      ![0, 1].includes(status) || report.auditReportVersion !== 2) {
    throw new Error(`npm audit failed or returned an unsupported report (exit ${status})`);
  }
  const findings = Object.entries(report.vulnerabilities).map(([name, finding]) => `${name}: ${finding.severity}`);
  const total = report.metadata.vulnerabilities.total;
  if (!Number.isInteger(total) || total < 0 || (total > 0 && !findings.length) || (status !== 0 && !findings.length)) {
    throw new Error('Inconsistent npm audit report');
  }
  return findings;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    const ui = fileURLToPath(new URL('../../app/ui/', import.meta.url));
    let failures;
    if (process.argv[2] === 'licenses') {
      failures = licenseFailures(JSON.parse(readFileSync(`${ui}/package-lock.json`, 'utf8')));
    } else if (process.argv[2] === 'report') {
      const lock = JSON.parse(readFileSync(`${ui}/package-lock.json`, 'utf8'));
      console.log('# npm dependency licences\n\nPackage path | Version | Licence | Scope\n--- | --- | --- | ---');
      for (const [path, pkg] of Object.entries(lock.packages)) {
        console.log(`${path || '(root)'} | ${pkg.version} | ${pkg.license ?? 'UNKNOWN'} | ${pkg.dev ? 'build/test only' : 'production dependency (bundling varies)'}`);
      }
      process.exitCode = licenseFailures(lock).length ? 1 : 0;
      process.exit(process.exitCode);
    } else if (process.argv[2] === 'audit') {
      // npm includes dev and optional dependencies; no severity downgrade or omit flag.
      const result = spawnSync('npm', ['audit', '--json', '--include=dev', '--include=optional', '--registry=https://registry.npmjs.org', '--fetch-retries=0', '--fetch-timeout=60000'], { cwd: ui, encoding: 'utf8' });
      if (result.error) throw result.error;
      failures = auditFailures(JSON.parse(result.stdout), result.status);
    } else throw new Error('Usage: node scripts/dependency-policy/npm-policy.mjs licenses|audit|report');
    if (failures.length) throw new Error(failures.join('\n'));
    console.log('Dependency policy passed');
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
