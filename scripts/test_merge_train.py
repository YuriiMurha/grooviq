from contextlib import redirect_stderr, redirect_stdout
from copy import deepcopy
import io
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

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
        landed = sha(number + 300)
        self.prs[number].update(state="closed", merged=True, merge_commit_sha=landed)
        return landed


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
        self.history = {}
        self.trees = {}

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
        if tree is None:
            raise TypeError("commit needs a tree")
        result = sha(500 + len(self.commits))
        self.commits[result] = (tree, parent, message)
        return result

    def verify_chain(self, head, batch):
        if not self.validated:
            raise RuntimeError("tested commit does not match its trailers")

    def first_parent_chain(self, base, head):
        return self.history.get((base, head), [])

    def tree(self, head):
        if head in self.trees:
            return self.trees[head]
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

    def run_tick(self, order=(10, 20, 30, 40, 50, 60), limit=4, **kwargs):
        if not isinstance(order, dict):
            order = {n: sha(n) for n in order}
        self.output = io.StringIO()
        with redirect_stdout(self.output):
            train.tick(self.repo, self.git, order, limit, **kwargs)
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

    def test_ui_only_pr_with_successful_selector_can_skip_both_rust_checks(self):
        self.repo.states[sha(10)] = {**GREEN, **{n: ("skipped", "") for n in REQUIRED[:2]},
                                    "changes": ("success", "")}
        self.run_tick((10,))
        self.written([10])
        self.assertFalse(self.repo.rejected)

    def test_skipped_pr_rust_requires_a_successful_selector(self):
        for selector in (None, "cancelled", "in_progress", "failure"):
            with self.subTest(selector=selector):
                self.setUp()
                states = {**GREEN, **{n: ("skipped", "https://run/10") for n in REQUIRED[:2]}}
                if selector is not None:
                    states["changes"] = (selector, "")
                self.repo.states[sha(10)] = states
                self.assertIn("re-run", self.run_tick((10, 20)))
                self.written([20])
                self.assertFalse(self.repo.rejected)
                self.assertTrue(self.repo.prs[10]["labels"])

    def test_successful_selector_does_not_excuse_skipped_pr_ui(self):
        self.repo.states[sha(10)] = {**GREEN, "ui": ("skipped", ""),
                                    "changes": ("success", "")}
        self.run_tick((10, 20))
        self.written([20])
        self.assertEqual(self.repo.rejected, [(10, sha(10), "ui (skipped)")])

    def test_own_head_failure_keeps_enrollment_while_main_is_red_or_pending(self):
        for status, message in (("failure", "main is red"), ("queued", "main is pending")):
            with self.subTest(status=status):
                self.setUp()
                self.repo.states[BASE] = {"ui": (status, "")}
                self.repo.states[sha(10)] = {"ui": ("failure", "")}
                self.assertIn(message, self.run_tick((10, 20)))
                self.assertFalse(self.repo.rejected or self.repo.merged)
                self.assertTrue(self.repo.prs[10]["labels"])
                self.written([20])

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

    def test_pr_trailer_after_a_suspect_is_rejected(self):
        self.git.batch([10], [20])
        self.git.description += f"\nTrain-PR: 30 {sha(30)}"
        with self.assertRaisesRegex(RuntimeError, "trailers"):
            self.run_tick((10, 20, 30))
        self.assertFalse(self.repo.merged or self.git.writes)

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

    def test_green_batch_rejects_a_member_whose_own_head_failed(self):
        self.git.batch([10])
        self.repo.states[sha(10)] = {**GREEN, "ui": ("failure", "")}
        self.run_tick((10, 20))
        self.assertEqual(self.repo.rejected, [(10, sha(10), "ui (failure)")])
        self.assertFalse(self.repo.merged)
        self.written([20], expected=ACTIVE)

    def test_new_approval_for_a_different_head_abandons_green_batch(self):
        self.git.batch([10])
        output = self.run_tick({10: sha(99), 20: sha(20)})
        self.assertIn("Abandoned batch", output)
        self.assertFalse(self.repo.merged)
        self.written([20], expected=ACTIVE)

    def test_red_or_pending_main_holds_failing_batch(self):
        for members in ([10], [10, 20]):
            for status, message in (("failure", "main is red"), ("queued", "Waiting")):
                with self.subTest(members=members, status=status):
                    self.setUp()
                    self.git.batch(members)
                    self.repo.states[BASE] = {"ui": (status, "")}
                    self.repo.states[ACTIVE] = {"ui": ("failure", "")}
                    self.assertIn(message, self.run_tick())
                    self.assertFalse(self.repo.merged or self.repo.rejected or self.git.writes)

    def test_missing_or_cancelled_main_checks_allow_bisection(self):
        for states in ({}, {"ui": ("cancelled", "https://run")}, GREEN):
            with self.subTest(states=states):
                self.setUp()
                self.git.batch([10, 20])
                self.repo.states[BASE] = states
                self.repo.states[ACTIVE] = {"ui": ("failure", "")}
                self.run_tick()
                self.written([10], [20], expected=ACTIVE)

    def test_bisection_respects_a_reduced_batch_limit(self):
        self.git.batch([10, 20, 30, 40, 50], [60])
        self.repo.states[ACTIVE] = {"ui": ("failure", "")}
        self.run_tick(limit=2)
        self.written([10, 20], [30, 40, 50, 60], expected=ACTIVE)

    def test_empty_order_stops_without_deleting_in_flight_batch(self):
        self.git.batch([10])
        with self.assertRaisesRegex(RuntimeError, "ORDER"):
            self.run_tick(())
        self.assertFalse(self.repo.merged or self.git.writes)

    def test_unrecognized_required_check_state_stops_without_writes(self):
        self.git.batch([10])
        self.repo.states[ACTIVE] = {**GREEN, "ui": ("future_state", "")}
        with self.assertRaisesRegex(RuntimeError, "Unexpected required check state.*future_state"):
            self.run_tick((10,))
        self.assertFalse(self.repo.merged or self.repo.rejected or self.git.writes)

    def test_missing_batch_check_waits(self):
        self.git.batch([10, 20])
        self.repo.states[ACTIVE] = {}
        self.assertIn(f"Waiting for batch {ACTIVE} (#10 #20)", self.run_tick())
        self.assertFalse(self.repo.merged or self.git.writes)

    def test_any_skipped_required_batch_check_holds_even_with_successful_selector(self):
        for name in REQUIRED:
            with self.subTest(name=name):
                self.setUp()
                self.git.batch([10, 20])
                self.repo.states[ACTIVE] = {**GREEN, name: ("skipped", "https://run/batch"),
                                            "changes": ("success", "")}
                self.assertIn("re-run https://run/batch", self.run_tick((10, 20)))
                self.assertFalse(self.repo.merged or self.repo.rejected or self.git.writes)

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

    def test_unattended_failure_stops_before_bisection_or_rejection(self):
        for members in ([10, 20], [10]):
            with self.subTest(members=members):
                self.setUp()
                self.git.batch(members)
                self.repo.states[ACTIVE] = {
                    **GREEN, "ui": ("failure", ""), REQUIRED[0]: ("in_progress", ""),
                }
                with self.assertRaisesRegex(
                    RuntimeError, "failed required checks.*ui.*make merge-train ORDER=.*make merge-train-agent-resume"
                    ".*make merge-train-order.*make merge-train-agent-resume",
                ):
                    self.run_tick(agent_deadline=float("inf"))
                self.assertFalse(self.git.writes)
                self.assertFalse(self.repo.rejected)
                self.assertFalse(self.repo.merged)

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
        self.assertIn(f"Tested batch {ACTIVE}", output.splitlines())
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

    def test_train_announces_parseable_squash_attempt_before_merge(self):
        from merge_train_agent import SQUASH_ATTEMPT

        self.git.batch([10, 20])
        merge = self.repo.merge
        before_merges = []

        def inspect_attempt(number, head):
            before_merges.append(self.output.getvalue().splitlines()[-1])
            return merge(number, head)

        self.repo.merge = inspect_attempt
        self.run_tick((10, 20), agent_deadline=time.monotonic() + 60)
        self.assertEqual(self.repo.merged, [(10, sha(10)), (20, sha(20))])
        attempts = [SQUASH_ATTEMPT.fullmatch(line) for line in before_merges]
        self.assertTrue(all(attempts), before_merges)
        self.assertEqual([attempt.groups() for attempt in attempts],
                         [("10", sha(10), BASE), ("20", sha(20), sha(310))])

    def test_agent_deadline_stops_between_squashes_after_reporting_the_prefix(self):
        self.git.batch([10, 20])
        merge = self.repo.merge
        now = 1

        def squash_after_deadline(number, head):
            nonlocal now
            result = merge(number, head)
            self.assertNotIn(f"Merged #{number}", self.output.getvalue())
            now = 3
            return result

        self.repo.merge = squash_after_deadline
        with patch.object(train.time, "monotonic", side_effect=lambda: now):
            with self.assertRaisesRegex(train.TickDeadline, "before squash of #20"):
                self.run_tick(agent_deadline=2)
        self.assertEqual(self.repo.merged, [(10, sha(10))])
        self.assertEqual(self.output.getvalue().count(f"Merged #10 at {sha(310)}"), 1)
        self.assertFalse(self.git.writes)

    def test_agent_deadline_stops_rebuild_after_final_squash(self):
        self.git.batch([10])
        merge = self.repo.merge
        now = 1

        def squash_before_expiry(number, head):
            nonlocal now
            result = merge(number, head)
            now = 3
            return result

        self.repo.merge = squash_before_expiry
        with patch.object(train.time, "monotonic", side_effect=lambda: now):
            with self.assertRaisesRegex(train.TickDeadline, "before rebuilding the batch after landing"):
                self.run_tick((10, 20), agent_deadline=2)
        self.assertEqual(self.repo.merged, [(10, sha(10))])
        self.assertIn(f"Merged #10 at {sha(310)}", self.output.getvalue())
        self.assertFalse(self.git.writes)

    def test_ambiguous_merge_errors_recover_the_landed_squash_and_continue(self):
        for error in (subprocess.TimeoutExpired("gh", 45),
                      subprocess.CalledProcessError(1, "gh", stderr="gh: unavailable (HTTP 503)"),
                      ValueError("invalid JSON response")):
            with self.subTest(error=error):
                self.setUp()
                self.git.batch([10, 20])
                merge = self.repo.merge

                def accepted_then_error(number, head):
                    landed = merge(number, head)
                    if number == 10:
                        self.git.history[BASE, landed] = [landed]
                        raise error
                    return landed

                self.repo.merge = accepted_then_error
                output = self.run_tick((10, 20))
                self.assertIn(f"Merged #10 at {sha(310)}", output)
                self.assertIn(f"Merged #20 at {sha(320)}", output)
                self.assertEqual(self.repo.merged, [(10, sha(10)), (20, sha(20))])

    def test_ambiguous_merge_stops_if_readback_has_a_different_pr_head(self):
        self.git.batch([10, 20])
        merge = self.repo.merge

        def accepted_different_head(number, head):
            landed = merge(number, head)
            self.repo.prs[number]["head"]["sha"] = sha(99)
            self.git.history[BASE, landed] = [landed]
            raise subprocess.TimeoutExpired("gh", 45)

        self.repo.merge = accepted_different_head
        with self.assertRaisesRegex(RuntimeError, "approved head.*Merged so far: #10"):
            self.run_tick((10, 20))
        self.assertEqual(self.repo.merged, [(10, sha(10))])
        self.assertFalse(self.git.writes)

    def test_ambiguous_merge_stops_when_github_squash_has_unexpected_parent(self):
        self.git.batch([10, 20])
        merge = self.repo.merge

        def accepted_on_changed_main(number, head):
            merge(number, head)
            raise subprocess.TimeoutExpired("gh", 45)

        self.repo.merge = accepted_on_changed_main
        with self.assertRaisesRegex(RuntimeError, "unexpected parent.*Merged so far: #10"):
            self.run_tick((10, 20))
        self.assertIn(f"Merged #10 at {sha(310)}", self.output.getvalue())
        self.assertEqual(self.repo.merged, [(10, sha(10))])
        self.assertFalse(self.git.writes)

    def test_base_modified_retries_with_backoff_and_rechecks_refs(self):
        self.git.batch([10])
        merge = self.repo.merge
        attempts = []

        def retryable_merge(number, head):
            attempts.append(number)
            if len(attempts) <= 3:
                raise subprocess.CalledProcessError(
                    1, "gh", stderr="gh: Base branch was modified. (HTTP 405)")
            return merge(number, head)

        self.repo.merge = retryable_merge
        read_refs = self.git.refs
        snapshots = []

        def refs():
            snapshot = read_refs()
            snapshots.append(snapshot)
            return snapshot

        self.git.refs = refs
        with patch("time.sleep") as sleep:
            self.run_tick((10,))
        self.assertEqual(attempts, [10] * 4)
        self.assertEqual(sleep.call_count, 3)
        self.assertEqual(snapshots.count((BASE, ACTIVE)), 5)

    def test_retries_stop_on_ref_move_or_after_three_retries(self):
        for move in (False, True):
            with self.subTest(move=move):
                self.setUp()
                self.git.batch([10])
                attempts = []

                def refuse(number, head):
                    attempts.append(number)
                    if move:
                        self.git.base = sha(101)
                    raise subprocess.CalledProcessError(
                        1, "gh", stderr="gh: Base branch was modified. (HTTP 405)")

                self.repo.merge = refuse
                with patch("time.sleep"), self.assertRaisesRegex(
                        RuntimeError, "main moved" if move else "Base branch was modified"):
                    self.run_tick((10,))
                self.assertEqual(attempts, [10] * (1 if move else 4))
                self.assertFalse(self.repo.merged or self.git.writes)

    def test_other_405_is_reported_once(self):
        self.git.batch([10])
        attempts = []

        def refuse(number, head):
            attempts.append(number)
            raise subprocess.CalledProcessError(1, "gh", stderr="gh: method refused (HTTP 405)")

        self.repo.merge = refuse
        with patch("time.sleep") as sleep, self.assertRaisesRegex(RuntimeError, "method refused"):
            self.run_tick((10,))
        self.assertEqual(attempts, [10])
        sleep.assert_not_called()

    def test_batch_branch_moves_before_or_during_squash(self):
        for during in (False, True):
            with self.subTest(during=during):
                self.setUp()
                self.git.batch([10])
                snapshots = iter([(BASE, ACTIVE), (BASE, ACTIVE if during else sha(201)),
                                  (sha(310), sha(201))])
                self.git.refs = lambda: next(snapshots, (sha(310), sha(201)))
                with self.assertRaisesRegex(RuntimeError, "batch branch moved during" if during
                                            else "batch branch moved before"):
                    self.run_tick((10,))
                self.assertEqual(self.repo.merged, [(10, sha(10))] if during else [])
                self.assertFalse(self.git.writes)

    def test_non_green_batch_with_matching_squash_is_a_bypass(self):
        for states in ({}, {"ui": ("cancelled", "https://run")}, {"ui": ("failure", "")}):
            with self.subTest(states=states), TemporaryDirectory() as tmp:
                self.setUp()
                self.git.batch([10, 20])
                landed = self.repo.merge(10, sha(10))
                self.repo.states[ACTIVE] = states
                self.git.history[BASE, landed] = [landed]
                self.git.history[BASE, ACTIVE] = [sha(490), ACTIVE]
                self.git.trees[sha(490)] = sha(901)
                latch = Path(tmp) / "mismatch"
                output = self.run_tick((10, 20), mismatch_file=latch)
                self.assertIn("#10 BYPASS", output)
                self.assertNotIn("Recovered landed prefix", output)
                self.assertFalse(latch.exists())
                self.written([20], expected=ACTIVE, base=landed)

    def test_main_advance_after_a_recovered_squash_still_checks_its_tree(self):
        self.git.batch([10, 20])
        first = self.repo.merge(10, sha(10))
        outside = sha(499)
        self.git.refs = lambda: (outside, ACTIVE)
        self.git.history[BASE, outside] = [first, outside]
        self.git.history[BASE, ACTIVE] = [sha(490), ACTIVE]
        self.git.trees[outside] = self.git.trees[sha(490)] = sha(901)
        self.git.trees[first] = sha(902)
        with TemporaryDirectory() as tmp:
            latch = Path(tmp) / "mismatch"
            with self.assertRaisesRegex(RuntimeError, "tested tree"):
                self.run_tick((10, 20), mismatch_file=latch)
            self.assertEqual(latch.read_text().strip(), ACTIVE)
            self.assertFalse(self.git.writes)

    def test_main_advance_retests_after_verified_prefix_and_reports_outside_member(self):
        self.git.batch([10, 20, 30])
        first = self.repo.merge(10, sha(10))
        second = self.repo.merge(20, sha(20))
        outside = sha(499)
        self.git.refs = lambda: (second, ACTIVE)
        self.git.history[BASE, second] = [first, outside, second]
        self.git.history[BASE, ACTIVE] = [sha(490), sha(491), ACTIVE]
        self.git.trees[first] = self.git.trees[sha(490)] = sha(901)
        self.git.trees[second] = sha(902)
        output = self.run_tick((10, 20, 30))
        self.assertIn(f"Merged #10 at {first}", output)
        self.assertIn("main moved", output)
        self.assertIn("#20 BYPASS", output)
        self.assertNotIn("#10 BYPASS", output)
        self.written([30], expected=ACTIVE, base=second)

    def test_foreign_commit_cannot_recover_a_reported_member_squash(self):
        self.git.batch([10, 20])
        self.repo.merge(10, sha(10))
        outside = sha(499)
        self.git.refs = lambda: (outside, ACTIVE)
        self.git.history[BASE, outside] = [outside]
        self.git.history[BASE, ACTIVE] = [sha(490), ACTIVE]
        self.git.trees[sha(490)] = self.git.landed_tree
        output = self.run_tick((10, 20))
        self.assertIn("#10 BYPASS", output)
        self.assertNotIn("Recovered landed prefix", output)
        self.written([20], expected=ACTIVE, base=outside)

    def test_member_squashed_at_a_different_head_is_a_bypass(self):
        self.git.batch([10, 20])
        self.repo.prs[10]["head"]["sha"] = sha(99)
        landed = self.repo.merge(10, sha(99))
        self.git.history[BASE, landed] = [landed]
        self.git.history[BASE, ACTIVE] = [sha(490), ACTIVE]
        self.git.trees[sha(490)] = self.git.landed_tree
        output = self.run_tick((10, 20))
        self.assertIn("#10 BYPASS", output)
        self.assertNotIn("Recovered landed prefix", output)
        self.written([20], expected=ACTIVE, base=landed)

    def test_recovery_rejects_a_tested_chain_shorter_than_its_trailers(self):
        self.git.batch([10, 20])
        landed = self.repo.merge(10, sha(10))
        self.git.history[BASE, landed] = [landed]
        self.git.history[BASE, ACTIVE] = [ACTIVE]
        with self.assertRaisesRegex(RuntimeError, "trailers"):
            self.run_tick((10, 20))
        self.assertFalse(self.git.writes)

    def test_next_tick_recovers_own_squash_prefix_and_retests_the_suffix(self):
        self.git.batch([10, 20], [30])
        landed = self.repo.merge(10, sha(10))
        self.git.history[BASE, landed] = [landed]
        self.git.history[BASE, ACTIVE] = [sha(490), ACTIVE]
        self.git.trees[sha(490)] = self.git.landed_tree
        output = self.run_tick((10, 20, 30))
        self.assertIn(f"Tested batch {ACTIVE}", output.splitlines())
        self.assertIn(f"Merged #10 at {landed}", output)
        self.assertIn("prefix", output)
        self.assertNotIn("BYPASS", output)
        self.written([20], [30], expected=ACTIVE, base=landed)

    def test_visible_prefix_is_verified_while_a_reported_squash_propagates(self):
        self.git.batch([10, 20])
        first = self.repo.merge(10, sha(10))
        second = self.repo.merge(20, sha(20))
        visible_main = first
        self.git.refs = lambda: (visible_main, ACTIVE)
        self.git.history[BASE, first] = [first]
        self.git.history[BASE, second] = [first, second]
        self.git.history[BASE, ACTIVE] = [sha(490), ACTIVE]
        self.git.trees[sha(490)] = self.git.landed_tree

        output = self.run_tick((10, 20))
        self.assertIn(f"Merged #10 at {first}", output)
        self.assertIn("prefix", output)
        self.assertNotIn("BYPASS", output)
        self.assertFalse(self.git.writes)

        visible_main = second
        self.assertIn(f"Merged #20 at {second}", self.run_tick((10, 20)))
        self.assertEqual(self.git.writes, [("", ACTIVE)])

    def test_later_tick_latches_mismatched_squash_prefix(self):
        self.git.batch([10, 20])
        landed = self.repo.merge(10, sha(10))
        self.git.history[BASE, landed] = [landed]
        self.git.history[BASE, ACTIVE] = [sha(490), ACTIVE]
        self.git.trees[sha(490)] = sha(901)
        with TemporaryDirectory() as tmp:
            latch = Path(tmp) / "mismatch"
            for ack in (None, sha(201)):
                with self.assertRaisesRegex(RuntimeError, "mismatch|tested tree"):
                    self.run_tick((10, 20), mismatch_file=latch, ack_mismatch=ack)
                self.assertEqual(latch.read_text().strip(), ACTIVE)
                self.assertFalse(self.git.writes)
            self.assertIn("Acknowledged", self.run_tick((10, 20), mismatch_file=latch,
                                                        ack_mismatch=ACTIVE))
            self.assertFalse(latch.exists())
            self.written([20], expected=ACTIVE, base=landed)

    def test_acknowledgement_for_an_unrelated_batch_stops_before_writes(self):
        self.git.batch([10])
        unrelated = sha(201)
        with self.assertRaisesRegex(RuntimeError, "must name the blocked or active batch SHA"):
            self.run_tick((10, 20), ack_mismatch=unrelated)
        self.assertFalse(self.git.writes or self.repo.merged)

    def test_acknowledgement_targets_the_latched_batch_not_the_current_batch(self):
        self.git.batch([10, 20])
        landed = self.repo.merge(10, sha(10))
        self.git.history[BASE, landed] = [landed]
        self.git.history[BASE, ACTIVE] = [sha(490), ACTIVE]
        self.git.trees[sha(490)] = sha(901)
        older_batch = sha(201)
        with TemporaryDirectory() as tmp:
            latch = Path(tmp) / "mismatch"
            latch.write_text(f"{older_batch}\n")
            with self.assertRaisesRegex(RuntimeError, older_batch):
                self.run_tick((10, 20), mismatch_file=latch, ack_mismatch=ACTIVE)
            self.assertEqual(latch.read_text().strip(), older_batch)
            with self.assertRaisesRegex(RuntimeError, "tested tree"):
                self.run_tick((10, 20), mismatch_file=latch, ack_mismatch=older_batch)
            self.assertEqual(latch.read_text().strip(), ACTIVE)
            self.assertFalse(self.git.writes)

    def test_malformed_merge_response_reports_the_committed_prefix(self):
        self.git.batch([20, 10])
        self.repo.merge_error = 10
        self.repo.merge_exception = ValueError("invalid JSON response")
        with self.assertRaisesRegex(RuntimeError, "Merged so far: #20"):
            self.run_tick()
        self.assertEqual(self.repo.merged, [(20, sha(20))])
        self.assertFalse(self.git.writes)

    def test_post_squash_stop_confirms_landed_members_with_github(self):
        self.git.batch([10])
        reads_after_tree_check = []
        pull = self.repo.pull
        tree = self.git.tree

        def record_verification(head):
            self.repo.pull = lambda number: (reads_after_tree_check.append(number), pull(number))[1]
            return tree(head)

        self.git.tree = record_verification
        self.git.landed_tree = sha(901)
        with self.assertRaisesRegex(RuntimeError, "Merged so far: #10"):
            self.run_tick((10,))
        self.assertEqual(reads_after_tree_check, [10])

    def test_tree_mismatch_stops_the_tick(self):
        self.git.batch([10])
        self.git.landed_tree = sha(901)
        with TemporaryDirectory() as tmp:
            latch = Path(tmp) / "mismatch"
            with self.assertRaisesRegex(RuntimeError, f"main tree {sha(901)} != tested tree {sha(900)}"):
                self.run_tick(mismatch_file=latch)
            self.assertIn(f"Merged #10 at {sha(310)}", self.output.getvalue())
            self.assertEqual(latch.read_text().strip(), ACTIVE)
            self.git.active = ""
            with self.assertRaisesRegex(RuntimeError, "mismatch"):
                self.run_tick(mismatch_file=latch)
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
    def test_make_treats_a_busy_lock_as_success_and_keeps_real_stops_fatal(self):
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            scripts = root / "scripts"
            scripts.mkdir()
            shutil.copyfile(train.__file__, scripts / "merge_train.py")
            gh = root / "gh"
            gh.write_text("#!/bin/sh\ntouch gh-called\nexit 99\n")
            gh.chmod(0o755)
            env = {**os.environ, "HOME": str(root), "PATH": f"{root}{os.pathsep}{os.environ['PATH']}"}
            makefile = Path(__file__).resolve().parents[1] / "Makefile"
            make = ["make", "-f", str(makefile), "merge-train", "BATCH=4", "ACK_MISMATCH="]

            with (root / ".git" / "merge-train.lock").open("a") as held:
                train.fcntl.flock(held, train.fcntl.LOCK_EX | train.fcntl.LOCK_NB)
                busy = subprocess.run([*make, f"ORDER=10={sha(10)}"], cwd=root, env=env,
                                      capture_output=True, text=True)
                stopped = subprocess.run([*make, "ORDER="], cwd=root, env=env,
                                         capture_output=True, text=True)

            self.assertIn("another tick holds the lock", busy.stdout)
            self.assertEqual(busy.returncode, 0, busy.stderr)
            self.assertEqual(stopped.returncode, 2)
            self.assertIn("ORDER requires", stopped.stderr)
            self.assertFalse((root / "gh-called").exists())

    def test_manual_train_refuses_registered_agent_order_before_creating_clients(self):
        with TemporaryDirectory() as tmp:
            home = Path(tmp)
            plist = home / "Library/LaunchAgents/io.github.yuriimurha.grooviq.merge-train.plist"
            plist.parent.mkdir(parents=True)
            plist.touch()
            disabled = home / "Library/Application Support/Grooviq Ops/merge-train/disabled"
            disabled.parent.mkdir(parents=True)
            with (patch.object(train.Path, "home", return_value=home),
                  patch.object(sys, "argv", ["merge_train.py", "--order", f"10={sha(10)}"]),
                  patch.object(train.subprocess, "check_output", return_value=tmp),
                  patch.object(train, "tick") as tick,
                  redirect_stderr(io.StringIO()) as stderr):
                self.assertEqual(train.main(), 1)
                self.assertIn("make merge-train-order", stderr.getvalue())
                self.assertIn("make merge-train-agent-disable", stderr.getvalue())
                self.assertIn("make merge-train ORDER=", stderr.getvalue())
                self.assertRegex(stderr.getvalue(), "make merge-train-agent-enable.*make merge-train-agent-resume")
                tick.assert_not_called()
                disabled.touch()
                self.assertEqual(train.main(), 0)
                tick.assert_called_once()

    def test_unattended_cli_holds_failed_pr_and_disables_all_git_hooks(self):
        with TemporaryDirectory() as tmp:
            fixture = Repo()
            pr = fixture.prs[10]
            git_calls, api_calls = [], []

            def git(argv, **kwargs):
                git_calls.append(argv)
                if "--git-common-dir" in argv:
                    stdout = tmp
                elif "ls-remote" in argv:
                    stdout = f"{BASE} {train.MAIN_REF}\n"
                elif "fetch" in argv:
                    stdout = ""
                else:
                    raise AssertionError(argv)
                return subprocess.CompletedProcess(argv, 0, stdout, "")

            def api(_self, path, *, method="GET", **kwargs):
                api_calls.append(method)
                if method != "GET":
                    return None
                if path == "branches/main/protection/required_status_checks":
                    return {"strict": False, "checks": [
                        {"context": name, "app_id": ACTIONS_APP} for name in REQUIRED
                    ]}
                if path.startswith("issues?"):
                    return [[{"number": 10, "pull_request": {}}]]
                if path == "pulls/10":
                    return pr if "DELETE" not in api_calls else {**pr, "labels": []}
                if path.startswith("commits/"):
                    head = path.split("/")[1]
                    return [{"check_runs": [
                        {"name": name, "head_sha": head, "app": {"id": ACTIONS_APP},
                         "status": "completed", "conclusion": "failure"
                         if name == "ui" and head == sha(10) else "success"}
                        for name in REQUIRED
                    ]}]
                raise AssertionError(path)

            argv = ["merge_train.py", "--order", f"10={sha(10)}",
                    "--agent-deadline", str(time.monotonic() + 60)]
            with (patch.object(sys, "argv", argv), patch.object(train.subprocess, "run", side_effect=git),
                  patch.object(train.GitHub, "api", api),
                  redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()) as stderr):
                code = train.main()
            self.assertIn("Held #10", stderr.getvalue())
            self.assertIn("make merge-train ORDER=", stderr.getvalue())
            self.assertRegex(stderr.getvalue(), "make merge-train-order.*make merge-train-agent-resume")
            self.assertEqual(code, 1)
            self.assertTrue(api_calls)
            self.assertEqual(set(api_calls), {"GET"})
            self.assertGreaterEqual(len(git_calls), 3)
            for argv in git_calls:
                self.assertEqual(argv[:3], ["git", "-c", "core.hooksPath=/dev/null"])

    def test_busy_flock_returns_tempfail_before_creating_clients(self):
        with TemporaryDirectory() as tmp:
            with (Path(tmp) / "merge-train.lock").open("a") as held:
                train.fcntl.flock(held, train.fcntl.LOCK_EX | train.fcntl.LOCK_NB)
                with (patch.object(sys, "argv", ["merge_train.py", "--order", f"10={sha(10)}"]),
                      patch.object(train.subprocess, "check_output", return_value=tmp) as discover,
                      patch.object(train.subprocess, "run") as command,
                      patch.object(train, "GitHub") as github,
                      patch.object(train, "Git") as git,
                      redirect_stdout(io.StringIO()) as stdout,
                      redirect_stderr(io.StringIO()) as stderr):
                    self.assertEqual(train.main(), 75)
                discover.assert_called_once_with(
                    ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"], text=True,
                )
                command.assert_not_called()
                github.assert_not_called()
                git.assert_not_called()
                self.assertEqual(stdout.getvalue(), "Merge train busy: another tick holds the lock.\n")
                self.assertEqual(stderr.getvalue(), "")

    def test_blocking_io_error_after_acquiring_lock_is_a_failure(self):
        with (TemporaryDirectory() as tmp,
              patch.object(sys, "argv", ["merge_train.py", "--order", f"10={sha(10)}"]),
              patch.object(train.subprocess, "check_output", return_value=tmp),
              patch.object(train.Path, "home", return_value=Path(tmp)),
              patch.object(train, "tick", side_effect=BlockingIOError("tick failed")),
              redirect_stderr(io.StringIO()) as stderr):
            self.assertEqual(train.main(), 1)
        self.assertIn("Merge train stopped: tick failed", stderr.getvalue())

    def test_invalid_cli_arguments_exit_one(self):
        script = Path(__file__).with_name("merge_train.py")
        result = subprocess.run([sys.executable, "-B", str(script), "--batch", "many"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("--batch", result.stderr)


    def test_empty_order_exits_before_reading_a_checkout(self):
        with (patch.object(sys, "argv", ["merge_train.py"]),
              patch.object(train.subprocess, "check_output") as git,
              redirect_stderr(io.StringIO()) as stderr):
            self.assertEqual(train.main(), 1)
        git.assert_not_called()
        self.assertIn("ORDER", stderr.getvalue())


class GitHubTests(unittest.TestCase):
    def test_api_exposes_http_error_text_to_retry_logic(self):
        def gh(argv, **kwargs):
            self.assertTrue(kwargs["capture_output"])
            raise subprocess.CalledProcessError(
                1, argv, stderr="gh: Base branch was modified. (HTTP 405)")

        with patch.object(train.subprocess, "run", side_effect=gh):
            with self.assertRaisesRegex(subprocess.CalledProcessError, "non-zero") as caught:
                GitHub().merge(10, sha(10))
        self.assertIn("Base branch was modified", caught.exception.stderr)

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

    def test_selector_uses_the_same_latest_check_response_and_verified_app_and_head(self):
        repo = GitHub()
        head = sha(10)
        rust = {"name": REQUIRED[0], "head_sha": head, "status": "completed",
                "conclusion": "skipped", "app": {"id": ACTIONS_APP}}
        selector = {**rust, "name": "changes", "conclusion": "success"}
        pages = [{"check_runs": [rust, {**selector, "head_sha": sha(20)},
                                  {**selector, "app": {"id": 9}}, selector]}]
        with patch.object(repo, "api", return_value=pages) as api:
            self.assertEqual(repo.checks(head), {REQUIRED[0]: ("skipped", ""),
                                                "changes": ("success", "")})
        api.assert_called_once_with(f"commits/{head}/check-runs?filter=latest&per_page=100",
                                    paginate=True)

    def test_ambiguous_optional_selector_never_authorizes_skips(self):
        repo = GitHub()
        head = sha(10)
        selector = {"name": "changes", "head_sha": head, "status": "completed",
                    "conclusion": "success", "app": {"id": ACTIONS_APP}}
        repo.api = lambda *args, **kwargs: [{"check_runs": [
            {**selector, "conclusion": "cancelled"}, selector,
        ]}]
        self.assertEqual(repo.checks(head), {"changes": (None, "")})

    def test_duplicate_or_incomplete_required_check_is_ambiguous(self):
        repo = GitHub()
        check = {"name": "ui", "head_sha": sha(10), "status": "completed",
                 "conclusion": "success", "app": {"id": ACTIONS_APP}}
        for checks in ([check, check], [{**check, "conclusion": None}]):
            with self.subTest(checks=checks):
                repo.api = lambda *args, **kwargs: [{"check_runs": checks}]
                with self.assertRaises(RuntimeError):
                    repo.checks(sha(10))

    def test_remote_write_attempts_are_visible_before_ambiguous_failures(self):
        for layer, operation, marker in (
            (train.Git(), lambda layer: layer.publish(sha(10), ""), "Git push"),
            (GitHub(), lambda layer: layer.api("issues/10/comments", method="POST"), "GitHub POST"),
        ):
            with self.subTest(marker=marker), patch.object(train.subprocess, "run") as run, \
                    patch("sys.stderr", new_callable=io.StringIO) as errors:
                def fail(*args, **kwargs):
                    self.assertIn(marker, errors.getvalue())
                    raise subprocess.CalledProcessError(1, args[0], stderr="HTTP 502")
                run.side_effect = fail
                with self.assertRaises(subprocess.CalledProcessError):
                    operation(layer)

    def test_unattended_rejection_holds_before_any_public_write(self):
        repo = GitHub(hold_rejections=True)
        with patch.object(repo, "api") as api:
            with self.assertRaisesRegex(RuntimeError, "Held #10.*failed check"):
                repo.reject(10, sha(10), "failed check")
            api.assert_not_called()

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


class GitTests(unittest.TestCase):
    def test_publish_does_not_push_after_deadline(self):
        git = train.Git(agent_deadline=2)
        with (patch.object(train.time, "monotonic", return_value=2),
              patch.object(train.subprocess, "run") as run,
              redirect_stderr(io.StringIO()) as stderr):
            with self.assertRaisesRegex(train.TickDeadline, "before publishing the batch"):
                git.publish(sha(10), ACTIVE)
        run.assert_not_called()
        self.assertEqual(stderr.getvalue(), "")


class GitIntegrationTest(unittest.TestCase):
    def test_trailing_conflict_keeps_the_last_accepted_tree(self):
        for conflict_with_main in (False, True):
            with self.subTest(conflict_with_main=conflict_with_main), TemporaryDirectory() as root:
                root = Path(root)
                remote = root / "remote.git"

                def git(*args, cwd=root):
                    return subprocess.check_output(["git", *args], cwd=cwd, text=True,
                                                   stderr=subprocess.PIPE).strip()

                git("init", "-b", "main", ".")
                git("init", "--bare", str(remote))
                git("config", "user.name", "Test")
                git("config", "user.email", "train@example.invalid")
                git("remote", "add", "origin", str(remote))

                def commit(file, content):
                    (root / file).write_text(content)
                    git("add", file)
                    git("commit", "-m", content)
                    return git("rev-parse", "HEAD")

                base = commit("shared", "original")
                if conflict_with_main:
                    commit("shared", "main change")
                main = git("rev-parse", "HEAD")
                git("push", "origin", "main")
                head10 = commit("independent" if conflict_with_main else "shared", "first change")
                git("push", "origin", "HEAD:refs/pull/10/head")
                git("switch", "--detach", base)
                head20 = commit("shared", "second change")
                git("push", "origin", "HEAD:refs/pull/20/head")

                order = {10: head10, 20: head20}
                repo = Repo()
                for n, head in order.items():
                    repo.prs[n]["head"]["sha"] = head
                layer = train.Git(root)
                with redirect_stdout(io.StringIO()):
                    train.build_batch(repo, layer, main, "", order, 4)
                _, active = layer.refs()
                self.assertTrue(active)
                self.assertEqual(layer.tree(active), layer.tree(head10))
                self.assertEqual(train.Batch.parse(layer.message(active)).members, [(10, head10)])
                self.assertEqual([item[0] for item in repo.rejected], [20] if conflict_with_main else [])

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
            first = git_layer.commit(git_layer.merge_tree(base, heads[0]), base, "Train squash of #10")
            active = git_layer.commit(git_layer.merge_tree(first, heads[1]), first, batch.message())
            self.assertEqual(train.Batch.parse(git_layer.message(active)), batch)
            self.assertEqual(git_layer.first_parent_chain(base, active), [first, active])
            self.assertEqual(git_layer.first_parent_chain(heads[0], active), [])
            git_layer.verify_chain(active, batch)
            short = git_layer.commit(git_layer.tree(first), base, batch.message())
            with self.assertRaisesRegex(RuntimeError, "trailers"):
                git_layer.verify_chain(short, batch)
            two_parent = git("commit-tree", git_layer.tree(first), "-p", base, "-p", heads[0],
                             "-m", "forged parents", cwd=observer)
            with self.assertRaisesRegex(RuntimeError, "trailers"):
                git_layer.verify_chain(two_parent, train.Batch(base, [(10, heads[0])], []))
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
            with self.assertRaisesRegex(RuntimeError, "fetched head moved"):
                git_layer.verify_chain(active, batch)


if __name__ == "__main__":
    unittest.main()
