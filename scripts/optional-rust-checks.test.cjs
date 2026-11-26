const { test } = require("node:test");
const { dirname, join, relative, resolve, sep } = require("node:path");
const assert = require("node:assert/strict");
const { readCargoMetadata, loadPolicy, shouldRun } = require("./optional-rust-checks.cjs");

const root = resolve("fake-workspace");
const dep = (name, dir, kind = null) => ({ name, path: join(root, dir), kind });
const pkg = (name, dir, dependencies = []) => ({
  name, manifest_path: join(root, dir, "Cargo.toml"), dependencies,
});
const packages = [
  pkg("grooviq-engine", "engine", [dep("dsp-alias", "dsp", "dev")]),
  pkg("grooviq-dsp", "dsp", [dep("new-library", "tools/new-library", "build")]),
  pkg("grooviq-golden", "tests/golden", [dep("grooviq-project", "project")]),
  pkg("grooviq-project", "project", [dep("grooviq-engine", "engine")]),
  pkg("new-library", "tools/new-library"),
  pkg("grooviq-app", "app/src-tauri", [dep("grooviq-engine", "engine")]),
];
const fixture = { packages, workspace_root: root };
const policy = loadPolicy(fixture);
const load = () => policy;

function request(workflow, files, overrides = {}) {
  const pullRequest = {
    number: 1, changed_files: files.length, head: { sha: "head" }, base: { sha: "base" },
  };
  return {
    workflow, eventName: "pull_request", pullRequest, repo: { owner: "o", repo: "r" },
    rest: { pulls: { listFiles: Symbol("listFiles"), get: async () => ({ data: pullRequest }) } },
    paginate: async () => files, load, ...overrides,
  };
}

const file = filename => ({ filename, status: "modified" });
const docs = [file("docs/a.md")];

test("the current Cargo manifests define the optional input crates", () => {
  const metadata = readCargoMetadata();
  const { inputs } = loadPolicy();
  const manifestDirs = new Map(metadata.packages.map(pkg => [pkg.name, dirname(pkg.manifest_path)]));
  for (const [workflow, roots] of [
    ["rtsan", ["grooviq-engine"]],
    ["perf", ["grooviq-golden", "grooviq-dsp"]],
  ]) {
    // Expand all reachable manifests until another pass adds no directories.
    const reachable = new Set(roots.map(name => manifestDirs.get(name)));
    for (let size = 0; size < reachable.size;) {
      size = reachable.size;
      for (const pkg of metadata.packages.filter(pkg => reachable.has(manifestDirs.get(pkg.name)))) {
        for (const dep of pkg.dependencies) {
          if (dep.path) reachable.add(dep.path);
        }
      }
    }
    const dirs = new Set([...reachable].map(dir => relative(metadata.workspace_root, dir).split(sep).join("/")));
    assert.deepEqual(inputs[workflow], dirs, workflow);
  }
});

test("dependencies follow local paths transitively across aliases and target kinds", async () => {
  for (const workflow of ["rtsan", "perf"]) {
    assert.equal(await shouldRun(request(workflow, [file("tools/new-library/src/lib.rs")])), true);
    assert.equal(await shouldRun(request(workflow, [file("app/src-tauri/src/lib.rs")])), false);
    const incomplete = () => loadPolicy({
      ...fixture, packages: packages.filter(pkg => pkg.name !== "new-library"),
    });
    assert.equal(await shouldRun(request(workflow, docs, { load: incomplete })), true);
  }
});

test("package directories must be inside the checkout and below its root", () => {
  const root = resolve("fake-workspace");
  const packages = [
    { name: "grooviq-engine", manifest_path: join(root, "engine", "Cargo.toml"), dependencies: [] },
    { name: "grooviq-golden", manifest_path: join(root, "golden", "Cargo.toml"), dependencies: [] },
    { name: "grooviq-dsp", manifest_path: join(root, "dsp", "Cargo.toml"), dependencies: [] },
  ];
  for (const location of [root, resolve("elsewhere")]) {
    packages[0].manifest_path = join(location, "Cargo.toml");
    assert.throws(() => loadPolicy({ packages, workspace_root: root }), /Cannot classify/);
  }
});

