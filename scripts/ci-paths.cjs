const ALL_JOBS = { rust: "true", wasm: "true", e2e: "true" };
const RUST_CRATE_DIRS = ["dsp/", "engine/", "project/", "tests/golden/", "app/src-tauri/", "tools/qa-fixture/"];
const SHARED_DIRS = [".github/", ".config/", ".cargo/", ".githooks/", "scripts/", "tests/fixtures/"];
const FILE_STATUSES = new Set(["added", "removed", "modified", "renamed", "copied", "changed", "unchanged"]);
const UI_BUILD_FILES = new Set(["app/ui/package.json", "app/ui/package-lock.json", "app/ui/index.html"]);
const UI_BUILD_CONFIG = /^app\/ui\/(?:vite\.config\.[^/]+|vite[^/]*\.(?:ts|js|mjs)|tsconfig[^/]*\.json)$/;

function classifyPath(path) {
  if (typeof path !== "string" || !path || SHARED_DIRS.some((dir) => path.startsWith(dir))) {
    return "shared";
  }
  // The Tauri command-parity test reads the mock source. Tauri's build embeds frontendDist.
  if (path === "app/ui/e2e/mockTauri.ts" || path.startsWith("app/ui/dist/")) return "shared";
  // The Rust jobs also install native npm dependencies and build frontendDist on both desktop OSes.
  if (UI_BUILD_FILES.has(path) || UI_BUILD_CONFIG.test(path)) return "shared";
  if (path.startsWith("app/ui/")) return "ui";
  if (RUST_CRATE_DIRS.some((dir) => path.startsWith(dir))) return "rust";
  if (path.startsWith("docs/") || path.endsWith(".md") || path === "LICENSE") return "inert";
  return "shared";
}

function classifyFiles(files) {
  let rust = false;
  let e2e = false;
  const filenames = new Set();
  if (!Array.isArray(files) || !files.length || files.length >= 3000) return ALL_JOBS;

  for (const file of files) {
    if (!FILE_STATUSES.has(file?.status) || filenames.has(file.filename)) return ALL_JOBS;
    filenames.add(file.filename);
    const paths = file.status === "renamed" ? [file.filename, file.previous_filename] : [file.filename];
    for (const path of paths) {
      const kind = classifyPath(path);
      if (kind === "shared") return ALL_JOBS;
      rust ||= kind === "rust";
      e2e ||= kind === "ui";
    }
  }
  return { rust: String(rust), wasm: String(rust), e2e: String(e2e) };
}

async function selectJobs({ eventName, pullRequest, repo, rest, paginate }) {
  if (eventName !== "pull_request") return ALL_JOBS;
  try {
    const args = { ...repo, pull_number: pullRequest.number };
    const files = await paginate(rest.pulls.listFiles, { ...args, per_page: 100 });
    // GitHub caps this endpoint at 3,000 files. Compare the event and current PR to
    // avoid skipping work with a partial list or one for a different revision.
    if (!files.length || files.length >= 3000 || files.length !== pullRequest.changed_files) return ALL_JOBS;
    const { data: current } = await rest.pulls.get(args);
    if (!pullRequest.head.sha || !pullRequest.base.sha ||
        current.head.sha !== pullRequest.head.sha || current.base.sha !== pullRequest.base.sha ||
        current.changed_files !== files.length) return ALL_JOBS;
    return classifyFiles(files);
  } catch {
    return ALL_JOBS;
  }
}

module.exports = { classifyFiles, selectJobs };
