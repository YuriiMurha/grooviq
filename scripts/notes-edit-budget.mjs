#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const testName = "ten_thousand_notes_edit_in_under_50_ms";
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

/** Cargo exits zero even when an exact filter selects nothing. Require the named pass
 * and exactly one non-ignored passing test; accept Windows harness line endings too. */
export function onePassingTest(output, name) {
  const lines = output.split(/\r?\n/);
  const summaries = lines.filter((line) => line.startsWith("test result:"));
  return lines.includes(`test ${name} ... ok`)
    && summaries.length === 1
    && /^test result: ok\. 1 passed; 0 failed; 0 ignored;/.test(summaries[0]);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const result = spawnSync("cargo", [
    "test", "--release", "-p", "grooviq-project", "--test", "notes", testName,
    "--", "--ignored", "--exact", "--nocapture", "--color", "never",
  ], { cwd: root, encoding: "utf8", stdio: ["inherit", "pipe", "inherit"] });
  process.stdout.write(result.stdout ?? "");
  if (result.error || result.signal || result.status !== 0) {
    console.error(result.error?.message ?? `Note-edit budget stopped: ${result.signal ?? result.status}`);
    process.exitCode = result.status || 1;
  } else if (!onePassingTest(result.stdout, testName)) {
    console.error(`Expected exactly one passing ${testName} test; the required budget did not run.`);
    process.exitCode = 1;
  }
}
