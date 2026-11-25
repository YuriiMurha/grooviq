import unittest

from merge_train import ACTIONS_APP, GitHub, REQUIRED, tick


class Repo:
    def __init__(self):
        self.sha = "a" * 40
        self.behind = 0
        self.status = {name: "success" for name in REQUIRED}
        self.actions = []
        self.closed = set()
        self.armed = False

    def enrolled(self):
        return [10, 20, 30]

    def pull(self, number):
        self.actions.append(("read", number))
        return {
            "state": "closed" if number in self.closed else "open",
            "html_url": f"https://github.com/YuriiMurha/grooviq/pull/{number}",
            "draft": False, "auto_merge": {} if self.armed else None,
            "head": {"sha": self.sha, "repo": {"full_name": "YuriiMurha/grooviq"}},
            "base": {"ref": "main"},
        }

    def checks(self, sha):
        self.actions.append(("checks", sha))
        return self.status

    def behind_main(self, sha):
        return self.behind

    def reject(self, number, sha, reason):
        self.actions.append(("remove", number, sha, reason))
        return True

    def refresh(self, number, sha):
        self.actions.append(("update", number, sha))

    def merge(self, number, sha):
        self.actions.append(("merge", number, sha))


class MergeTrainTests(unittest.TestCase):
    def setUp(self):
        self.repo = Repo()

    def test_root_order_beats_number_and_checks_pin_merge(self):
        self.assertIn("Merged #20", tick(self.repo, [20, 10], self.repo.sha))
        self.assertEqual(self.repo.actions, [
            ("read", 20), ("checks", self.repo.sha), ("merge", 20, self.repo.sha),
        ])

    def test_green_head_behind_main_updates_only_front(self):
        self.repo.behind = 2
        self.assertIn("Updated #20", tick(self.repo, [20], self.repo.sha))
        self.assertEqual(self.repo.actions[-1], ("update", 20, self.repo.sha))
        self.assertEqual([a for a in self.repo.actions if a[0] == "read"], [("read", 20)])

    def test_unknown_head_cannot_reuse_approval(self):
        self.assertIn("approval", tick(self.repo, [20], "b" * 40))
        self.assertEqual(self.repo.actions, [("read", 20)])

    def test_pending_or_missing_check_holds_later_prs(self):
        del self.repo.status["ui"]
        self.assertIn("Waiting", tick(self.repo, [20, 10], self.repo.sha))
        self.assertEqual(self.repo.actions, [("read", 20), ("checks", self.repo.sha)])

    def test_failure_removes_front_with_reason(self):
        self.repo.status["ui"] = "failure"
        self.assertIn("Removed #20", tick(self.repo, [20, 10], self.repo.sha))
        self.assertEqual(self.repo.actions[-1], ("remove", 20, self.repo.sha, "ui (failure)"))

    def test_auto_merge_cannot_take_train_slot(self):
        self.repo.armed = True
        self.assertIn("auto-merge", tick(self.repo, [20], self.repo.sha))
        self.assertEqual(self.repo.actions, [("read", 20)])

    def test_closed_front_does_not_take_train_slot(self):
        self.repo.closed.add(20)
        self.assertIn("Merged #10", tick(self.repo, [20, 10], self.repo.sha))
        self.assertEqual(self.repo.actions[-1], ("merge", 10, self.repo.sha))

    def test_completed_skip_is_not_green(self):
        self.repo.status["ui"] = "skipped"
        self.assertIn("Removed #20", tick(self.repo, [20], self.repo.sha))
        self.assertEqual(self.repo.actions[-1], ("remove", 20, self.repo.sha, "ui (skipped)"))


class GitHubTests(unittest.TestCase):
    def test_changed_head_is_not_removed_for_a_prior_failure(self):
        repo = GitHub()
        mutations = []
        repo.api = lambda *args, **kwargs: mutations.append((args, kwargs))
        repo.pull = lambda n: {"state": "open", "head": {"sha": "b" * 40}}
        self.assertFalse(repo.reject(20, "a" * 40, "ui (failure)"))
        self.assertEqual(mutations, [])

    def test_failure_comment_identifies_failed_head(self):
        repo = GitHub()
        sha = "a" * 40
        mutations = []
        repo.api = lambda *args, **kwargs: mutations.append((args, kwargs))
        repo.pull = lambda n: {"state": "open", "head": {"sha": sha}}
        self.assertTrue(repo.reject(20, sha, "ui (failure)"))
        self.assertIn(sha, mutations[0][1]["body"]["body"])
        self.assertEqual(mutations[1], (("issues/20/labels/merge-train",), {"method": "DELETE"}))

    def test_train_holds_a_head_pushed_during_failure_comment(self):
        repo = GitHub()
        sha = "a" * 40
        mutations = []
        heads = iter([sha, "b" * 40])
        repo.api = lambda *args, **kwargs: mutations.append((args, kwargs))
        repo.pull = lambda n: {"state": "open", "head": {"sha": next(heads)}}
        self.assertFalse(repo.reject(20, sha, "ui (failure)"))
        self.assertEqual([kwargs["method"] for _, kwargs in mutations], ["POST"])

    def test_refresh_and_squash_send_the_observed_head(self):
        repo = GitHub()
        sha = "a" * 40
        mutations = []

        def api(path, **kwargs):
            mutations.append((path, kwargs))
            return {"merged": True}

        repo.api = api
        repo.refresh(20, sha)
        repo.merge(20, sha)
        self.assertEqual(mutations, [
            ("pulls/20/update-branch", {"method": "PUT", "body": {"expected_head_sha": sha}}),
            ("pulls/20/merge", {"method": "PUT", "body": {"sha": sha, "merge_method": "squash"}}),
        ])

    def test_changed_protection_stops_before_inspecting_checks(self):
        contexts = [{"context": name, "app_id": ACTIONS_APP} for name in REQUIRED]
        for policy in ({"strict": False, "checks": contexts}, {"strict": True, "checks": contexts[:-1]}):
            with self.subTest(policy=policy):
                repo = GitHub()
                repo.api = lambda path, **kwargs: (
                    policy if path == "branches/main/protection/required_status_checks" else []
                )
                with self.assertRaisesRegex(RuntimeError, "policy changed"):
                    repo.checks("a" * 40)

    def test_check_app_and_commit_are_both_verified(self):
        sha = "a" * 40
        check = {"name": "ui", "head_sha": sha, "status": "completed",
                 "conclusion": "success", "app": {"id": ACTIONS_APP}}
        repo = GitHub()
        responses = iter([
            {"strict": True, "checks": [
                {"context": name, "app_id": ACTIONS_APP} for name in REQUIRED
            ]},
            [{"check_runs": [
                {**check, "head_sha": "b" * 40},
                {**check, "app": {"id": 9}},
            ]}],
        ])
        repo.api = lambda *args, **kwargs: next(responses)
        self.assertEqual(repo.checks(sha), {})

    def test_unsuccessful_squash_is_not_reported_as_merged(self):
        repo = GitHub()
        repo.api = lambda *args, **kwargs: {"merged": False, "message": "Main advanced"}
        with self.assertRaisesRegex(RuntimeError, "Main advanced"):
            repo.merge(20, "a" * 40)


if __name__ == "__main__":
    unittest.main()
