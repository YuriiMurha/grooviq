#!/usr/bin/env node
// Runs only the checks that the changes against origin/main need.
// `--plan` prints the plan and runs nothing. Any failed, skipped or interrupted check fails the run.
// Checks run on the working tree, so stage what you checked.
import { spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const usage = "Usage: node scripts/check-changed.mjs [--plan] [--base <ref>]";
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const cargoBin = join(homedir(), ".cargo", "bin");
const env = { ...process.env, PATH: `${cargoBin}:${process.env.PATH ?? ""}` };

let planOnly = false;
let base = "origin/main";
const argv = process.argv.slice(2);
for (let i = 0; i < argv.length; i++) {
  if (argv[i] === "--plan") planOnly = true;
  else if (argv[i] === "--base" && argv[i + 1]) base = argv[++i];
  else fail(usage);
}

function fail(message) {
  console.error(message);
  process.exit(2);
}

function git(args) {
  const result = spawnSync("git", args, { cwd: root, encoding: "utf8" });
  if (result.status !== 0) fail(`git ${args.join(" ")} failed:\n${result.stderr.trim()}`);
  return result.stdout;
}

const lines = (text) => text.split("\n").filter(Boolean);

// Committed, staged, unstaged and untracked changes since the merge base with `base`, listed
// separately: a staged change that the working tree undoes is still in the next commit.
const mergeBase = git(["merge-base", base, "HEAD"]).trim();
const changed = [
  ...new Set([
    ...lines(git(["diff", "--name-only", "--no-renames", mergeBase, "HEAD"])),
    ...lines(git(["diff", "--name-only", "--no-renames", "--cached"])),
    ...lines(git(["diff", "--name-only", "--no-renames"])),
    ...lines(git(["ls-files", "--others", "--exclude-standard"])),
  ]),
].sort();

// Workspace crates by directory, and who depends on whom, from Cargo itself.
function workspaceCrates() {
  const result = spawnSync("cargo", ["metadata", "--format-version", "1", "--no-deps"], {
    cwd: root,
    env,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.status !== 0) fail(`cargo metadata failed:\n${result.stderr}`);
  const packages = JSON.parse(result.stdout).packages;
  const names = new Set(packages.map((p) => p.name));
  return packages.map((p) => ({
    name: p.name,
    dir: relative(root, dirname(p.manifest_path)),
    deps: p.dependencies.map((d) => d.name).filter((d) => names.has(d)),
  }));
}

const crates = workspaceCrates();

/** The crate plus every workspace crate that depends on it, directly or not. */
function withDependents(name) {
  const found = new Set([name]);
  let grew = true;
  while (grew) {
    grew = false;
    for (const c of crates)
      if (!found.has(c.name) && c.deps.some((d) => found.has(d))) {
        found.add(c.name);
        grew = true;
      }
  }
  return found;
}

const under = (path, dir) => path === dir || path.startsWith(`${dir}/`);
const RUST_WIDE = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "rustfmt.toml", "clippy.toml"];
const CI = [".github/workflows/"];
// Read by people, never by a build, test or check.
const INERT = [/\.md$/, /^LICENSE$/, /^docs\//, /^\.github\/(ISSUE_TEMPLATE\/|pull_request_template\.md$)/, /^\.(gitignore|gitattributes|editorconfig)$/];
const TOOLING = [/^Makefile$/, /^scripts\//, /^\.githooks\//];

let workspace = false;
const rustCrates = new Set();
let ui = false;
let tooling = false;
const reasons = [];
const inert = [];

for (const path of changed) {
  if (RUST_WIDE.includes(path) || CI.some((dir) => path.startsWith(dir))) {
    workspace = true;
    ui = true;
    reasons.push(`${path}: whole workspace`);
    continue;
  }
  // The engine writes this stream fixture and the UI decodes it.
  if (under(path, "tests/fixtures")) {
    for (const c of withDependents("grooviq-engine")) rustCrates.add(c);
    ui = true;
    reasons.push(`${path}: engine and UI share it`);
    continue;
  }
  if (under(path, "app/ui")) {
    ui = true;
    reasons.push(`${path}: UI`);
    continue;
  }
  const crate = crates
    .filter((c) => c.dir && under(path, c.dir))
    .sort((a, b) => b.dir.length - a.dir.length)[0];
  if (crate) {
    for (const c of withDependents(crate.name)) rustCrates.add(c);
    reasons.push(`${path}: ${crate.name} and its dependents`);
    continue;
  }
  if (TOOLING.some((re) => re.test(path))) {
    tooling = true;
    reasons.push(`${path}: tooling syntax`);
    continue;
  }
  if (INERT.some((re) => re.test(path))) {
    inert.push(path);
    continue;
  }
  // Unknown ground: check everything rather than guess.
  workspace = true;
  ui = true;
  reasons.push(`${path}: unmapped path, whole workspace`);
}

const steps = [];
const step = (name, cmd, args, cwd = root) => steps.push({ name, cmd, args, cwd });
const uiDir = join(root, "app", "ui");
const needsUi = ui || workspace || rustCrates.has("grooviq-app");

const manifests = ["app/ui/package.json", "app/ui/package-lock.json"].filter((m) => changed.includes(m));
if (needsUi && (manifests.length || !existsSync(join(uiDir, "node_modules"))))
  step(
    `ui install (${manifests.length ? `${manifests.join(", ")} changed` : "node_modules missing"})`,
    "npm",
    ["ci", "--no-audit", "--no-fund"],
    uiDir,
  );
if (ui) {
  step("ui lint", "npm", ["run", "lint"], uiDir);
  step("ui typecheck", "npm", ["run", "typecheck"], uiDir);
  step("ui tests", "npm", ["test", "--", "--run"], uiDir);
  step("semantic replay browser", "npx", ["playwright", "install", "chromium"], uiDir);
  step("semantic replay", "npm", ["run", "e2e", "--", "--config", "playwright.replay.config.ts", "--project", "chromium"], uiDir);
}
// The app crate's build script embeds the built UI, so it needs app/ui/dist.
if (ui || workspace || rustCrates.has("grooviq-app")) step("ui build", "npm", ["run", "build"], uiDir);

if (workspace || rustCrates.size > 0) {
  step("rust fmt", "cargo", ["fmt", "--all", "--check"]);
  const scope = workspace ? ["--workspace"] : [...rustCrates].sort().flatMap((c) => ["-p", c]);
  step("rust clippy", "cargo", ["clippy", ...scope, "--all-targets", "--all-features", "--", "-D", "warnings"]);
  step("rust tests", "cargo", ["test", ...scope, "--all-features"]);
}
if (workspace || rustCrates.has("grooviq-project") || changed.some((path) => path.startsWith("scripts/notes-edit-budget."))) {
  step("notes budget runner tests", "node", ["--test", "scripts/notes-edit-budget.test.mjs"]);
  step("notes edit budget", "node", ["scripts/notes-edit-budget.mjs"]);
}
if (tooling) {
  step("tooling: check-changed plans", "node", ["scripts/check-changed.mjs", "--plan", "--base", base]);
  if (existsSync(join(root, ".githooks", "pre-commit")))
    step("tooling syntax: pre-commit hook", "sh", ["-n", ".githooks/pre-commit"]);
  step("tooling syntax: Makefile", "make", ["-n", "help"]);
}

console.log(`Base ${base} (${mergeBase.slice(0, 10)}); ${changed.length} changed paths.`);
for (const reason of reasons) console.log(`  ${reason}`);
if (inert.length) console.log(`  no checks needed: ${inert.join(", ")}`);
console.log(steps.length ? "\nPlan:" : "\nPlan: nothing to check.");
for (const s of steps) console.log(`  ${s.name}: ${s.cmd} ${s.args.join(" ")}`);
if (planOnly) process.exit(0);

let interrupted = false;
let child = null;
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"])
  process.on(signal, () => {
    interrupted = true;
    child?.kill(signal);
  });

const results = [];
for (const s of steps) {
  if (interrupted) break;
  console.log(`\n=== ${s.name}`);
  const status = await new Promise((done) => {
    child = spawn(s.cmd, s.args, { cwd: s.cwd, env, stdio: "inherit" });
    child.on("error", (error) => done(`could not start (${error.message})`));
    child.on("exit", (code, signal) => done(signal ? `killed by ${signal}` : code));
  });
  child = null;
  results.push({ name: s.name, status });
  if (status !== 0) break;
}

console.log("\nResult:");
for (const s of steps) {
  const r = results.find((x) => x.name === s.name);
  const verdict = !r ? "NOT RUN" : r.status === 0 ? "pass" : `FAIL (${r.status})`;
  console.log(`  ${verdict.padEnd(10)} ${s.name}`);
}
const passed = results.length === steps.length && results.every((r) => r.status === 0) && !interrupted;
if (interrupted) console.log("Interrupted: this is not a pass.");
process.exit(passed ? 0 : interrupted ? 130 : 1);
