from contextlib import redirect_stdout
from copy import deepcopy
import io
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
import unittest

import merge_train as train
from merge_train import ACTIONS_APP, GitHub, REQUIRED


def sha(number):
    return f"{number:040x}"


BASE, ACTIVE = sha(100), sha(200)
GREEN = {name: ("success", "") for name in REQUIRED}


class Repo:
    def __init__(self):
        self.prs = {
            n: {
                "state": "open", "merged": False, "draft": False, "auto_merge": None,
                "head": {"sha": sha(n), "repo": {"full_name": "YuriiMurha/grooviq"}},
                "base": {"ref": "main"}, "labels": [{"name": "merge-train"}],
            }
            for n in (10, 20, 30, 40, 50, 60)
        }
        self.states = {}
        self.rejected = []
        self.merged = []
        self.merge_error = None
        self.merge_exception = RuntimeError("merge refused")
        self.policy_error = False

    def verify_policy(self):
        if self.policy_error:
            raise RuntimeError("policy changed")

    def enrolled(self):
        return [n for n, pr in self.prs.items() if pr["state"] == "open" and pr["labels"]]

    def pull(self, number):
        return deepcopy(self.prs[number])

    def checks(self, head):
        return self.states.get(head, GREEN)

    def reject(self, number, head, reason):
        self.rejected.append((number, head, reason))
        self.prs[number]["labels"] = []
        return True

    def merge(self, number, head):
        if number == self.merge_error:
            raise self.merge_exception
        self.merged.append((number, head))
        self.prs[number].update(state="closed", merged=True)
        return sha(number + 300)


class FakeGit:
    def __init__(self, repo):
        self.repo = repo
        self.base = BASE
        self.active = ""
        self.description = ""
        self.commits = {}
        self.writes = []
        self.conflicts = set()
        self.ref_moved = set()
        self.landed_tree = sha(900)
        self.tested_tree = sha(900)
        self.validated = True

    def refs(self):
        return ((sha(300 + self.repo.merged[-1][0]) if self.repo.merged else self.base),
                self.active)

    def fetch(self, head):
        pass

    def fetch_pull(self, number, head):
        return number not in self.ref_moved

    def message(self, head):
        return self.description

    def merge_tree(self, parent, head):
        if (parent, head) in self.conflicts:
            return None
        return self.tested_tree

    def commit(self, tree, parent, message):
        result = sha(500 + len(self.commits))
        self.commits[result] = (tree, parent, message)
        return result

    def verify_chain(self, head, batch):
        if not self.validated:
            raise RuntimeError("tested commit does not match its trailers")

    def tree(self, head):
        return self.tested_tree if head == self.active else self.landed_tree

    def publish(self, head, expected):
        self.writes.append((head, expected))

    def batch(self, members, suspects=()):
        self.active = ACTIVE
        self.description = "\n".join([
            "Merge train batch", "", f"Train-Base: {self.base}",
            *(f"Train-PR: {n} {sha(n)}" for n in members),
            *(f"Train-Suspect: {n} {sha(n)}" for n in suspects),
        ])


