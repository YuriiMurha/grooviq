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
const FILE_STATUSES = new Set(["added", "removed", "modified", "renamed", "copied", "changed", "unchanged"]);
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
  if (typeof path !== "string") return true;
  if (SHARED_FILES.has(path) || WORKFLOW_FILES[workflow].has(path) ||
      /^rust-toolchain[^/]*$/.test(path) || under(path, ".cargo") || under(path, ".config")) return true;
  if ([...inputs[workflow]].some(dir => under(path, dir))) return true;
  return !UNRELATED_FILES.has(path) &&
    !workspaceDirs.some(dir => under(path, dir)) &&
    !UNRELATED_DIRS.some(dir => under(path, dir));
}

async function shouldRun({ workflow, eventName, pullRequest, repo, rest, paginate, load = loadPolicy }) {
  if (eventName !== "pull_request" || !Object.hasOwn(ROOT_PACKAGES, workflow)) return true;
  try {
    const args = { ...repo, pull_number: pullRequest.number };
    const files = await paginate(rest.pulls.listFiles, { ...args, per_page: 100 });
    // GitHub caps this endpoint at 3,000 files. Compare both snapshots before allowing a skip.
    if (!Array.isArray(files) || !files.length || files.length >= 3000 ||
        files.length !== pullRequest.changed_files) return true;
    const { data: current } = await rest.pulls.get(args);
    if (!pullRequest.head.sha || !pullRequest.base.sha ||
        current.head.sha !== pullRequest.head.sha || current.base.sha !== pullRequest.base.sha ||
        current.changed_files !== files.length) return true;

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
