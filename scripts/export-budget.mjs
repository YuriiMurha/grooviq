#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const testName = "export_is_faster_than_real_time";
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

import { onePassingTest } from "./notes-edit-budget.mjs";

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const result = spawnSync("cargo", [
    "test", "--release", "-p", "grooviq-engine", "--test", "export", testName,
    "--", "--ignored", "--exact", "--nocapture", "--color", "never",
  ], { cwd: root, encoding: "utf8", stdio: ["inherit", "pipe", "inherit"] });
  process.stdout.write(result.stdout ?? "");
  if (result.error || result.signal || result.status !== 0) {
    console.error(result.error?.message ?? `Export budget stopped: ${result.signal ?? result.status}`);
    process.exitCode = result.status || 1;
  } else if (!onePassingTest(result.stdout, testName)) {
    console.error(`Expected exactly one passing ${testName} test; the required budget did not run.`);
    process.exitCode = 1;
  }
}
