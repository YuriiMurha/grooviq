import assert from "node:assert/strict";
import { test } from "node:test";
import { onePassingTest } from "./notes-edit-budget.mjs";

const pass = "test budget ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 17 filtered out\n";

test("accepts the named pass on macOS and Windows", () => {
  assert.equal(onePassingTest(pass, "budget"), true);
  assert.equal(onePassingTest(pass.replaceAll("\n", "\r\n"), "budget"), true);
});

test("rejects a renamed test's successful zero-test Cargo exit", () => {
  assert.equal(onePassingTest("running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 18 filtered out\n", "budget"), false);
  assert.equal(onePassingTest(pass, "renamed_budget"), false);
});

test("rejects failure, ignored tests, extra tests and missing harness evidence", () => {
  for (const output of [
    pass.replace("1 passed; 0 failed", "0 passed; 1 failed"),
    pass.replace("0 ignored", "1 ignored"),
    pass.replace("1 passed", "2 passed"),
    "test budget ... ok\n",
    pass + "test result: ok. 0 passed; 0 failed; 0 ignored;\n",
  ]) assert.equal(onePassingTest(output, "budget"), false);
});