class MergeTrainTests(unittest.TestCase):
    def setUp(self):
        self.repo = Repo()
        self.git = FakeGit(self.repo)

    def run_tick(self, order=(10, 20, 30, 40, 50, 60), limit=4):
        self.output = io.StringIO()
        with redirect_stdout(self.output):
            train.tick(self.repo, self.git, {n: sha(n) for n in order}, limit)
        return self.output.getvalue()

    def written(self, members, suspects=(), expected="", base=BASE):
        [(head, lease)] = self.git.writes
        self.assertEqual(lease, expected)
        _, parent, message = self.git.commits[head]
        for _ in members[:0:-1]:
            _, parent, intermediate = self.git.commits[parent]
            self.assertTrue(intermediate)
        self.assertEqual(parent, base)
        self.assertEqual(message.splitlines()[2:], [
            f"Train-Base: {base}",
            *(f"Train-PR: {n} {sha(n)}" for n in members),
            *(f"Train-Suspect: {n} {sha(n)}" for n in suspects),
        ])

    def test_batch_is_bounded_and_follows_approved_priority(self):
        self.run_tick((30, 10, 20, 40), limit=2)
        self.written([30, 10])
        self.assertFalse(self.repo.merged)

    def test_bad_policy_blocks_even_a_green_batch(self):
        self.git.batch([10])
        self.repo.policy_error = True
        with self.assertRaisesRegex(RuntimeError, "policy changed"):
            self.run_tick()
        self.assertFalse(self.repo.merged or self.git.writes)

    def test_only_eligible_pulls_join_the_batch(self):
        self.repo.prs[10]["labels"] = []
        self.repo.prs[20]["draft"] = True
        self.repo.prs[30]["base"]["ref"] = "side"
        self.repo.prs[40]["head"]["repo"]["full_name"] = "fork/grooviq"
        self.repo.prs[50]["state"] = "closed"
        self.run_tick()
        self.written([60])

    def test_moved_head_auto_merge_and_missing_approval_are_held(self):
        self.repo.prs[10]["head"]["sha"] = sha(99)
        self.repo.prs[20]["auto_merge"] = {}
        output = self.run_tick((10, 20, 30))
        self.written([30])
        self.assertIn("head moved; supply the new approved head", output)
        self.assertIn("bypass", output.lower())
        self.assertIn("#40", output)
        self.assertIn("no approved head", output)
        self.assertFalse(self.repo.rejected)

    def test_auto_merge_is_reported_even_without_an_approved_head(self):
        self.repo.prs[20]["auto_merge"] = {}
        output = self.run_tick((10,))
        self.written([10])
        self.assertRegex(output.lower(), r"#20:[^\n]*bypass")

    def test_pending_missing_and_cancelled_head_checks_allow_later_candidates(self):
        self.repo.states[sha(10)] = {**GREEN, "ui": ("in_progress", "")}
        self.repo.states[sha(20)] = {}
        self.repo.states[sha(30)] = {**GREEN, "ui": ("cancelled", "https://run/30")}
        self.assertIn("re-run", self.run_tick((10, 20, 30, 40)))
        self.written([40])
        self.assertFalse(self.repo.rejected)

    def test_each_required_failure_conclusion_rejects_the_head(self):
        for conclusion in ("failure", "timed_out", "startup_failure", "action_required",
                           "stale", "neutral", "skipped"):
            with self.subTest(conclusion=conclusion):
                self.setUp()
                self.repo.states[sha(10)] = {**GREEN, "ui": (conclusion, "")}
                self.run_tick((10, 20))
                self.written([20])
                [(number, head, reason)] = self.repo.rejected
                self.assertEqual((number, head), (10, sha(10)))
                self.assertIn(f"ui ({conclusion})", reason)

    def test_fetched_pr_head_must_match_the_approved_sha(self):
        self.git.ref_moved.add(10)
        self.run_tick((10, 20))
        self.written([20])
        self.assertFalse(self.repo.rejected)

    def test_conflicts_with_main_are_rejected_and_batch_only_conflicts_deferred(self):
        self.git.conflicts.update({(sha(500), sha(20)), (BASE, sha(20)),
                                   (sha(500), sha(30))})
        self.run_tick((10, 20, 30, 40))
        self.written([10, 40])
        [(number, head, reason)] = self.repo.rejected
        self.assertEqual((number, head), (20, sha(20)))
        self.assertIn("refresh and re-enrol", reason)

    def test_incomplete_or_malformed_trailers_stop_before_writes(self):
        for trailers in ("", f"Train-Base: {BASE}", "Train-PR: 10 invalid",
                         f"Train-Base: {BASE}\nTrain-PR: 10 {sha(10)}\nTrain-Suspect: 10 {sha(10)}"):
            with self.subTest(trailers=trailers):
                self.git.active = ACTIVE
                self.git.description = f"Merge train batch\n\n{trailers}"
                with self.assertRaisesRegex(RuntimeError, "trailers"):
                    self.run_tick()
                self.assertFalse(self.repo.merged or self.git.writes)

    def test_main_advance_abandons_and_rebuilds_with_a_lease(self):
        self.git.batch([10])
        self.git.base = sha(101)
        self.assertIn("main moved", self.run_tick((10, 20)))
        self.written([10, 20], expected=ACTIVE, base=sha(101))

    def test_outside_merge_logs_bypass_and_rebuilds(self):
        self.git.batch([10, 20])
        self.repo.prs[10].update(state="closed", merged=True)
        self.git.base = sha(101)
        self.assertIn("bypass", self.run_tick((10, 20)).lower())
        self.written([20], expected=ACTIVE, base=sha(101))

    def test_changed_head_label_or_open_state_abandons(self):
        for fields in ({"head": {"sha": sha(99), "repo": {"full_name": "YuriiMurha/grooviq"}}},
                       {"labels": []}, {"state": "closed"}):
            with self.subTest(fields=fields):
                self.setUp()
                self.git.batch([10])
                self.repo.prs[10].update(fields)
                self.run_tick((10, 20))
                self.written([20], expected=ACTIVE)
                self.assertFalse(self.repo.merged)

    def test_green_batch_cannot_merge_a_member_without_current_approval(self):
        self.git.batch([10])
        self.assertIn("no approved head", self.run_tick((20,)))
        self.written([20], expected=ACTIVE)
        self.assertFalse(self.repo.merged)

    def test_green_batch_cannot_merge_a_member_with_auto_merge_enabled(self):
        self.git.batch([10])
        self.repo.prs[10]["auto_merge"] = {}
        self.assertIn("bypass", self.run_tick((10, 20)).lower())
        self.written([20], expected=ACTIVE)
        self.assertFalse(self.repo.merged)

    def test_missing_batch_check_waits(self):
        self.git.batch([10, 20])
        self.repo.states[ACTIVE] = {}
        self.assertIn(f"Waiting for batch {ACTIVE} (#10 #20)", self.run_tick())
        self.assertFalse(self.repo.merged or self.git.writes)

    def test_cancelled_batch_gives_run_url_without_bisection(self):
        self.git.batch([10, 20])
        self.repo.states[ACTIVE] = {"ui": ("cancelled", "https://run/123")}
        self.assertIn("https://run/123", self.run_tick())
        self.assertFalse(self.repo.merged or self.repo.rejected or self.git.writes)

    def test_failed_batch_bisects_first_rounded_half_before_old_suspects(self):
        self.git.batch([10, 20, 30], [40, 50])
        self.repo.states[ACTIVE] = {**GREEN, "ui": ("failure", ""),
                                    REQUIRED[0]: ("cancelled", "https://run/123")}
        self.run_tick()
        self.written([10, 20], [30, 40, 50], expected=ACTIVE)
        self.assertFalse(self.repo.merged or self.repo.rejected)

    def test_failed_singleton_is_rejected_and_suspects_revalidated(self):
        self.git.batch([10], [20, 30, 40])
        self.repo.prs[20]["draft"] = True
        self.repo.states[ACTIVE] = {**GREEN, "ui": ("failure", "")}
        self.assertIn("#20", self.run_tick())
        [(number, head, reason)] = self.repo.rejected
        self.assertEqual((number, head), (10, sha(10)))
        self.assertIn(ACTIVE, reason)
        self.assertIn("ui (failure)", reason)
        self.written([30], [40], expected=ACTIVE)

    def test_green_batch_squashes_in_order_and_builds_half_the_suspects(self):
        self.git.batch([20, 10], [30, 40, 50])
        output = self.run_tick()
        self.assertEqual(self.repo.merged, [(20, sha(20)), (10, sha(10))])
        self.assertIn(f"Merged #20 at {sha(320)}", output)
        self.assertIn(f"Merged #10 at {sha(310)}", output)
        self.written([30, 40], [50], expected=ACTIVE, base=sha(310))

    def test_green_batch_merges_only_after_rechecking_main(self):
        self.git.batch([10])
        snapshots = iter([(BASE, ACTIVE), (sha(101), ACTIVE)])
        read_refs = self.git.refs
        self.git.refs = lambda: next(snapshots, read_refs())
        with self.assertRaisesRegex(RuntimeError, "main moved"):
            self.run_tick()
        self.assertFalse(self.repo.merged or self.git.writes)

    def test_green_batch_rechecks_member_head_after_checks(self):
        self.git.batch([10])
        original = self.repo.checks

        def checks(head):
            if head == ACTIVE:
                self.repo.prs[10]["head"]["sha"] = sha(99)
            return original(head)

        self.repo.checks = checks
        with self.assertRaisesRegex(RuntimeError, "head moved"):
            self.run_tick()
        self.assertFalse(self.repo.merged or self.git.writes)

    def test_unrelated_green_batch_commit_cannot_authorize_merges(self):
        self.git.batch([10])
        self.git.validated = False
        with self.assertRaisesRegex(RuntimeError, "does not match its trailers"):
            self.run_tick()
        self.assertFalse(self.repo.merged or self.git.writes)

    def test_outside_main_advance_stops_before_the_next_squash(self):
        self.git.batch([10, 20])
        snapshots = iter([(BASE, ACTIVE), (BASE, ACTIVE), (sha(101), ACTIVE)])
        read_refs = self.git.refs
        self.git.refs = lambda: next(snapshots, read_refs())
        with self.assertRaisesRegex(RuntimeError, "Merged so far: #10"):
            self.run_tick()
        self.assertEqual(self.repo.merged, [(10, sha(10))])
        self.assertFalse(self.git.writes)

    def test_partial_merge_stops_with_the_committed_prefix(self):
        self.git.batch([20, 10, 30])
        self.repo.merge_error = 10
        with self.assertRaisesRegex(RuntimeError, "Merged so far: #20"):
            self.run_tick()
        self.assertEqual(self.repo.merged, [(20, sha(20))])
        self.assertFalse(self.git.writes)

    def test_malformed_merge_response_reports_the_committed_prefix(self):
        self.git.batch([20, 10])
        self.repo.merge_error = 10
        self.repo.merge_exception = ValueError("invalid JSON response")
        with self.assertRaisesRegex(RuntimeError, "Merged so far: #20"):
            self.run_tick()
        self.assertEqual(self.repo.merged, [(20, sha(20))])
        self.assertFalse(self.git.writes)

    def test_tree_mismatch_stops_the_tick(self):
        self.git.batch([10])
        self.git.landed_tree = sha(901)
        with self.assertRaisesRegex(RuntimeError, f"main tree {sha(901)} != tested tree {sha(900)}"):
            self.run_tick()
        self.assertFalse(self.git.writes)

    def test_green_batch_deletes_the_branch_when_queue_is_empty(self):
        self.git.batch([10])
        self.run_tick((10,))
        self.assertEqual(self.git.writes, [("", ACTIVE)])

    def test_order_rejects_bad_entries_duplicate_prs_and_nonpositive_limit(self):
        self.assertEqual(train.parse_order([f"20={sha(20)}", f"10={sha(10)}"]),
                         {20: sha(20), 10: sha(10)})
        for entries in (["20"], ["20=123"], [f"0={sha(20)}"],
                        [f"20={sha(10)}", f"20={sha(20)}"]):
            with self.subTest(entries=entries), self.assertRaises(ValueError):
                train.parse_order(entries)
        with self.assertRaises(RuntimeError):
            self.run_tick(limit=0)
        self.assertFalse(self.git.writes)


