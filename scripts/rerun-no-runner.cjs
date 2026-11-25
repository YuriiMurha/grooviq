async function isCurrentHead(event, repo, rest, paginate) {
  if (event.event === 'push') {
    const { data: ref } = await rest.git.getRef({ ...repo, ref: `heads/${event.head_branch}` });
    return ref.object.sha === event.head_sha;
  }

  const source = event.head_repository;
  if (!source?.id || !source.owner?.login) return false;

  // GitHub can send an empty pull_requests array, including for fork runs.
  const associated = event.pull_requests ?? [];
  const pulls = await paginate(rest.pulls.list, {
    ...repo, state: 'open', head: `${source.owner.login}:${event.head_branch}`, per_page: 100,
  });
  return pulls.some(pull => pull.state === 'open' && pull.head.repo?.id === source.id &&
    pull.head.ref === event.head_branch && pull.head.sha === event.head_sha &&
    (associated.length === 0 || associated.some(pr => pr.number === pull.number)));
}

async function rerunNoRunner({ event, repo, rest, paginate, log }) {
  if (event.run_attempt !== 1 || !event.head_sha || !event.head_branch ||
      !(event.event === 'pull_request' ||
        (event.event === 'push' && ['main', 'merge-train/batch'].includes(event.head_branch)))) {
    log(`Run ${event.id}: only first attempts on PRs, main, or the merge train are eligible`);
    return;
  }

  const run = { ...repo, run_id: event.id };
  let subject = `Run ${event.id}`;
  try {
    const jobs = await paginate(rest.actions.listJobsForWorkflowRunAttempt, {
      ...run, attempt_number: 1, per_page: 100,
    });
    const candidates = [];
    for (const job of jobs) {
      subject = `Job ${job.id}`;
      if (job.name !== 'rust (macos-latest)') {
        log(`${subject}: outside the required macOS Rust check`);
        continue;
      }
      if (job.conclusion !== 'cancelled' || !Array.isArray(job.steps) || job.steps.length !== 0) {
        log(`${subject}: requires a cancellation with zero steps`);
        continue;
      }

      const checkRunId = Number(job.check_run_url?.match(/\/check-runs\/(\d+)$/)?.[1]);
      if (!Number.isSafeInteger(checkRunId) || checkRunId <= 0) {
        log(`${subject}: missing check run ID`);
        continue;
      }
      const annotations = await paginate(rest.checks.listAnnotations, {
        ...repo, check_run_id: checkRunId, per_page: 100,
      });
      if (annotations.some(annotation => annotation.message.includes('Canceling since a higher priority waiting request'))) {
        log(`${subject}: superseded by another CI run`);
        continue;
      }
      if (!annotations.some(annotation => annotation.message.includes('not acquired by Runner'))) {
        log(`${subject}: missing runner acquisition failure annotation`);
        continue;
      }
      candidates.push(job.id);
    }
    if (candidates.length === 0) return;

    // A manual rerun or a replayed event can leave the original attempt in the payload.
    subject = `Run ${event.id}`;
    const { data: latest } = await rest.actions.getWorkflowRun(run);
    const firstAttempt = latest.run_attempt === 1 && latest.status === 'completed';
    const current = firstAttempt && await isCurrentHead(event, repo, rest, paginate);
    for (const jobId of candidates) {
      subject = `Job ${jobId}`;
      if (!firstAttempt) {
        log(`${subject}: run is no longer on completed attempt 1`);
      } else if (!current) {
        log(`${subject}: triggering SHA is no longer the current head`);
      } else {
        await rest.actions.reRunJobForWorkflowRun({ ...repo, job_id: jobId });
        log(`${subject}: requested one rerun after runner acquisition failed`);
      }
    }
  } catch (error) {
    log(`${subject}: API error, stopping reruns (${String(error.message).replace(/[\r\n]+/g, ' ')})`);
  }
}

module.exports = { rerunNoRunner };