const pathCases = [
  ["engine/src/lib.rs", true, true],
  ["dsp/benches/kernels.rs", true, true],
  ["project/src/lib.rs", false, true],
  ["app/src-tauri/src/lib.rs", false, false],
  ["app/src-tauri2/lib.rs", true, true],
  ["tests/golden/samples/kick.wav", false, true],
  ["app/ui/src/App.tsx", false, false],
  ["docs/spec.md", false, false],
  ["README.md", false, false],
  ["tools/qa-fixture/src/main.rs", false, false],
  ["tools/other.sh", false, false],
  ["scripts/merge_train.py", false, false],
  [".github/workflows/rtsan.yml", true, false],
  [".github/workflows/perf.yml", false, true],
  [".github/workflows/release.yml", false, false],
  ["scripts/capture-instruction-profiles.py", false, true],
  ["scripts/perf-fetch.py", false, true],
  ["scripts/test_perf_fetch.py", false, true],
  ...[
    "Cargo.toml", "Cargo.lock", "rust-toolchain", "rust-toolchain.toml", ".cargo/config.toml",
    ".config/nextest.toml", "Makefile", "scripts/optional-rust-checks.cjs",
    "scripts/optional-rust-checks.test.cjs", "clippy.toml", "tests/fixtures/new.bin",
    "future/input", "app/ui-kit/lib.rs", "docs2/build.rs", ".github/actions/new/action.yml",
  ].map(path => [path, true, true]),
];

for (const [column, workflow] of ["rtsan", "perf"].entries()) {
  for (const [path, ...expected] of pathCases) {
    test(`${workflow}: classify ${path}`, async () => {
      assert.equal(await shouldRun(request(workflow, [file(path)])), expected[column]);
    });
  }

  test(`${workflow}: paginate all paths and read the current revision`, async () => {
    const files = Array.from({ length: 400 }, (_, i) => file(`app/ui/${i}.ts`));
    files.push(file("dsp/src/lib.rs"));
    const args = request(workflow, files);
    args.paginate = async (method, params) => {
      assert.equal(method, args.rest.pulls.listFiles);
      assert.deepEqual(params, { owner: "o", repo: "r", pull_number: 1, per_page: 100 });
      return files;
    };
    // Check calls directly because shouldRun catches errors.
    let read = false;
    args.rest.pulls.get = async params => {
      read = true;
      assert.deepEqual(params, { owner: "o", repo: "r", pull_number: 1 });
      return { data: args.pullRequest };
    };
    assert.equal(await shouldRun(args), true);
    assert.equal(read, true);
    files[400] = file("docs/last.md");
    assert.equal(await shouldRun(args), false);
  });

  test(`${workflow}: check both sides of a rename`, async () => {
    const files = [{ filename: "docs/new.md", status: "renamed", previous_filename: "engine/src/old.rs" }];
    assert.equal(await shouldRun(request(workflow, files)), true);
    files[0] = { filename: "engine/src/new.rs", status: "renamed", previous_filename: "docs/old.md" };
    assert.equal(await shouldRun(request(workflow, files)), true);
    files[0] = { filename: "docs/new.md", status: "renamed", previous_filename: "docs/old.md" };
    assert.equal(await shouldRun(request(workflow, files)), false);
    delete files[0].previous_filename;
    assert.equal(await shouldRun(request(workflow, files)), true);
  });

  test(`${workflow}: empty, truncated and API-limited diffs fail open`, async () => {
    assert.equal(await shouldRun(request(workflow, [])), true);
    for (const [eventCount, currentCount] of [[2, 1], [1, 2]]) {
      const args = request(workflow, docs);
      args.pullRequest.changed_files = eventCount;
      args.rest.pulls.get = async () => ({ data: { ...args.pullRequest, changed_files: currentCount } });
      assert.equal(await shouldRun(args), true, `counts ${eventCount}/${currentCount}/1`);
    }
    for (const size of [2999, 3000, 3001]) {
      const files = Array.from({ length: size }, (_, i) => file(`app/ui/${i}.ts`));
      assert.equal(await shouldRun(request(workflow, files)), size >= 3000);
    }
  });

  for (const key of ["head", "base"]) {
    test(`${workflow}: stale or absent ${key} SHA fails open`, async () => {
      for (const [eventSha, currentSha] of [["old", "new"], [undefined, undefined]]) {
        const args = request(workflow, docs);
        args.pullRequest[key] = { sha: eventSha };
        args.rest.pulls.get = async () => ({ data: { ...args.pullRequest, [key]: { sha: currentSha } } });
        assert.equal(await shouldRun(args), true);
      }
    });
  }

  test(`${workflow}: API errors and malformed file lists fail open`, async () => {
    const arrayLike = { 0: docs[0], length: 1, [Symbol.iterator]: docs[Symbol.iterator] };
    for (const result of [null, arrayLike, [null], [{}],
      [{ ...docs[0], status: "unknown" }], [file(undefined)], [docs[0], docs[0]]]) {
      const args = request(workflow, Array.isArray(result) ? result : docs, { paginate: async () => result });
      assert.equal(await shouldRun(args), true);
    }
    const args = request(workflow, docs);
    args.paginate = async () => { throw Error("pagination denied"); };
    assert.equal(await shouldRun(args), true);
    args.paginate = async () => docs;
    args.rest.pulls.get = async () => { throw Error("rate limit"); };
    assert.equal(await shouldRun(args), true);
  });

  test(`${workflow}: metadata errors fail open`, async () => {
    const load = () => { throw Error("metadata unavailable"); };
    assert.equal(await shouldRun(request(workflow, docs, { load })), true);
  });

  test(`${workflow}: pushes and manual runs always run without the PR API or metadata`, async () => {
    for (const eventName of ["push", "workflow_dispatch", "unknown"]) {
      let called = false;
      const paginate = async () => { called = true; return docs; };
      assert.equal(await shouldRun(request(workflow, docs, { eventName, paginate })), true);
      assert.equal(called, false);
    }
  });
}

