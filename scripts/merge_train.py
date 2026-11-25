"""Test a batch of approved Grooviq PRs before squash-merging it into main."""

import argparse
from dataclasses import dataclass
import fcntl
import json
from pathlib import Path
import re
import subprocess
import sys
import time
from uuid import uuid4

REPO = "YuriiMurha/grooviq"
LABEL = "merge-train"
AUTO_MERGE = "BYPASS risk: GitHub auto-merge is enabled"
ACTIONS_APP = 15368
REQUIRED = ("rust (macos-latest)", "rust (windows-latest)", "ui")
PENDING = {None, "queued", "in_progress", "pending", "requested", "waiting"}
FAILED = {"failure", "timed_out", "startup_failure", "action_required", "stale", "neutral", "skipped"}
SHA = re.compile(r"[a-f0-9]{40}")
ENTRY = re.compile(r"([1-9][0-9]*)=([a-f0-9]{40})")
MEMBER = re.compile(r"Train-(PR|Suspect): ([1-9][0-9]*) ([a-f0-9]{40})")
MAIN_REF = "refs/heads/main"
BATCH_REF = "refs/heads/merge-train/batch"


@dataclass
class Batch:
    base: str
    members: list[tuple[int, str]]
    suspects: list[tuple[int, str]]

    def message(self):
        return "\n".join([
            "Merge train batch", "", f"Train-Base: {self.base}",
            *(f"Train-PR: {number} {head}" for number, head in self.members),
            *(f"Train-Suspect: {number} {head}" for number, head in self.suspects),
        ])

    @classmethod
    def parse(cls, message):
        def malformed():
            return RuntimeError("Missing or malformed batch trailers.")

        first, *lines = message.strip().split("\n\n")[-1].split("\n")
        base = first.removeprefix("Train-Base: ")
        if first == base or not SHA.fullmatch(base):
            raise malformed()
        members, suspects, seen = [], [], set()
        for line in lines:
            match = MEMBER.fullmatch(line)
            if match is None:
                raise malformed()
            kind, number, head = match.groups()
            number = int(number)
            if number in seen or (kind == "PR" and suspects):
                raise malformed()
            seen.add(number)
            (members if kind == "PR" else suspects).append((number, head))
        if not members:
            raise malformed()
        return cls(base, members, suspects)


def parse_order(entries):
    order = {}
    for entry in entries:
        match = ENTRY.fullmatch(entry)
        if match is None:
            raise ValueError(f"Expected PR=<full lowercase SHA>, got {entry!r}.")
        number, head = match.groups()
        number = int(number)
        if number in order:
            raise ValueError(f"PR #{number} is listed twice in ORDER.")
        order[number] = head
    return order


def labelled(pr):
    return any(label["name"] == LABEL for label in pr["labels"])


def hold_reason(pr, head, approved):
    if pr["state"] != "open" or not labelled(pr):
        return "PR closed or no longer enrolled"
    source = pr["head"]["repo"]
    if pr["draft"] or pr["base"]["ref"] != "main" or not source or source["full_name"] != REPO:
        return "expected a ready, same-repository PR into main"
    if pr["auto_merge"] is not None:
        return AUTO_MERGE
    if approved is None:
        return "no approved head"
    if pr["head"]["sha"] != head or head != approved:
        return "head moved; supply the new approved head"
    return None


def check_status(repo, head, *, missing_pending=True):
    checks = repo.checks(head)
    failures, cancelled, pending = [], [], False
    for name in REQUIRED:
        if name not in checks and not missing_pending:
            continue
        state, url = checks.get(name, (None, ""))
        if state in FAILED:
            failures.append(f"{name} ({state})")
        elif state == "cancelled":
            cancelled.append(url or name)
        elif state in PENDING:
            pending = True
        elif state != "success":
            raise RuntimeError(f"Unexpected required check state for {name} on {head}: {state}")
    return ", ".join(failures), ", ".join(cancelled), pending


def reject(repo, number, head, reason):
    if repo.reject(number, head, reason):
        print(f"Removed #{number} from the train: {reason}.")
    else:
        print(f"Held #{number}: the failed head {head} is no longer current.")


def eligible(repo, candidate, order):
    number, head = candidate
    reason = hold_reason(repo.pull(number), head, order.get(number))
    if reason:
        print(f"Held #{number}: {reason}.")
        return False
    failures, cancelled, pending = check_status(repo, head)
    if failures:
        reject(repo, number, head, failures)
        return False
    if cancelled:
        print(f"Held #{number}: re-run {cancelled}.")
    elif pending:
        print(f"Held #{number}: waiting for required checks on {head}.")
    return not (cancelled or pending)


