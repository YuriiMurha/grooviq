// These trees are not inputs to bench/RTSan today. If Rust starts consuming UI or
// documentation assets, remove that exclusion before adding the dependency.
function irrelevant(path) {
  return typeof path === "string" &&
    (path.startsWith("app/ui/") || path.startsWith("docs/") ||
     ["README.md", "AGENTS.md"].includes(path));
}

async function shouldRun({ eventName, pullRequest, repo, rest, paginate }) {
  if (eventName !== "pull_request") return true;
  try {
    const args = { ...repo, pull_number: pullRequest.number };
    const files = await paginate(rest.pulls.listFiles, { ...args, per_page: 100 });
    // The API caps this endpoint at 3,000 files. Never classify a partial list,
    // an empty diff, or a list from a newer PR revision as irrelevant.
    if (!files.length || files.length >= 3000 || files.length !== pullRequest.changed_files) return true;
    const current = (await rest.pulls.get(args)).data;
    if (current.head.sha !== pullRequest.head.sha || current.base.sha !== pullRequest.base.sha ||
        current.changed_files !== files.length) return true;
    return files.some(file => !irrelevant(file.filename) ||
      (file.status === "renamed" && !irrelevant(file.previous_filename)));
  } catch {
    return true;
  }
}
module.exports = { shouldRun };