test("unknown workflow fails open without touching the PR API", async () => {
  let called = false;
  const paginate = async () => { called = true; return docs; };
  assert.equal(await shouldRun(request("other", docs, { paginate })), true);
  assert.equal(called, false);
});

const batchRef = "refs/heads/merge-train/batch";
const batchSha = "a".repeat(40);
const batchRepo = { owner: "o", repo: "r" };

function batch(files, overrides = {}) {
  const compare = Symbol("compareCommitsWithBasehead");
  return {
    workflow: "rtsan", eventName: "push", ref: batchRef, after: batchSha, repo: batchRepo, load,
    rest: {
      repos: { compareCommitsWithBasehead: compare },
      git: {
        getRef: async params => {
          assert.deepEqual(params, { ...batchRepo, ref: "heads/merge-train/batch" });
          return { data: { object: { sha: batchSha } } };
        },
      },
    },
    paginate: async (method, params, map) => {
      assert.equal(method, compare);
      assert.deepEqual(params, { ...batchRepo, basehead: `main...${batchSha}`, per_page: 1 });
      let done = false;
      const result = map({ data: { files, total_commits: 3 } }, () => { done = true; });
      assert.equal(done, true, "only the first compare page lists changed files");
      return [].concat(result);
    },
    ...overrides,
  };
}

test("RTSan compares the pushed batch SHA with main and classifies the changed files", async () => {
  assert.equal(await shouldRun(batch([file("app/ui/src/App.tsx")])), false);
  assert.equal(await shouldRun(batch([file("engine/src/lib.rs")])), true);
  assert.equal(await shouldRun(batch([
    { filename: "docs/new.md", status: "renamed", previous_filename: "engine/src/old.rs" },
  ])), true);
  assert.equal(await shouldRun(batch([file("scripts/optional-rust-checks.cjs")])), true);
});

test("other pushes and manual events bypass the batch compare", async () => {
  for (const overrides of [
    { ref: "refs/heads/main" }, { workflow: "perf" }, { eventName: "workflow_dispatch" },
  ]) {
    let compared = false;
    const paginate = async () => { compared = true; return docs; };
    assert.equal(await shouldRun(batch(docs, { ...overrides, paginate })), true);
    assert.equal(compared, false);
  }
});

