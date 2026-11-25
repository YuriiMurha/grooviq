const { test } = require('node:test');
const assert = require('node:assert/strict');
const { rerunNoRunner } = require('./rerun-no-runner.cjs');

// Relevant fields from run 45638721042 attempt 1 and job 139291115499.
const run = {
  id: 45638721042,
  event: 'pull_request',
  run_attempt: 1,
  head_sha: '9981e0949e7d70b408cd3e9c7909d7a7c9ba15d6',
  head_branch: 'perf/cache-reverb-decay',
  head_repository: { id: 1459356589, owner: { login: 'YuriiMurha' } },
  pull_requests: [],
};
const macJob = {
  id: 139291115499,
  name: 'rust (macos-latest)',
  conclusion: 'cancelled',
  steps: [],
  check_run_url: 'https://api.github.com/repos/YuriiMurha/grooviq/check-runs/139291115499',
};
const runnerFailure = {
  annotation_level: 'failure',
  message: 'The job was not acquired by Runner of type hosted even after multiple attempts',
};
const capacityNotice = {
  annotation_level: 'notice',
  message: 'Due to capacity constraints, jobs targeting macOS arm64 runners may experience longer queue times.',
};
const repo = { owner: 'YuriiMurha', repo: 'grooviq' };

function scenario({
  event = run,
  jobs = [macJob],
  annotations = [runnerFailure, capacityNotice],
  currentSha = run.head_sha,
  currentRepoId = run.head_repository.id,
  latestAttempt = 1,
  apiError,
} = {}) {
  const calls = [];
  const logs = [];
  const api = (name, data) => async (args) => {
    calls.push([name, args]);
    if (apiError === name) throw new Error('rate limited');
    return { data };
  };
  const pull = {
    number: 489,
    state: 'open',
    head: { sha: currentSha, ref: run.head_branch, repo: { id: currentRepoId } },
  };
  const rest = {
    actions: {
      listJobsForWorkflowRunAttempt: api('jobs', { jobs }),
      getWorkflowRun: api('run', { run_attempt: latestAttempt, status: 'completed' }),
      reRunJobForWorkflowRun: api('rerun'),
    },
    checks: { listAnnotations: api('annotations', annotations) },
    pulls: { list: api('pulls', [pull]) },
    git: { getRef: api('ref', { object: { sha: currentSha } }) },
  };
  return {
    execute: () => rerunNoRunner({
      event, repo, rest, log: message => logs.push(message),
      paginate: async (method, args) => {
        const { data } = await method(args);
        return data.jobs ?? data;
      },
    }),
    calls,
    logs,
    reruns: () => calls.filter(([name]) => name === 'rerun').map(([, args]) => args),
  };
}

test('reruns the recorded no-runner cancellation on its current open PR', async () => {
  const { execute, calls, reruns, logs } = scenario();
  await execute();
  assert.deepEqual(calls.find(([name]) => name === 'jobs')?.[1], {
    ...repo, run_id: 45638721042, attempt_number: 1, per_page: 100,
  });
  assert.equal(calls.find(([name]) => name === 'annotations')?.[1].check_run_id, 139291115499);
  assert.equal(calls.find(([name]) => name === 'pulls')?.[1].head, 'YuriiMurha:perf/cache-reverb-decay');
  assert.deepEqual(reruns(), [{ ...repo, job_id: 139291115499 }]);
  assert.equal(logs.length, 1);
});

test('ignores supersession even with a macOS capacity notice', async () => {
  const superseded = {
    annotation_level: 'failure',
    message: 'Canceling since a higher priority waiting request for ci-ci-489 exists',
  };
  for (const annotations of [[superseded, capacityNotice], [superseded, runnerFailure]]) {
    const { execute, reruns } = scenario({ annotations });
    await execute();
    assert.deepEqual(reruns(), []);
  }
});

test('requires the runner acquisition annotation for unknown cancellations', async () => {
  for (const annotations of [[], [capacityNotice]]) {
    const { execute, reruns } = scenario({ annotations });
    await execute();
    assert.deepEqual(reruns(), []);
  }
});

test('ignores jobs that ran steps and jobs outside the required macOS check', async () => {
  for (const job of [
    { ...macJob, steps: [{ name: 'Checkout', conclusion: 'cancelled' }] },
    { ...macJob, name: 'rust (windows-latest)' },
    { ...macJob, steps: undefined },
  ]) {
    const { execute, calls, reruns } = scenario({ jobs: [job] });
    await execute();
    assert.deepEqual(reruns(), []);
    assert.deepEqual(calls.map(([name]) => name), ['jobs']);
  }
});

test('ignores attempt 2 and a delayed event after a rerun was already requested', async () => {
  const second = scenario({ event: { ...run, run_attempt: 2 } });
  await second.execute();
  assert.deepEqual(second.calls, []);
  const delayed = scenario({ latestAttempt: 2 });
  await delayed.execute();
  assert.deepEqual(delayed.reruns(), []);
});

test('requires the current SHA of the associated PR and the correct head repository', async () => {
  const event = { ...run, pull_requests: [{ number: 489 }] };
  for (const settings of [
    { event, currentSha: 'new-head' },
    { event, currentRepoId: 42 },
    { event: { ...run, pull_requests: [{ number: 500 }] } },
  ]) {
    const { execute, reruns } = scenario(settings);
    await execute();
    assert.deepEqual(reruns(), []);
  }
});

test('requires the exact current branch tip for main and the merge train', async () => {
  for (const branch of ['main', 'merge-train/batch']) {
    const event = { ...run, event: 'push', head_branch: branch };
    const current = scenario({ event });
    await current.execute();
    assert.deepEqual(current.calls.find(([name]) => name === 'ref')?.[1], {
      ...repo, ref: 'heads/' + branch,
    });
    assert.deepEqual(current.reruns(), [{ ...repo, job_id: 139291115499 }]);

    const stale = scenario({ event, currentSha: 'new-tip' });
    await stale.execute();
    assert.deepEqual(stale.reruns(), []);
  }
});

test('reruns only the macOS cancellation alongside a real failure', async () => {
  const failed = { ...macJob, id: 139291115552, name: 'rust (windows-latest)', conclusion: 'failure' };
  const { execute, reruns, logs } = scenario({ jobs: [
    failed, { ...macJob, id: 139291115500, conclusion: 'failure' }, macJob,
  ] });
  await execute();
  assert.deepEqual(reruns(), [{ ...repo, job_id: 139291115499 }]);
  assert.equal(logs.length, 3);
});

test('logs API errors and stops without retrying the request', async () => {
  for (const apiError of ['jobs', 'annotations', 'pulls', 'run', 'ref', 'rerun']) {
    const event = apiError === 'ref' ? { ...run, event: 'push', head_branch: 'main' } : run;
    const { execute, calls, reruns, logs } = scenario({ event, apiError });
    await execute();
    assert.equal(calls.filter(([name]) => name === apiError).length, 1, apiError);
    assert.equal(reruns().length, apiError === 'rerun' ? 1 : 0, apiError);
    assert.ok(logs.some(message => message.includes('rate limited')), apiError);
  }
});
