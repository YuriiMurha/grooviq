const { spawnSync } = require("node:child_process");
const { appendFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } = require("node:fs");
const { cpus, platform } = require("node:os");
const { join, resolve } = require("node:path");

const BUDGET_COUNT = 6;
const SAMPLE_COUNT = 5;

function parseSamples(stdout) {
  return [...stdout.matchAll(/roll timing samples \(ms\)\s*(\[[^\]]*\])/g)].map(([, raw]) => {
    try {
      const samples = JSON.parse(raw);
      if (samples.length === SAMPLE_COUNT && samples.every(value => Number.isFinite(value) && value >= 0)) {
        return samples;
      }
    } catch {
      // Keep incomplete reports visible, but never count them as a pass.
    }
    return null;
  });
}

function isLimitFailure(test, samples) {
  if (test.failureMessages?.length !== 1) return false;
  const [message] = test.failureMessages;
  const match = /^AssertionError: expected (\S+) to be less than (\S+)\r?\n\s+at measure \([^\r\n]*?src\/roll\/budgets\.test\.ts:\d+:\d+\)/.exec(message);
  return match !== null && Number(match[1]) === samples[2] && samples[2] >= Number(match[2]);
}

function assessAttempt({ status, stdout, stderr, report }) {
  const samplesMs = parseSamples(stdout);
  const files = report?.testResults;
  const file = files?.[0];
  const tests = Array.isArray(file?.assertionResults) ? file.assertionResults : [];
  const measured = tests.filter(test => test?.ancestorTitles?.includes("piano roll budgets"));
  const completed = measured.filter(test => test.status === "passed" || test.status === "failed");
  const failed = completed.filter(test => test.status === "failed");

  if (!Array.isArray(files) || files.length !== 1 || typeof file?.name !== "string" ||
      !file.name.endsWith("/src/roll/budgets.test.ts") ||
      measured.length !== BUDGET_COUNT || completed.length !== BUDGET_COUNT ||
      report.numPassedTests !== BUDGET_COUNT - failed.length || report.numFailedTests !== failed.length ||
      samplesMs.length !== BUDGET_COUNT || samplesMs.includes(null)) {
    return { outcome: "error", detail: "expected six completed timing budgets with five samples each", samplesMs };
  }

  // The isolated test file runs serially. The reporter lists tests in that same order.
  const budgets = completed.map((test, i) => ({
    name: test.title, status: test.status, samplesMs: samplesMs[i],
  }));
  if (!file.message && !/⎯+ Unhandled Errors ⎯+/.test(stdout + stderr)) {
    if (status === 0 && report.success === true && failed.length === 0) {
      return { outcome: "passed", budgets };
    }
    if (status > 0 && report.success === false && failed.length > 0 &&
        completed.every((test, i) => test.status === "passed" || isLimitFailure(test, samplesMs[i]))) {
      return { outcome: "limit-failure", budgets };
    }
  }
  return { outcome: "error", detail: "test run failed outside the timing assertions", budgets };
}

function runTiming(runAttempt) {
  const first = assessAttempt(runAttempt(1));
  if (first.outcome !== "limit-failure") return { outcome: first.outcome, attempts: [first] };

  const second = assessAttempt(runAttempt(2));
  const outcome = { passed: "contended", "limit-failure": "regression", error: "error" }[second.outcome];
  return { outcome, attempts: [first, second] };
}

function runBenchmark(path) {
  // Remove any report from an earlier invocation before starting the test.
  rmSync(path, { force: true });
  const result = spawnSync("make", ["bench-roll-operations"], {
    cwd: resolve(__dirname, ".."),
    encoding: "utf8",
    env: { ...process.env, ROLL_TIMING_REPORT: path, FORCE_COLOR: "0" },
  });
  const stdout = result.stdout ?? "";
  const stderr = result.stderr ?? "";
  process.stdout.write(stdout);
  process.stderr.write(stderr);
  if (result.error) console.error(result.error.message);
  let report;
  try {
    report = JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    console.error(`Could not read Vitest report: ${error.message}`);
  }
  return { status: result.error ? null : result.status, stdout, stderr, report };
}

function main(outputDir) {
  mkdirSync(outputDir, { recursive: true });
  const hostCpus = cpus();
  const machine = { cpu: hostCpus[0]?.model ?? "unknown", logicalCores: hostCpus.length, os: platform() };
  const result = runTiming(attempt => runBenchmark(join(outputDir, `vitest-attempt-${attempt}.json`)));
  writeFileSync(join(outputDir, "samples.json"), JSON.stringify({ machine, ...result }, null, 2) + "\n");

  const status = {
    passed: "passed: all six timing budgets met",
    contended: "contended: first run failed, rerun passed",
    regression: "regression: timing limits failed on both attempts",
    error: "error: timing run did not complete cleanly",
  }[result.outcome];
  const lines = [
    "## Piano-roll timing", "", status, "",
    "| CPU model | Logical cores |",
    "| --- | ---: |",
    `| ${machine.cpu} | ${machine.logicalCores} |`, "",
    "| Attempt | Budget | Result | Raw samples (ms, median in bold) |",
    "| ---: | --- | --- | --- |",
  ];
  for (const [i, attempt] of result.attempts.entries()) {
    const rows = attempt.budgets ?? attempt.samplesMs.map((samplesMs, index) => ({
      name: `Unmatched sample group ${index + 1}`, status: "unknown", samplesMs,
    }));
    for (const budget of rows) {
      const samples = budget.samplesMs?.map((ms, index) => index === 2 ? `**${ms}**` : String(ms));
      lines.push(`| ${i + 1} | ${budget.name} | ${budget.status} | ${samples?.join(", ") ?? "unavailable"} |`);
    }
  }
  for (const attempt of result.attempts) {
    if (attempt.detail) lines.push("", attempt.detail);
  }
  if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, lines.join("\n") + "\n");

  console.log(`Runner: ${machine.cpu}; ${machine.logicalCores} logical cores`);
  if (result.outcome === "contended") console.log(`::warning::${status}`);
  else console.log(status);
  if (result.outcome === "regression" || result.outcome === "error") process.exitCode = 1;
}

if (require.main === module) {
  if (!process.argv[2]) throw Error("Usage: node scripts/roll-timing.cjs <output-directory>");
  main(resolve(process.argv[2]));
}

module.exports = { assessAttempt, runTiming };