test("missing, malformed and deleted batch SHAs run RTSan without comparing", async () => {
  for (const after of [undefined, "invalid", "0".repeat(40)]) {
    let compared = false;
    const paginate = async () => { compared = true; return docs; };
    assert.equal(await shouldRun(batch(docs, { after, paginate })), true);
    assert.equal(compared, false);
  }
});

test("batch compare and ref API errors run RTSan", async () => {
  const denied = async () => { throw Error("GitHub API denied access"); };
  assert.equal(await shouldRun(batch(docs, { paginate: denied })), true);
  const args = batch(docs);
  args.rest.git.getRef = denied;
  assert.equal(await shouldRun(args), true);
});

test("truncated batch comparisons run RTSan", async () => {
  for (const size of [299, 300, 301]) {
    const files = Array.from({ length: size }, (_, i) => file(`app/ui/${i}.ts`));
    assert.equal(await shouldRun(batch(files)), size >= 300);
  }
});

test("missing and empty batch comparisons run RTSan", async () => {
  const arrayLike = { 0: docs[0], length: 1, [Symbol.isConcatSpreadable]: true };
  for (const files of [undefined, arrayLike, []]) {
    assert.equal(await shouldRun(batch(files)), true);
  }
});

test("a batch ref that moved or disappeared since the push runs RTSan", async () => {
  for (const sha of ["b".repeat(40), undefined]) {
    const args = batch(docs);
    args.rest.git.getRef = async () => ({ data: { object: { sha } } });
    assert.equal(await shouldRun(args), true);
  }
});

test("RTSan rechecks the batch ref after the comparison", async () => {
  const args = batch(docs);
  let sha = batchSha;
  const compare = args.paginate;
  args.paginate = async (...params) => {
    const files = await compare(...params);
    sha = "b".repeat(40);
    return files;
  };
  args.rest.git.getRef = async () => ({ data: { object: { sha } } });
  assert.equal(await shouldRun(args), true);
});

test("roll selects the UI timing inputs and unknown paths", async () => {
  for (const path of [
    "app/ui/src/App.tsx", "app/ui/src/roll/budgets.test.ts", "app/ui/package.json",
    "app/ui/package-lock.json", "app/ui/vite.config.ts", "app/ui/tsconfig.node.json",
    ".github/workflows/perf.yml", "Makefile", "scripts/optional-rust-checks.cjs",
    "scripts/optional-rust-checks.test.cjs", "scripts/roll-timing.cjs", "scripts/roll-timing.test.cjs",
    "app/ui/new-config", "scripts/new-script.cjs", "future/input", "tests/fixtures/new.json",
  ]) {
    assert.equal(await shouldRun(request("roll", [...docs, file(path)])), true, path);
  }
});

test("roll skips Rust, docs and e2e inputs", async () => {
  for (const path of [
    "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml", "clippy.toml",
    "engine/src/lib.rs", "dsp/src/lib.rs", "app/src-tauri/src/lib.rs", "tools/new-library/src/lib.rs",
    ".github/workflows/rtsan.yml", "README.md", "docs/guide.md", "app/ui/e2e/roll.spec.ts",
  ]) {
    assert.equal(await shouldRun(request("roll", [file(path)])), false, path);
  }
});

test("roll inspects both sides of renames and fails open on selector doubt", async () => {
  const renamed = (filename, previous_filename) => ({ filename, previous_filename, status: "renamed" });
  for (const entry of [
    renamed("app/ui/e2e/old.ts", "app/ui/src/old.ts"),
    renamed("app/ui/src/new.ts", "docs/old.md"),
    renamed("docs/new.md", undefined),
    file(""), file("docs/../Makefile"),
  ]) assert.equal(await shouldRun(request("roll", [entry])), true);
  assert.equal(await shouldRun(request("roll", [renamed("docs/new.md", "docs/old.md")])), false);
  assert.equal(await shouldRun(request("roll", docs, { load: () => { throw Error("metadata"); } })), true);
  assert.equal(await shouldRun(request("roll", docs, { paginate: async () => [] })), true);
  assert.equal(await shouldRun(request("roll", docs, { eventName: "push", ref: "refs/heads/main" })), true);
});
