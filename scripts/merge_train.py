"""Advance one enrolled Grooviq PR through the existing branch protections."""

import argparse
import fcntl
import json
from pathlib import Path
import re
import subprocess
import sys

REPO = "YuriiMurha/grooviq"
LABEL = "merge-train"
ACTIONS_APP = 15368
REQUIRED = ("rust (macos-latest)", "rust (windows-latest)", "ui")
PENDING = {None, "queued", "in_progress", "pending", "requested", "waiting"}
SHA = re.compile(r"[a-f0-9]{40}")


def tick(repo, order, approved_head):
    enrolled = set(repo.enrolled())
    queue = dict.fromkeys([*order, *sorted(enrolled)])
    for number in queue:
        if number not in enrolled:
            continue
        pr = repo.pull(number)
        if pr["state"] != "open":
            continue
        head = pr["head"]
        sha = head["sha"]
        source = head["repo"]
        if pr["draft"] or pr["base"]["ref"] != "main" or not source or source["full_name"] != REPO:
            return f"Held #{number}: expected a ready, same-repository PR into main."
        if pr["auto_merge"] is not None:
            return f"Held #{number}: GitHub auto-merge is enabled."
        if approved_head != sha:
            return f"Held #{number} at {sha}: supply the exact head after verifying approval."
        checks = repo.checks(sha)
        failures = [f"{name} ({checks[name]})" for name in REQUIRED
                    if checks.get(name) not in PENDING | {"success"}]
        if failures:
            reason = ", ".join(failures)
            if not repo.reject(number, sha, reason):
                return f"Held #{number}: the failed head {sha} is no longer current."
            return f"Removed #{number} from the train: {reason}. Notify Root and the lane."
        if any(checks.get(name) != "success" for name in REQUIRED):
            return f"Waiting for required checks on #{number} at {sha}."
        if repo.behind_main(sha):
            repo.refresh(number, sha)
            return f"Updated #{number} from {sha}. Verify the new head before the next tick."
        repo.merge(number, sha)
        return f"Merged #{number} at {sha}. Notify Root and the lane."
    return "No open PRs enrolled in the merge train."


class GitHub:
    def api(self, path, *, method="GET", body=None, paginate=False):
        argv = ["gh", "api", "--method", method, f"repos/{REPO}/{path}"]
        if paginate:
            argv += ["--paginate", "--slurp"]
        if body is not None:
            argv += ["--input", "-"]
        # Retry on a later tick after checking remote state, especially after a write error.
        result = subprocess.run(
            argv, input="" if body is None else json.dumps(body),
            text=True, stdout=subprocess.PIPE, check=True, timeout=45,
        )
        return json.loads(result.stdout) if result.stdout else None

    def enrolled(self):
        pages = self.api(f"issues?state=open&labels={LABEL}&per_page=100", paginate=True)
        return [item["number"] for page in pages for item in page if "pull_request" in item]

    def pull(self, number):
        return self.api(f"pulls/{number}")

    def checks(self, sha):
        policy = self.api("branches/main/protection/required_status_checks")
        contexts = {(check["context"], check["app_id"]) for check in policy["checks"]}
        if policy["strict"] is not True or contexts != {(name, ACTIONS_APP) for name in REQUIRED}:
            raise RuntimeError("Main's required-check policy changed; reconcile it before running the train.")
        pages = self.api(f"commits/{sha}/check-runs?filter=latest&per_page=100", paginate=True)
        checks = {}
        for page in pages:
            for check in page["check_runs"]:
                name = check["name"]
                if (name in REQUIRED and check["head_sha"] == sha
                        and check["app"]["id"] == ACTIONS_APP):
                    if name in checks:
                        raise RuntimeError(f"Ambiguous required check {name} on {sha}.")
                    checks[name] = check["conclusion"] if check["status"] == "completed" else check["status"]
                    if checks[name] is None:
                        raise RuntimeError(f"Missing conclusion for completed check {name} on {sha}.")
        return checks

    def behind_main(self, sha):
        return self.api(f"compare/main...{sha}?per_page=1")["behind_by"]

    def reject(self, number, sha, reason):
        def still_current():
            pr = self.pull(number)
            return pr["state"] == "open" and pr["head"]["sha"] == sha

        if not still_current():
            return False
        self.api(f"issues/{number}/comments", method="POST",
                 body={"body": f"A required check failed on `{sha}`: {reason}. "
                               "The train will remove that head from enrollment."})
        if not still_current():
            return False
        # Label removal has no expected-head parameter, unlike update and merge.
        self.api(f"issues/{number}/labels/{LABEL}", method="DELETE")
        return True

    def refresh(self, number, sha):
        self.api(f"pulls/{number}/update-branch", method="PUT", body={"expected_head_sha": sha})

    def merge(self, number, sha):
        result = self.api(f"pulls/{number}/merge", method="PUT",
                          body={"sha": sha, "merge_method": "squash"})
        if result.get("merged") is not True:
            raise RuntimeError(f"GitHub rejected the merge of #{number}: {result.get('message')}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--order", type=int, nargs="*", default=[],
                        help="priority PR numbers; other enrolled PRs follow in number order")
    parser.add_argument("--head", metavar="SHA", help="exact approved head of the first PR")
    args = parser.parse_args()
    if args.head and SHA.fullmatch(args.head) is None:
        parser.error("--head requires a full lowercase commit SHA")

    try:
        common_dir = subprocess.check_output(
            ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"], text=True,
        ).strip()
        with (Path(common_dir) / "merge-train.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            print(tick(GitHub(), args.order, args.head))
    except (OSError, subprocess.SubprocessError, RuntimeError) as error:
        print(f"Merge train stopped: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