def build_batch(repo, git, base, active, order, limit, members=(), suspects=()):
    git.fetch(base)
    suspects = [candidate for candidate in suspects if eligible(repo, candidate, order)]
    queue = dict(order)
    while members or suspects or queue:
        bisecting = bool(members or suspects)
        if not members:
            if suspects:
                split = min(limit, (len(suspects) + 1) // 2)
                members, suspects = suspects[:split], suspects[split:]
            else:
                members = list(queue.items())
        accepted, deferred = [], []
        parent = base
        for candidate in members:
            number, head = candidate
            queue.pop(number, None)
            if not eligible(repo, candidate, order):
                continue
            if not git.fetch_pull(number, head):
                print(f"Held #{number}: fetched head moved; supply the new approved head.")
                continue
            tree = git.merge_tree(parent, head)
            if tree is None:
                if parent == base or git.merge_tree(base, head) is None:
                    reject(repo, number, head, "conflicts with main; refresh and re-enrol")
                else:
                    print(f"Deferred #{number}: conflicts with an earlier batch member.")
                    deferred.append(candidate)
                continue
            accepted.append(candidate)
            # A squash leaves the PR head out of the ancestry used for the next merge.
            accepted_tree, previous = tree, parent
            parent = git.commit(tree, parent, f"Train squash of #{number}")
            if len(accepted) == limit:
                break
        if accepted:
            carried = deferred + suspects if bisecting else []
            batch = Batch(base, accepted, carried)
            head = git.commit(accepted_tree, previous, batch.message())
            git.publish(head, active)
            print(f"Pushed batch {head} ({numbers(accepted)}).")
            return
        members = ()
    if active:
        git.publish("", active)
    print("No eligible PRs in the merge train.")


def numbers(candidates):
    return " ".join(f"#{number}" for number, _ in candidates)


def error_detail(error):
    detail = getattr(error, "stderr", None)
    if isinstance(detail, bytes):
        detail = detail.decode(errors="replace")
    return detail.strip() if detail else str(error)


def landed_sha(pr):
    head = pr.get("merge_commit_sha")
    if pr["merged"] and isinstance(head, str) and SHA.fullmatch(head):
        return head
    return None


def print_landed(members):
    for number, head in members:
        print(f"Merged #{number} at {head}")


def report_landed(repo, members, *, show_merges=True):
    landed, unknown = [], []
    for number, _ in members:
        try:
            pr = repo.pull(number)
            head = landed_sha(pr)
            if head:
                landed.append((number, head))
            elif pr["merged"]:
                unknown.append(number)
        except Exception as error:
            print(f"Could not verify #{number}: {error_detail(error)}.")
            unknown.append(number)
    if show_merges:
        print_landed(landed)
    prefix = ", ".join(f"#{number} at {head}" for number, head in landed)
    unconfirmed = ", ".join(f"#{number}" for number in unknown)
    return f"Merged so far: {prefix or 'none confirmed'}." + (
        f" Unconfirmed: {unconfirmed}." if unknown else ""
    )


def squash(repo, git, active, number, head, expected_main):
    for attempt in range(4):
        main, current = git.refs()
        if main != expected_main:
            raise RuntimeError("main moved before squash")
        if current != active:
            raise RuntimeError("batch branch moved before squash")
        try:
            return repo.merge(number, head)
        except Exception as error:
            pr = repo.pull(number)
            if pr["merged"]:
                landed = landed_sha(pr)
                main, current = git.refs()
                if not landed or pr["head"]["sha"] != head or main != landed:
                    raise RuntimeError("GitHub's reported squash does not match the approved head and main tip") from error
                if current != active:
                    raise RuntimeError("batch branch moved during squash; leaving it in place") from error
                git.fetch(main)
                if git.first_parent_chain(expected_main, main) != [landed]:
                    raise RuntimeError("GitHub's reported squash has an unexpected parent") from error
                return landed
            detail = error_detail(error).lower()
            if (attempt == 3 or not isinstance(error, subprocess.CalledProcessError)
                    or "http 405" not in detail or "base branch was modified" not in detail):
                raise
            time.sleep(0.25 * (attempt + 1))


def verify_landed_tree(git, active, main, tested_head, mismatch_file, ack_mismatch=None):
    git.fetch(main)
    landed, tested = git.tree(main), git.tree(tested_head)
    if landed != tested:
        if ack_mismatch == active:
            print(f"Acknowledged tree mismatch for batch {active}.")
            return
        if mismatch_file is not None:
            temp = mismatch_file.with_suffix(".tmp")
            temp.write_text(f"{active}\n")
            temp.replace(mismatch_file)
        raise RuntimeError(f"main tree {landed} != tested tree {tested}; "
                           f"investigate before passing --ack-mismatch {active}")


def recover_prefix(git, active, batch, main, pulls, mismatch_file, ack_mismatch):
    if main == batch.base:
        return 0
    landed = []
    for (number, head), pr in zip(batch.members, pulls):
        squash_head = landed_sha(pr)
        if not squash_head or pr["head"]["sha"] != head:
            break
        landed.append((number, squash_head))
    if not landed:
        return 0
    git.fetch(main)
    if git.first_parent_chain(batch.base, main) != [head for _, head in landed]:
        return 0
    print_landed(landed)
    tested = git.first_parent_chain(batch.base, active)
    if len(tested) != len(batch.members):
        raise RuntimeError("Tested commit does not match its trailers.")
    verify_landed_tree(git, active, main, tested[len(landed) - 1], mismatch_file, ack_mismatch)
    print(f"Recovered landed prefix of batch {active} ({numbers(landed)}).")
    return len(landed)


def merge_batch(repo, git, active, batch, order, mismatch_file):
    for number, head in batch.members:
        reason = hold_reason(repo.pull(number), head, order.get(number))
        if reason:
            raise RuntimeError(f"Held #{number} before merge: {reason}.")
    git.verify_chain(active, batch)
    merged = []
    expected_main = batch.base
    try:
        for number, head in batch.members:
            expected_main = squash(repo, git, active, number, head, expected_main)
            merged.append((number, expected_main))
        main, current = git.refs()
    except (Exception, KeyboardInterrupt) as error:
        report = report_landed(repo, batch.members)
        raise RuntimeError(f"Stopped at #{number}: {error_detail(error)}. {report}") from error

    print_landed(merged)
    try:
        verify_landed_tree(git, active, main, active, mismatch_file)
        if current != active:
            raise RuntimeError("batch branch moved during squash; leaving it in place.")
    except (Exception, KeyboardInterrupt) as error:
        report = report_landed(repo, batch.members, show_merges=False)
        raise RuntimeError(f"{error_detail(error)}. {report}") from error
    return main


def tick(repo, git, order, limit=4, *, mismatch_file=None, ack_mismatch=None):
    if not order:
        raise RuntimeError("ORDER requires at least one approved PR head.")
    if limit < 1:
        raise RuntimeError("Batch size must be positive.")
    repo.verify_policy()
    base, active = git.refs()
    blocked = mismatch_file.read_text().strip() if mismatch_file is not None and mismatch_file.exists() else ""
    if blocked and ack_mismatch != blocked:
        raise RuntimeError(f"Tree mismatch is blocked; investigate before passing --ack-mismatch {blocked}.")
    if ack_mismatch and ack_mismatch != (blocked or active):
        raise RuntimeError("The mismatch acknowledgement must name the blocked or active batch SHA.")
    if blocked:
        mismatch_file.unlink()
        print(f"Acknowledged recorded mismatch for batch {blocked}.")
    for number in sorted(set(repo.enrolled()) - order.keys()):
        warning = f"; {AUTO_MERGE}" if repo.pull(number)["auto_merge"] is not None else ""
        print(f"Held #{number}: no approved head{warning}.")
    if active:
        git.fetch(active)
        batch = Batch.parse(git.message(active))
        pulls = [repo.pull(number) for number, _ in batch.members]
        recovered = recover_prefix(git, active, batch, base, pulls, mismatch_file, ack_mismatch)
        if recovered:
            landed = {number for number, _ in batch.members[:recovered]}
            remaining = {number: head for number, head in order.items() if number not in landed}
            build_batch(repo, git, base, active, remaining, limit,
                        batch.members[recovered:recovered + limit],
                        batch.members[recovered + limit:] + batch.suspects)
            return
        changed = base != batch.base
        if changed:
            print(f"Abandoned batch {active}: main moved from {batch.base} to {base}.")
        for (number, head), pr in zip(batch.members, pulls):
            reason = hold_reason(pr, head, order.get(number))
            if pr["merged"]:
                reason = "BYPASS: member already merged outside this tick"
            if reason:
                print(f"Abandoned batch {active}: #{number} {reason}.")
                changed = True
        if not changed and all(eligible(repo, candidate, order) for candidate in batch.members):
            failures, cancelled, pending = check_status(repo, active)
            if failures:
                main_failures, _, main_pending = check_status(repo, base, missing_pending=False)
                if main_failures:
                    print(f"Held batch {active}: main is red ({main_failures}).")
                    return
                if main_pending:
                    print(f"Waiting for required checks on main at {base}.")
                    return
                if len(batch.members) > 1:
                    split = min(limit, (len(batch.members) + 1) // 2)
                    print(f"Bisecting batch {active}: {failures}.")
                    build_batch(repo, git, base, active, order, limit,
                                batch.members[:split], batch.members[split:] + batch.suspects)
                    return
                number, head = batch.members[0]
                reject(repo, number, head, f"batch {active}: {failures}")
                order = {n: sha for n, sha in order.items() if n != number}
            elif cancelled:
                print(f"Held batch {active}: re-run {cancelled}.")
                return
            elif pending:
                print(f"Waiting for batch {active} ({numbers(batch.members)}).")
                return
            else:
                base = merge_batch(repo, git, active, batch, order, mismatch_file)
                order = {n: sha for n, sha in order.items() if (n, sha) not in batch.members}
            build_batch(repo, git, base, active, order, limit, suspects=batch.suspects)
            return
    build_batch(repo, git, base, active, order, limit)


class GitHub:
    def api(self, path, *, method="GET", body=None, paginate=False):
        argv = ["gh", "api", "--method", method, f"repos/{REPO}/{path}"]
        if paginate:
            argv += ["--paginate", "--slurp"]
        if body is not None:
            argv += ["--input", "-"]
        # The caller reconciles remote state before retrying a failed write.
        result = subprocess.run(
            argv, input="" if body is None else json.dumps(body),
            text=True, capture_output=True, check=True, timeout=45,
        )
        return json.loads(result.stdout) if result.stdout else None

    def enrolled(self):
        pages = self.api(f"issues?state=open&labels={LABEL}&per_page=100", paginate=True)
        return [item["number"] for page in pages for item in page if "pull_request" in item]

    def pull(self, number):
        return self.api(f"pulls/{number}")

    def verify_policy(self):
        policy = self.api("branches/main/protection/required_status_checks")
        contexts = [(check["context"], check["app_id"]) for check in policy["checks"]]
        if policy["strict"] is not False:
            raise RuntimeError("Main's required-check policy must have strict=false.")
        if len(contexts) != len(REQUIRED) or set(contexts) != {(name, ACTIONS_APP) for name in REQUIRED}:
            raise RuntimeError("Main's required-check policy must match the three GitHub Actions checks.")

    def checks(self, sha):
        pages = self.api(f"commits/{sha}/check-runs?filter=latest&per_page=100", paginate=True)
        checks = {}
        for page in pages:
            for check in page["check_runs"]:
                name = check["name"]
                if (name in REQUIRED and check["head_sha"] == sha
                        and (check.get("app") or {}).get("id") == ACTIONS_APP):
                    if name in checks:
                        raise RuntimeError(f"Ambiguous required check {name} on {sha}.")
                    state = check["conclusion"] if check["status"] == "completed" else check["status"]
                    if state is None:
                        raise RuntimeError(f"Missing conclusion for completed check {name} on {sha}.")
                    url = check.get("details_url") or check.get("html_url") or ""
                    checks[name] = state, url.split("/job/", 1)[0]
        return checks

    def reject(self, number, sha, reason):
        def still_current():
            pr = self.pull(number)
            return pr["state"] == "open" and pr["head"]["sha"] == sha and labelled(pr)

        if not still_current():
            return False
        self.api(f"issues/{number}/comments", method="POST",
                 body={"body": f"Merge train rejected `{sha}`: {reason}. "
                               "The train will remove that head from enrollment."})
        if not still_current():
            return False
        # Label removal has no expected-head parameter, unlike merge.
        self.api(f"issues/{number}/labels/{LABEL}", method="DELETE")
        return True

    def merge(self, number, sha):
        result = self.api(f"pulls/{number}/merge", method="PUT",
                          body={"sha": sha, "merge_method": "squash"})
        if not isinstance(result, dict):
            raise RuntimeError(f"Invalid merge response for #{number}.")
        if result.get("merged") is not True:
            raise RuntimeError(f"GitHub rejected the merge of #{number}: {result.get('message')}")
        head = result.get("sha")
        if not isinstance(head, str) or not SHA.fullmatch(head):
            raise RuntimeError(f"GitHub did not return a squash SHA for #{number}.")
        return head


class Git:
    def __init__(self, cwd=None):
        self.cwd = cwd

    def run(self, *args, check=True):
        return subprocess.run(["git", *args], cwd=self.cwd, check=check,
                              capture_output=True, text=True, timeout=90)

    def remote_refs(self, *refs):
        result = self.run("ls-remote", "--refs", "origin", *refs)
        return {ref: head for head, ref in map(str.split, result.stdout.splitlines())}

    def refs(self):
        refs = self.remote_refs(MAIN_REF, BATCH_REF)
        if MAIN_REF not in refs:
            raise RuntimeError("origin has no main branch.")
        return refs[MAIN_REF], refs.get(BATCH_REF, "")

    def fetch(self, head):
        self.run("fetch", "--no-tags", "--no-write-fetch-head", "--no-recurse-submodules",
                 "--refmap=", "origin", head)

    def fetch_pull(self, number, head):
        ref = f"refs/pull/{number}/head"
        # Read the fetched ref through a private name so another fetch cannot replace it.
        local = f"refs/grooviq/merge-train/{uuid4().hex}"
        try:
            result = self.run("fetch", "--no-tags", "--no-write-fetch-head", "--no-recurse-submodules",
                              "--refmap=", "origin", f"{ref}:{local}", check=False)
            if result.returncode and self.remote_refs(ref).get(ref) != head:
                return False
            result.check_returncode()
            return self.run("rev-parse", "--verify", local).stdout.strip() == head
        finally:
            self.run("update-ref", "-d", local)

    def merge_tree(self, parent, head):
        result = self.run("merge-tree", "--write-tree", parent, head, check=False)
        if result.returncode == 1:
            return None
        result.check_returncode()
        return result.stdout.strip()

    def commit(self, tree, parent, message):
        return self.run("commit-tree", tree, "-p", parent, "-m", message).stdout.strip()

    def message(self, head):
        return self.run("show", "-s", "--format=%B", head).stdout

    def tree(self, head):
        return self.run("rev-parse", f"{head}^{{tree}}").stdout.strip()

    def first_parent_chain(self, base, head):
        commits = self.run("rev-list", "--first-parent", "--parents", "--reverse",
                           f"{base}..{head}").stdout.splitlines()
        if not commits or commits[0].split()[1:2] != [base]:
            return []
        return [line.split()[0] for line in commits]

    def verify_chain(self, head, batch):
        commits = self.run("rev-list", "--first-parent", "--parents", "--reverse",
                           f"{batch.base}..{head}").stdout.splitlines()
        parent = batch.base
        if len(commits) != len(batch.members):
            raise RuntimeError("Tested commit does not match its trailers.")
        for line, (number, approved) in zip(commits, batch.members):
            commit, *parents = line.split()
            if not self.fetch_pull(number, approved):
                raise RuntimeError(f"Held #{number}: fetched head moved; supply the new approved head.")
            if parents != [parent] or self.merge_tree(parent, approved) != self.tree(commit):
                raise RuntimeError("Tested commit does not match its trailers.")
            parent = commit

    def publish(self, head, expected):
        self.run("push", "origin", f"{head}:{BATCH_REF}", f"--force-with-lease={BATCH_REF}:{expected}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--order", nargs="*", default=[], metavar="PR=SHA",
                        help="priority PR numbers and exact approved heads")
    parser.add_argument("--batch", type=int, default=4, help="maximum members per batch")
    parser.add_argument("--ack-mismatch", metavar="SHA", help="acknowledge an investigated tree mismatch")
    try:
        args = parser.parse_args()
    except SystemExit as error:
        return int(bool(error.code))
    try:
        order = parse_order(args.order)
        if not order:
            raise ValueError("ORDER requires at least one approved PR head.")
        if args.ack_mismatch and not SHA.fullmatch(args.ack_mismatch):
            raise ValueError("--ack-mismatch requires a full lowercase commit SHA.")
    except ValueError as error:
        print(f"Merge train stopped: {error}", file=sys.stderr)
        return 1

    try:
        common_dir = subprocess.check_output(
            ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"], text=True,
        ).strip()
        with (Path(common_dir) / "merge-train.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            tick(GitHub(), Git(), order, args.batch,
                 mismatch_file=Path(common_dir) / "merge-train-mismatch",
                 ack_mismatch=args.ack_mismatch)
    except (OSError, subprocess.SubprocessError, RuntimeError, ValueError, KeyError) as error:
        print(f"Merge train stopped: {error_detail(error)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
