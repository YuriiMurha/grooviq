const { execFileSync } = require("node:child_process");
const { homedir } = require("node:os");
const { delimiter, dirname, isAbsolute, join, relative, resolve, sep } = require("node:path");

const ROOT_PACKAGES = {
  rtsan: ["grooviq-engine"],
  perf: ["grooviq-golden", "grooviq-dsp"],
};
const SHARED_FILES = new Set([
  "Cargo.toml", "Cargo.lock", "Makefile", "scripts/optional-rust-checks.cjs",
  "scripts/optional-rust-checks.test.cjs",
]);
const WORKFLOW_FILES = {
  rtsan: new Set([".github/workflows/rtsan.yml"]),
  perf: new Set([
    ".github/workflows/perf.yml", "scripts/capture-instruction-profiles.py",
    "scripts/perf-fetch.py", "scripts/test_perf_fetch.py",
  ]),
};
const UNRELATED_DIRS = [
  "app/ui", "docs", "scripts", "tools", ".githooks", ".github/workflows", ".github/ISSUE_TEMPLATE",
];
const UNRELATED_FILES = new Set([
  "AGENTS.md", "CLAUDE.md", "LICENSE", "NOTICE", "README.md", "deny.toml", ".editorconfig",
  ".gitattributes", ".gitignore", ".github/pull_request_template.md",
]);
const ROLL_UNRELATED_RUST_FILES = new Set([
  "Cargo.toml", "Cargo.lock", "rustfmt.toml", "clippy.toml", ".config/nextest.toml",
  ".github/workflows/rtsan.yml",
]);
const FILE_STATUSES = new Set(["added", "removed", "modified", "renamed", "copied", "changed", "unchanged"]);
const BATCH_BRANCH = "merge-train/batch";
const under = (path, dir) => path === dir || path.startsWith(`${dir}/`);

function readCargoMetadata() {
  const output = execFileSync("cargo", ["metadata", "--no-deps", "--format-version", "1", "--locked", "--offline"], {
    cwd: resolve(__dirname, ".."),
    encoding: "utf8",
    env: { ...process.env, PATH: [process.env.PATH, join(homedir(), ".cargo", "bin")].join(delimiter) },
    timeout: 120_000,
  });
  return JSON.parse(output);
}

function loadPolicy(metadata = readCargoMetadata()) {
  const { packages, workspace_root: root } = metadata;
  const byPath = new Map(packages.map(pkg => [dirname(pkg.manifest_path), pkg]));
  const byName = new Map(packages.map(pkg => [pkg.name, pkg]));
  const dirs = new Map(packages.map(pkg => {
    const dir = relative(root, dirname(pkg.manifest_path)).split(sep).join("/");
    if (!dir || isAbsolute(dir) || under(dir, "..")) {
      throw Error(`Cannot classify workspace package ${pkg.name}`);
    }
    return [pkg, dir];
  }));

  const inputs = {};
  for (const [workflow, roots] of Object.entries(ROOT_PACKAGES)) {
    const pending = roots.map(name => byName.get(name));
    const visited = new Set();
    while (pending.length) {
      const pkg = pending.pop();
      if (!pkg) throw Error(`Unmapped local package in ${workflow} dependency graph`);
      if (visited.has(pkg)) continue;
      visited.add(pkg);
      // Use paths so renamed dependencies and dependencies for other targets count too.
      for (const dep of pkg.dependencies) {
        if (dep.path) pending.push(byPath.get(dep.path));
      }
    }
    inputs[workflow] = new Set([...visited].map(pkg => dirs.get(pkg)));
  }
  return { inputs, workspaceDirs: [...dirs.values()] };
}

function isInput(path, workflow, { inputs, workspaceDirs }) {
  if (typeof path !== "string" || path.split("/").some(part => !part || part === "." || part === "..")) return true;
  if (workflow === "roll") {
    // The roll runs in Vitest without the engine. Keep unmapped paths in scope.
    if (under(path, "app/ui")) return !under(path, "app/ui/e2e");
    return !UNRELATED_FILES.has(path) && !ROLL_UNRELATED_RUST_FILES.has(path) &&
      !/^rust-toolchain[^/]*$/.test(path) && !under(path, ".cargo") &&
      !under(path, "docs") && !workspaceDirs.some(dir => under(path, dir));
  }
  if (SHARED_FILES.has(path) || WORKFLOW_FILES[workflow].has(path) ||
      /^rust-toolchain[^/]*$/.test(path) || under(path, ".cargo") || under(path, ".config")) return true;
  if ([...inputs[workflow]].some(dir => under(path, dir))) return true;
  return !UNRELATED_FILES.has(path) &&
    !workspaceDirs.some(dir => under(path, dir)) &&
    !UNRELATED_DIRS.some(dir => under(path, dir));
}

async function listPullFiles({ pullRequest, repo, rest, paginate }) {
  const args = { ...repo, pull_number: pullRequest.number };
  const files = await paginate(rest.pulls.listFiles, { ...args, per_page: 100 });
  // GitHub caps this endpoint at 3,000 files. Compare both snapshots before allowing a skip.
  if (!Array.isArray(files) || !files.length || files.length >= 3000 ||
      files.length !== pullRequest.changed_files) return null;
  const { data: current } = await rest.pulls.get(args);
  if (!pullRequest.head.sha || !pullRequest.base.sha ||
      current.head.sha !== pullRequest.head.sha || current.base.sha !== pullRequest.base.sha ||
      current.changed_files !== files.length) return null;
  return files;
}

async function listBatchFiles({ after, repo, rest, paginate }) {
  if (!/^[a-f0-9]{40}$/.test(after ?? "") || after === "0".repeat(40)) return null;
  const files = await paginate(rest.repos.compareCommitsWithBasehead, {
    ...repo, basehead: `main...${after}`, per_page: 1,
  }, ({ data }, done) => {
    // The first compare page lists files for the entire diff. Later pages only list commits.
    done();
    if (!Array.isArray(data.files)) throw Error("Missing compare file list");
    return data.files;
  });
  // The compare API has a hard cap of 300 files and exposes no total file count.
  if (files.length >= 300) return null;
  const { data } = await rest.git.getRef({ ...repo, ref: `heads/${BATCH_BRANCH}` });
  return data.object.sha === after ? files : null;
}

async function shouldRun(request) {
  const { workflow, eventName, ref, load = loadPolicy } = request;
  if (workflow !== "roll" && !Object.hasOwn(ROOT_PACKAGES, workflow)) return true;
  const isBatchPush = workflow === "rtsan" && eventName === "push" && ref === `refs/heads/${BATCH_BRANCH}`;
  if (eventName !== "pull_request" && !isBatchPush) return true;
  try {
    const files = await (isBatchPush ? listBatchFiles(request) : listPullFiles(request));
    if (!files?.length) return true;
    const policy = load();
    const filenames = new Set();
    for (const file of files) {
      if (!FILE_STATUSES.has(file?.status) || filenames.has(file.filename)) return true;
      filenames.add(file.filename);
      if (isInput(file.filename, workflow, policy) ||
          (file.status === "renamed" && isInput(file.previous_filename, workflow, policy))) return true;
    }
    return false;
  } catch {
    return true;
  }
}

module.exports = { readCargoMetadata, loadPolicy, shouldRun };
