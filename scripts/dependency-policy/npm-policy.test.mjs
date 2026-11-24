import { test } from 'node:test';
import assert from 'node:assert/strict';
import { licenseAllowed, licenseFailures, auditFailures, allowed } from './npm-policy.mjs';

test('allowed licences and SPDX boolean expressions', () => {
  for (const license of allowed) assert.equal(licenseAllowed(license), true, license);
  for (const license of ['(MIT OR Apache-2.0) AND ISC', 'MIT OR Proprietary', 'Proprietary OR MIT']) {
    assert.equal(licenseAllowed(license), true, license);
  }
});
test('prohibited, unknown and malformed licences fail closed', () => {
  for (const license of [undefined, '', 'Proprietary', 'MIT AND Proprietary', 'MIT WITH Unknown', 'MIT OR', '(MIT', 'MIT)', 'MIT junk', 'MIT WITH LLVM-exception', 'Apache-2.0 WITH Unknown', 'Apache-2.0 WITH LLVM-exception WITH LLVM-exception', 'Apache-2.0 WITH', 'BlueOak-1.0.1', 'CC-BY-3.0']) {
    assert.equal(licenseAllowed(license), false, String(license));
  }
  assert.throws(() => licenseFailures({}));
  assert.deepEqual(licenseFailures({ lockfileVersion: 3, packages: {
    'node_modules/a': { version: '1', license: 'MIT' },
    'node_modules/b': { version: '2' },
  } }), ['node_modules/b@2: UNKNOWN']);
});
const report = (vulnerabilities = {}) => ({ auditReportVersion: 2, vulnerabilities,
  metadata: { vulnerabilities: { total: Object.keys(vulnerabilities).length } } });
test('audit rejects findings at every severity, regardless of npm exit threshold', () => {
  assert.deepEqual(auditFailures(report(), 0), []);
  for (const severity of ['info', 'low', 'moderate', 'high', 'critical']) {
    for (const status of [0, 1]) assert.deepEqual(auditFailures(report({ dep: { severity } }), status), [`dep: ${severity}`]);
  }
});
test('registry failures and unusable audit reports never pass', () => {
  for (const value of [{}, { error: { code: 'ENETUNREACH' } }, { ...report(), auditReportVersion: 1 }]) {
    assert.throws(() => auditFailures(value, 1));
  }
  for (const status of [null, 2, 1]) assert.throws(() => auditFailures(report(), status));
  assert.throws(() => auditFailures({ ...report(), metadata: { vulnerabilities: { total: 1 } } }, 0));
});

test('approved exception composes with SPDX operators without broadening permission', () => {
  assert.equal(licenseAllowed('(Apache-2.0 WITH LLVM-exception OR MIT) AND ISC'), true);
  assert.equal(licenseAllowed('Apache-2.0 WITH LLVM-exception AND Proprietary'), false);
  assert.equal(licenseAllowed('MIT WITH LLVM-exception AND ISC'), false);
});
