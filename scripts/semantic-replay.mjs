#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { onePassingTest } from "./notes-edit-budget.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const replays = [
  { crate: "grooviq-project", suite: "semantic_replay", name: "session_matches_shared_semantic_replay" },
  { crate: "grooviq-app", suite: "arrangement_media_replay", name: "arrangement_media_matches_shared_replay" },
  { crate: "grooviq-app", suite: "midi", name: "semantic_replay::hub_matches_shared_midi_replay" },
];
for (const { crate, suite, name } of replays) {
  const result = spawnSync("cargo", [
    "test", "-p", crate, "--all-features", "--test", suite,
    "--", name, "--exact", "--nocapture", "--color", "never",
  ], { cwd: root, encoding: "utf8", stdio: ["inherit", "pipe", "inherit"] });
  process.stdout.write(result.stdout ?? "");
  if (result.error || result.signal || result.status !== 0) {
    console.error(result.error?.message ?? `Replay ${name} stopped: ${result.signal ?? result.status}`);
    process.exitCode = result.status || 1;
    break;
  }
  if (!onePassingTest(result.stdout, name)) {
    console.error(`Expected exactly one passing ${name} test; the real replay did not run.`);
    process.exitCode = 1;
    break;
  }
}
