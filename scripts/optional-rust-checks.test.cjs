const { test } = require('node:test');
const assert = require('node:assert/strict');
const { shouldRun } = require('./optional-rust-checks.cjs');

function request(files, overrides = {}) {
  const pullRequest = { number: 1, changed_files: files.length, head: { sha: 'head' }, base: { sha: 'base' } };
  return { eventName: 'pull_request', pullRequest, repo: { owner: 'o', repo: 'r' },
    rest: { pulls: { listFiles: Symbol('listFiles'), get: async () => ({ data: pullRequest }) } },
    paginate: async () => files, ...overrides };
}
const file = filename => ({ filename, status: 'modified' });

test('UI and documentation changes omit optional work', async () => {
  assert.equal(await shouldRun(request(['app/ui/src/App.tsx', 'app/ui/package-lock.json', 'docs/spec.md', 'README.md'].map(file))), false);
});
test('all actual Rust inputs and unknown paths retain work, even after many UI files', async () => {
  for (const path of ['engine/src/lib.rs', 'dsp/benches/kernels.rs', 'project/src/lib.rs', 'tests/golden/src/lib.rs', 'tests/fixtures/live-v2.bin', 'engine/tests/data/blip.mp3', 'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'Makefile', 'clippy.toml', '.cargo/config.toml', '.github/actions/new/action.yml', '.github/workflows/perf.yml', 'scripts/new.sh', 'future/input']) {
    assert.equal(await shouldRun(request([...Array.from({ length: 400 }, (_, i) => file(`app/ui/${i}.ts`)), file(path)])), true, path);
  }
});
test('renames check both paths including missing previous names', async () => {
  for (const previous_filename of ['engine/src/lib.rs', undefined]) {
    assert.equal(await shouldRun(request([{ filename: 'docs/moved.md', status: 'renamed', previous_filename }])), true);
  }
  assert.equal(await shouldRun(request([{ filename: 'docs/new.md', status: 'renamed', previous_filename: 'docs/old.md' }])), false);
});
test('empty, truncated, API-limited and changed revisions retain coverage', async () => {
  assert.equal(await shouldRun(request([])), true);
  const truncated = request([file('app/ui/a.ts')]);
  truncated.pullRequest.changed_files = 2;
  assert.equal(await shouldRun(truncated), true);
  for (const size of [2999, 3000, 3001]) {
    assert.equal(await shouldRun(request(Array.from({ length: size }, (_, i) => file(`app/ui/${i}.ts`)))), size >= 3000);
  }
  for (const key of ['head', 'base']) {
    const changed = request([file('docs/a.md')]);
    changed.rest.pulls.get = async () => ({ data: { ...changed.pullRequest, [key]: { sha: 'new' } } });
    assert.equal(await shouldRun(changed), true);
  }
});
test('API errors and malformed responses fail open; pushes/manual runs always run', async () => {
  for (const paginate of [async () => { throw Error('denied'); }, async () => null, async () => [null]]) {
    assert.equal(await shouldRun(request([file('docs/a.md')], { paginate })), true);
  }
  const broken = request([file('docs/a.md')]);
  broken.rest.pulls.get = async () => { throw Error('rate limit'); };
  assert.equal(await shouldRun(broken), true);
  for (const eventName of ['push', 'workflow_dispatch', 'unknown']) {
    assert.equal(await shouldRun({ eventName }), true);
  }
});
