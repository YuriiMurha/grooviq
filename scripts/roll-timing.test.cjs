const assert = require("node:assert/strict");
const { test } = require("node:test");
const { assessAttempt, runTiming } = require("./roll-timing.cjs");

const makeAttempt = ({ timingFailure = false, status = timingFailure ? 2 : 0 } = {}) => {
  const samples = [0.1, 0.2, timingFailure ? 8 : 0.3, 9, 10];
  const assertions = Array.from({ length: 6 }, (_, i) => {
    const failed = timingFailure && i === 0;
    return {
      title: `budget ${i}`,
      ancestorTitles: ["piano roll budgets"],
      status: failed ? "failed" : "passed",
      failureMessages: failed ? ["AssertionError: expected 8 to be less than 4\n    at measure (src/roll/budgets.test.ts:144:20)"] : [],
    };
  });
  assertions.push({ title: "counted test", ancestorTitles: ["counted"], status: "skipped" });
  return {
    status,
    stdout: Array.from({ length: 6 }, () => `roll timing samples (ms) [\n  ${samples.join(",\n  ")}\n]\n`).join(""),
    stderr: "",
    report: {
      numPassedTests: timingFailure ? 5 : 6,
      numFailedTests: timingFailure ? 1 : 0,
      success: !timingFailure,
      testResults: [{ name: "/checkout/app/ui/src/roll/budgets.test.ts", message: "", assertionResults: assertions }],
    },
  };
};

test("validates all six completed budgets and preserves five raw samples per budget", () => {
  const attempt = makeAttempt();
  const result = assessAttempt(attempt);
  assert.equal(result.outcome, "passed");
  assert.equal(result.budgets.length, 6);
  assert.deepEqual(result.budgets[0], {
    name: "budget 0", status: "passed", samplesMs: [0.1, 0.2, 0.3, 9, 10],
  });

  const oneLine = assessAttempt({ ...attempt, stdout: attempt.stdout.replaceAll("\n  ", " ") });
  assert.deepEqual(oneLine, result);
});

test("retry requires complete results and an actual timing assertion failure", () => {
  const attempt = makeAttempt({ timingFailure: true });
  assert.equal(assessAttempt(attempt).outcome, "limit-failure");

  const failed = attempt.report.testResults[0].assertionResults[0];
  failed.failureMessages.push("AssertionError: expected 9999 to be 10000");
  assert.equal(assessAttempt(attempt).outcome, "error");

  const nonTiming = makeAttempt({ timingFailure: true });
  nonTiming.report.testResults[0].assertionResults[0].failureMessages[0] =
    "AssertionError: expected 8 to be less than 4\n    at unrelated (src/roll/budgets.test.ts:200:10)";
  assert.equal(assessAttempt(nonTiming).outcome, "error");

  const wrongMedian = makeAttempt({ timingFailure: true });
  wrongMedian.report.testResults[0].assertionResults[0].failureMessages[0] =
    "AssertionError: expected 9 to be less than 4\n    at measure (src/roll/budgets.test.ts:144:20)";
  assert.equal(assessAttempt(wrongMedian).outcome, "error");
});

test("missing, partial, or invalid evidence cannot pass or trigger a retry", () => {
  const attempt = makeAttempt();
  const empty = { numPassedTests: 0, numFailedTests: 0, success: true, testResults: [] };
  const timingFailure = makeAttempt({ timingFailure: true });
  for (const incomplete of [
    { ...attempt, report: empty },
    { ...attempt, report: null },
    { ...attempt, report: { testResults: [null] } },
    { ...attempt, stdout: attempt.stdout.replace("[\n  0.1,", "[\n  NaN,") },
    { ...attempt, stdout: attempt.stdout.slice(attempt.stdout.indexOf("]", 1) + 1) },
    { ...attempt, status: 2 },
    { ...timingFailure, status: null },
    { ...timingFailure, stderr: "⎯⎯ Unhandled Errors ⎯⎯" },
  ]) assert.equal(assessAttempt(incomplete).outcome, "error");
});

test("a limit failure gets one rerun and only a clean rerun is contended", () => {
  const pass = () => makeAttempt();
  const limitFailure = () => makeAttempt({ timingFailure: true });
  const error = () => makeAttempt({ status: 2 });

  for (const [first, second, outcome, count] of [
    [pass, error, "passed", 1],
    [limitFailure, pass, "contended", 2],
    [limitFailure, limitFailure, "regression", 2],
    [limitFailure, error, "error", 2],
    [error, pass, "error", 1],
  ]) {
    let calls = 0;
    const result = runTiming(() => [first, second][calls++]());
    assert.equal(calls, count);
    assert.equal(result.attempts.length, count);
    assert.equal(result.outcome, outcome);
  }
});
