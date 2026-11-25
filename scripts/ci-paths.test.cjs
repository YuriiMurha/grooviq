const { test } = require("node:test");
const assert = require("node:assert/strict");
const { selectJobs } = require("./ci-paths.cjs");

const all = { rust: "true", wasm: "true", e2e: "true" };
const uiOnly = { rust: "false", wasm: "false", e2e: "true" };
const rustOnly = { rust: "true", wasm: "true", e2e: "false" };
const inertOnly = { rust: "false", wasm: "false", e2e: "false" };
const file = (filename) => ({ filename, status: "modified" });

function request(files) {
  const pullRequest = { number: 1, changed_files: files.length, head: { sha: "head" }, base: { sha: "base" } };
  return {
    eventName: "pull_request",
    pullRequest,
    repo: { owner: "o", repo: "r" },
    rest: { pulls: { listFiles: Symbol("listFiles"), get: async () => ({ data: pullRequest }) } },
    paginate: async () => files,
  };
}

const check = async (expected, ...paths) =>
  assert.deepEqual(await selectJobs(request(paths.map(file))), expected, paths.join(", "));

test("UI, Rust, and inert paths skip only unrelated checks", async () => {
  await check(uiOnly, "app/ui/src/App.tsx", "app/ui/package-lock.json", "README.md");
  for (const path of ["dsp/src/lib.rs", "engine/tests/data/blip.mp3", "project/src/lib.rs",
    "tests/golden/src/lib.rs", "app/src-tauri/tauri.conf.json", "tools/qa-fixture/src/main.rs"]) {
    await check(rustOnly, path, "docs/images/banner.png");
  }
  await check(inertOnly, "README.md", "docs/design.md", "LICENSE");
  await check(all, "engine/src/lib.rs", "app/ui/src/App.tsx");
});

test("shared inputs, tooling and unknown paths retain all checks", async () => {
  for (const path of ["app/ui/e2e/mockTauri.ts", "app/ui/dist/index.html", "tests/fixtures/session-replay.json",
    "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "rustfmt.toml", "clippy.toml", "Makefile",
    ".config/nextest.toml", ".cargo/config.toml", ".githooks/pre-commit", ".github/pull_request_template.md",
    "scripts/future.md", "package.json", "unknown/data"]) {
    await check(all, path);
  }
});

test("both sides of a rename inform coverage", async () => {
  const renamed = (filename, previous_filename) => ({ filename, previous_filename, status: "renamed" });
  for (const entry of [
    renamed("app/ui/example.ts", "dsp/src/example.rs"),
    renamed("dsp/src/example.rs", "app/ui/example.ts"),
    renamed("app/ui/e2e/new.ts", "app/ui/e2e/mockTauri.ts"),
    renamed("docs/example.md", undefined),
  ]) {
    assert.deepEqual(await selectJobs(request([entry])), all);
  }
  assert.deepEqual(await selectJobs(request([renamed("docs/new.md", "docs/old.md")])), inertOnly);
});

test("empty, capped, duplicated and partial file lists fail open", async () => {
  for (const count of [0, 2999, 3000, 3001]) {
    const files = Array.from({ length: count }, (_, i) => file(`app/ui/${i}.ts`));
    assert.deepEqual(await selectJobs(request(files)), count === 2999 ? uiOnly : all, String(count));
  }
  assert.deepEqual(await selectJobs(request([file("docs/a.md"), file("docs/a.md")])), all);
  const partial = request([file("docs/a.md")]);
  partial.pullRequest.changed_files = 2;
  assert.deepEqual(await selectJobs(partial), all);
});

test("the current PR must match both event SHAs and the file count", async () => {
  for (const changed of [{ head: { sha: "new" } }, { base: { sha: "new" } }, { changed_files: 2 }]) {
    const args = request([file("docs/a.md")]);
    args.rest.pulls.get = async () => ({ data: { ...args.pullRequest, ...changed } });
    assert.deepEqual(await selectJobs(args), all);
  }
  const missing = request([file("docs/a.md")]);
  delete missing.pullRequest.head.sha;
  assert.deepEqual(await selectJobs(missing), all);
});

test("non-PR events, API exceptions and malformed responses run everything", async () => {
  for (const eventName of ["push", "workflow_dispatch", "merge_group"]) {
    assert.deepEqual(await selectJobs({ eventName }), all);
  }
  for (const files of [null, [null], [{ status: "modified" }], [{ filename: "docs/a.md" }]]) {
    const args = request([file("docs/a.md")]);
    args.paginate = async () => files;
    assert.deepEqual(await selectJobs(args), all);
  }
  for (const api of ["list", "get"]) {
    const args = request([file("docs/a.md")]);
    const denied = async () => { throw new Error("API denied"); };
    if (api === "list") args.paginate = denied;
    else args.rest.pulls.get = denied;
    assert.deepEqual(await selectJobs(args), all);
  }
});
