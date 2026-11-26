import { appendFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { readJSON, validate } from '../e2e-browser-results.mjs';
import { cases } from './run.mjs';

const browsers = ['webkit', 'chromium'];
const shards = [1, 2];

export function receiptPath(browser, shard, { run, attempt }, repetition) {
  return `ci-sharded-${browser}-${shard}-of-2-${run}-${attempt}/${repetition}/receipt.json`;
}

// Diagnostics run even when the matrix or shared validator fails.
function summarize(receipt, row, label, errors) {
  if (receipt.outcome !== 'success') errors.push(`${label}: suite outcome ${receipt.outcome ?? 'missing'}`);
  for (const error of receipt.report?.errors ?? []) {
    errors.push(`${label}: ${error.message ?? 'Playwright report error'}`);
  }
  for (const { spec, test } of cases(receipt.report ?? {})) {
    const results = test.results ?? [];
    const status = results[0]?.status ?? 'missing attempt';
    const annotations = test.annotations ?? [];
    const clean = results.length === 1 && results[0].retry === 0 &&
      !annotations.some(a => a.type === 'fixme' || a.type === 'fail');
    const skips = annotations.filter(a => a.type === 'skip');
    const allowedSkip = test.projectName === 'webkit' && spec.file === 'm2-panels.e2e.ts' &&
      spec.title === 'a_dpr_change_redraws_sharp' && skips.length === 1 &&
      skips[0].description === 'changes the ratio through the Chrome DevTools Protocol';
    row.cases++;
    if (clean && status === 'passed' && test.status === 'expected' && test.expectedStatus === 'passed') {
      row.passed++;
    } else if (clean && status === 'skipped' && test.status === 'skipped' &&
      test.expectedStatus === 'skipped' && allowedSkip) {
      row.skipped++;
    } else {
      row.failed++;
      const detail = results.length > 1 || results.some(result => result.retry > 0) ? 'retry' : status;
      errors.push(`${label}: ${spec.file}:${spec.line} ${spec.title} (${detail}; expected ${test.expectedStatus})`);
    }
  }
}

export function aggregate(root, meta, repeats, dependency) {
  if (!Number.isSafeInteger(repeats) || repeats < 1 || repeats > 20) throw Error('expected 1–20 repetitions');
  const rows = [];
  const errors = [];
  if (dependency !== 'success') errors.push(`Browser matrix outcome: ${dependency || 'missing'}`);

  for (let repetition = 1; repetition <= repeats; repetition++) {
    const receipts = [];
    for (const browser of browsers) {
      const row = { repetition, browser, cases: 0, passed: 0, skipped: 0, failed: 0 };
      rows.push(row);
      for (const shard of shards) {
        const label = `Repetition ${repetition}, ${browser} shard ${shard}/2`;
        try {
          const receipt = readJSON(root, receiptPath(browser, shard, meta, repetition));
          if (receipt?.project !== browser || receipt.shard?.index !== shard || receipt.shard?.total !== 2) {
            throw Error('receipt does not match its browser and shard path');
          }
          receipts.push(receipt);
          summarize(receipt, row, label, errors);
        } catch (error) {
          errors.push(`${label}: ${error.message}`);
        }
      }
    }
    try {
      // The matrix status is checked above. Always account for the repetitions that reached disk.
      validate(receipts, meta, 'success');
    } catch (error) {
      errors.push(`Repetition ${repetition}: ${error.message}`);
    }
  }
  return { ok: errors.length === 0, rows, errors };
}

export function renderSummary({ ok, rows, errors }, { head, workflow, run, attempt }) {
  const lines = [
    '# CI-sharded browser stability',
    '',
    `Candidate \`${head}\` · Workflow \`${workflow}\` · Run ${run}, attempt ${attempt} · **${ok ? 'Passed' : 'Failed'}**`,
    '',
    '| Repetition | Browser | Cases | Passed | Skipped | Failed |',
    '| ---: | --- | ---: | ---: | ---: | ---: |',
    ...rows.map(r => `| ${r.repetition} | ${r.browser} | ${r.cases} | ${r.passed} | ${r.skipped} | ${r.failed} |`),
  ];
  if (errors.length) {
    lines.push('', '## Failures', '', ...errors.map(error => `    ${error.replace(/[\r\n]/g, ' ')}`));
  }
  return lines.join('\n') + '\n';
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [root, output] = process.argv.slice(2);
  const { CANDIDATE, GITHUB_WORKFLOW_SHA, GITHUB_RUN_ID, GITHUB_RUN_ATTEMPT, STABILITY_REPEATS,
    BROWSER_RESULT, GITHUB_STEP_SUMMARY } = process.env;
  const meta = { head: CANDIDATE, workflow: GITHUB_WORKFLOW_SHA, run: GITHUB_RUN_ID, attempt: GITHUB_RUN_ATTEMPT };
  const result = aggregate(root, meta, Number(STABILITY_REPEATS), BROWSER_RESULT);
  writeFileSync(output, renderSummary(result, meta));
  // GitHub caps the step summary at 1 MiB. Keep the complete diagnostics in the artifact.
  const summary = renderSummary({ ...result, errors: result.errors.slice(0, 100).map(error => error.slice(0, 500)) }, meta);
  const extra = result.errors.length ? '\nSee the summary artifact for complete failure details.\n' : '';
  appendFileSync(GITHUB_STEP_SUMMARY, summary + extra);
  if (!result.ok) process.exitCode = 1;
}
