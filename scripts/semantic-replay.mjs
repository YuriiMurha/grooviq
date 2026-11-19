#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { onePassingTest } from "./notes-edit-budget.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const name = "session_matches_shared_semantic_replay";
const result = spawnSync("cargo", [
  "test", "-p", "grooviq-project", "--test", "semantic_replay",
  "--", "--nocapture", "--color", "never",
], { cwd: root, encoding: "utf8", stdio: ["inherit", "pipe", "inherit"] });
process.stdout.write(result.stdout ?? "");
if (result.error || result.signal || result.status !== 0) {
  console.error(result.error?.message ?? `Real Session replay stopped: ${result.signal ?? result.status}`);
  process.exitCode = result.status || 1;
} else if (!onePassingTest(result.stdout, name)) {
  console.error(`Expected exactly one passing ${name} test; the real Session replay did not run.`);
  process.exitCode = 1;
}