class CliTests(unittest.TestCase):
    def test_invalid_cli_arguments_exit_one(self):
        script = Path(__file__).with_name("merge_train.py")
        result = subprocess.run([sys.executable, "-B", str(script), "--batch", "many"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("--batch", result.stderr)


class GitHubTests(unittest.TestCase):
    def test_policy_requires_exact_contexts_and_apps_and_strict_false(self):
        repo = GitHub()
        expected = [{"context": name, "app_id": ACTIONS_APP} for name in REQUIRED]
        for strict, checks in ((True, expected), (None, expected), (False, expected[:-1]),
                               (False, expected + [expected[0]]),
                               (False, [*expected[:-1], {"context": "ui", "app_id": None}])):
            with self.subTest(strict=strict, checks=checks):
                repo.api = lambda path: {"strict": strict, "checks": checks}
                with self.assertRaisesRegex(RuntimeError, "policy"):
                    repo.verify_policy()
        repo.api = lambda path: {"strict": False, "checks": expected}
        repo.verify_policy()

    def test_check_reader_uses_latest_and_filters_check_app_and_commit(self):
        repo = GitHub()
        head = sha(10)
        check = {"name": "ui", "head_sha": head, "status": "completed",
                 "conclusion": "cancelled", "app": {"id": ACTIONS_APP},
                 "details_url": "https://github.com/owner/repo/actions/runs/10/job/20"}

        def api(path, **kwargs):
            self.assertEqual(path, f"commits/{head}/check-runs?filter=latest&per_page=100")
            self.assertEqual(kwargs, {"paginate": True})
            return [{"check_runs": [{**check, "head_sha": sha(20)},
                                    {**check, "app": {"id": 9}},
                                    {**check, "app": None}, check]}]

        repo.api = api
        self.assertEqual(repo.checks(head), {
            "ui": ("cancelled", "https://github.com/owner/repo/actions/runs/10"),
        })

    def test_duplicate_or_incomplete_required_check_is_ambiguous(self):
        repo = GitHub()
        check = {"name": "ui", "head_sha": sha(10), "status": "completed",
                 "conclusion": "success", "app": {"id": ACTIONS_APP}}
        for checks in ([check, check], [{**check, "conclusion": None}]):
            with self.subTest(checks=checks):
                repo.api = lambda *args, **kwargs: [{"check_runs": checks}]
                with self.assertRaises(RuntimeError):
                    repo.checks(sha(10))

    def test_changed_head_after_comment_keeps_label(self):
        repo = GitHub()
        calls = []
        heads = iter([sha(10), sha(20)])
        repo.api = lambda *args, **kwargs: calls.append((args, kwargs))
        repo.pull = lambda n: {"state": "open", "head": {"sha": next(heads)},
                               "labels": [{"name": "merge-train"}]}
        self.assertFalse(repo.reject(10, sha(10), "ui (failure)"))
        self.assertEqual([kwargs["method"] for _, kwargs in calls], ["POST"])
        self.assertIn(sha(10), calls[0][1]["body"]["body"])

    def test_unchanged_head_is_unlabelled_after_comment(self):
        repo = GitHub()
        calls = []
        repo.api = lambda *args, **kwargs: calls.append((args, kwargs))
        repo.pull = lambda n: {"state": "open", "head": {"sha": sha(10)},
                               "labels": [{"name": "merge-train"}]}
        self.assertTrue(repo.reject(10, sha(10), "ui (failure)"))
        self.assertEqual(calls[-1], (("issues/10/labels/merge-train",), {"method": "DELETE"}))

    def test_merge_pins_head_uses_squash_and_returns_committed_sha(self):
        repo = GitHub()

        def api(path, **kwargs):
            self.assertEqual((path, kwargs), (
                "pulls/10/merge",
                {"method": "PUT", "body": {"sha": sha(10), "merge_method": "squash"}},
            ))
            return {"merged": True, "sha": sha(310)}

        repo.api = api
        self.assertEqual(repo.merge(10, sha(10)), sha(310))
        repo.api = lambda *args, **kwargs: {"merged": False, "message": "Main advanced"}
        with self.assertRaisesRegex(RuntimeError, "Main advanced"):
            repo.merge(10, sha(10))

    def test_missing_squash_sha_stops_the_tick(self):
        repo = GitHub()
        repo.api = lambda *args, **kwargs: {"merged": True}
        with self.assertRaisesRegex(RuntimeError, "squash SHA"):
            repo.merge(10, sha(10))


class GitIntegrationTest(unittest.TestCase):
    def test_local_origin_push_compare_and_lease(self):
        with TemporaryDirectory() as root:
            src = Path(root) / "source"
            remote = Path(root) / "remote.git"

            def git(*args, cwd=src):
                return subprocess.check_output(["git", *args], cwd=cwd, text=True,
                                               stderr=subprocess.PIPE).strip()

            git("init", "--bare", str(remote), cwd=root)
            git("init", "-b", "main", str(src), cwd=root)
            git("config", "user.name", "Test")
            git("config", "user.email", "train@example.invalid")
            git("remote", "add", "origin", str(remote))
            git("commit", "--allow-empty", "-m", "base")
            base = git("rev-parse", "HEAD")
            heads = []
            for n in (10, 20):
                git("switch", "--detach", base)
                (src / str(n)).write_text(f"PR {n}\n")
                git("add", str(n))
                git("commit", "-m", f"PR {n}")
                heads.append(git("rev-parse", "HEAD"))
                git("push", "origin", f"HEAD:refs/pull/{n}/head")
            git("push", "origin", "main")
            observer = Path(root) / "observer"
            git("clone", "-b", "main", str(remote), str(observer), cwd=root)
            git("config", "user.name", "Test", cwd=observer)
            git("config", "user.email", "train@example.invalid", cwd=observer)
            git_layer = train.Git(observer)
            self.assertEqual(git_layer.refs(), (base, ""))
            self.assertFalse(git_layer.fetch_pull(10, heads[1]))
            for n, head in zip((10, 20), heads):
                self.assertTrue(git_layer.fetch_pull(n, head))
            batch = train.Batch(base, list(zip((10, 20), heads)), [(30, sha(30))])
            first = git_layer.commit(git_layer.merge_tree(base, heads[0]), base, "PR 10")
            active = git_layer.commit(git_layer.merge_tree(first, heads[1]), first, batch.message())
            self.assertEqual(train.Batch.parse(git_layer.message(active)), batch)
            self.assertEqual(git("rev-list", "--first-parent", f"{base}..{active}", cwd=observer).splitlines(),
                             [active, first])
            git_layer.verify_chain(active, batch)
            forged = git_layer.commit(git_layer.tree(base), first, batch.message())
            with self.assertRaisesRegex(RuntimeError, "trailers"):
                git_layer.verify_chain(forged, batch)
            git_layer.publish(active, "")
            self.assertEqual(git_layer.refs(), (base, active))
            with self.assertRaises(subprocess.CalledProcessError):
                git_layer.publish(first, "")
            with self.assertRaises(subprocess.CalledProcessError):
                git_layer.publish("", first)
            self.assertEqual(git_layer.refs(), (base, active))

            git("switch", "main")
            for head in heads:
                git("merge", "--squash", head)
                git("commit", "-m", "server squash")
            git("push", "origin", "main")
            landed, _ = git_layer.refs()
            self.assertNotEqual(landed, active)
            git_layer.fetch(landed)
            self.assertEqual(git_layer.tree(landed), git_layer.tree(active))
            git_layer.publish("", active)
            self.assertEqual(git_layer.refs(), (landed, ""))

            class MovingRefGit(train.Git):
                def run(self, *args, **kwargs):
                    if args[0] == "fetch":
                        git("push", "--force", "origin", f"{heads[1]}:refs/pull/10/head")
                    return super().run(*args, **kwargs)

            self.assertFalse(MovingRefGit(observer).fetch_pull(10, heads[0]))


if __name__ == "__main__":
    unittest.main()
