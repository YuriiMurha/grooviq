import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("reclaim_caches.py")


class RuntimeTests(unittest.TestCase):
    def test_old_python_refuses_both_commands_before_probing_disk(self):
        import reclaim_caches

        for command in ("reclaim-caches", "disk-check"):
            with self.subTest(command=command):
                error = io.StringIO()
                with (
                    patch.object(sys, "version_info", (3, 10)),
                    patch.object(sys, "argv", [str(SCRIPT), command]),
                    patch.object(shutil, "disk_usage", side_effect=AssertionError("disk probe ran")),
                    redirect_stderr(error),
                ):
                    self.assertEqual(reclaim_caches.main(), 1)
                self.assertIn("requires Python 3.11", error.getvalue())


class DiskCheckTests(unittest.TestCase):
    def test_threshold_controls_exit_status(self):
        with tempfile.TemporaryDirectory() as repo:
            for minimum, status in (("0", 0), ("1e12", 1)):
                with self.subTest(minimum=minimum):
                    result = subprocess.run(
                        [sys.executable, "-B", str(SCRIPT), "disk-check", "--min-gib", minimum],
                        cwd=repo,
                        capture_output=True,
                        text=True,
                    )
                    self.assertEqual(result.returncode, status, result.stderr)
                    self.assertIn("GiB free", result.stdout)
                    self.assertEqual("Warning" in result.stdout, status == 1)


class ReclaimTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory())).resolve()
        self.main = self.root / "main"
        self.remote = self.root / "remote.git"
        self.release = self.root / "quality-release"
        self.prs = {}
        self.git(self.root, "init", "--bare", str(self.remote))
        self.git(self.root, "init", "-b", "main", str(self.main))
        self.git(self.main, "config", "user.name", "Cache Test")
        self.git(self.main, "config", "user.email", "cache-test@example.invalid")
        self.git(self.main, "config", "commit.gpgsign", "false")
        self.git(self.main, "config", "core.hooksPath", str(self.root / "no-hooks"))
        (self.main / ".gitignore").write_text("/target/\n/node_modules/\n")
        (self.main / "tracked").write_text("source")
        self.git(self.main, "add", ".gitignore", "tracked")
        self.git(self.main, "commit", "-m", "Initial")
        self.git(self.main, "remote", "add", "origin", str(self.remote))
        self.git(self.main, "push", "origin", "main")

    def git(self, cwd, *args):
        return subprocess.run(
            ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
        ).stdout.strip()

    def worktree(self, name, *, detached=True):
        path = self.root / name
        options = ["--detach"] if detached else ["-b", name]
        self.git(self.main, "worktree", "add", *options, str(path), "main")
        return self.make_cache(path)

    def make_cache(self, worktree):
        target = worktree / "target"
        target.mkdir()
        (target / "artifact").write_text("binary")
        self.age(target / "artifact", target)
        return target

    def age(self, *paths):
        for path in paths:
            old = time.time() - 3600
            os.utime(path, (old, old))

    def merge_branch(self, target):
        branch = target.parent.name
        self.git(target.parent, "commit", "--allow-empty", "-m", "First commit title")
        head = self.git(target.parent, "rev-parse", "HEAD")
        self.git(target.parent, "push", "origin", branch)
        self.git(self.main, "commit", "--allow-empty", "-m", f"Squash title for {branch}")
        squash = self.git(self.main, "rev-parse", "HEAD")
        self.git(self.main, "push", "origin", "main")
        self.git(self.remote, "update-ref", "-d", f"refs/heads/{branch}")
        self.prs[branch] = [{
            "title": f"Squash title for {branch}",
            "headRefOid": head,
            "isCrossRepository": False,
            "mergeCommit": {"oid": squash},
        }]
        return self.prs[branch][0]

    def run_command(self, args, **kwargs):
        if args[0] == "gh":
            branch = args[args.index("--head") + 1]
            return subprocess.CompletedProcess(args, 0, json.dumps(self.prs.get(branch, [])), "")
        if args[0] == "ps":
            return subprocess.CompletedProcess(args, 0, "/usr/libexec/logd\n", "")
        if args[0] == "lsof":
            return subprocess.CompletedProcess(args, 1, "", "")
        return subprocess.run(args, capture_output=True, text=True, **kwargs)

    def reclaim(self, *, apply=True, runner=None, recent_minutes=45):
        from reclaim_caches import reclaim_caches

        output = io.StringIO()
        with redirect_stdout(output):
            status = reclaim_caches(
                self.main,
                apply=apply,
                recent_minutes=recent_minutes,
                release_cache=self.release,
                run=runner or self.run_command,
            )
        self.assertEqual(status, 0, output.getvalue())
        return output.getvalue()

    def test_requires_deleted_remote_branch_and_squash_on_main(self):
        merged = self.worktree("landed", detached=False)
        self.merge_branch(merged)
        open_branch = self.worktree("open", detached=False)
        self.merge_branch(open_branch)
        self.git(open_branch.parent, "push", "origin", "open")
        absent = self.worktree("abandoned", detached=False)
        off_main = self.worktree("off-main", detached=False)
        evidence = self.merge_branch(off_main)
        evidence["mergeCommit"]["oid"] = evidence["headRefOid"]

        self.reclaim()
        self.assertFalse(merged.exists())
        for target in (open_branch, absent, off_main):
            self.assertTrue(target.is_dir(), target)

    def test_local_commits_after_squash_keep_the_cache(self):
        target = self.worktree("reused", detached=False)
        self.merge_branch(target)
        self.git(target.parent, "commit", "--allow-empty", "-m", "New local work")
        self.reclaim()
        self.assertTrue(target.is_dir())

    def test_dirty_detached_worktrees_keep_their_cache(self):
        clean = self.worktree("clean checkout")
        dirty = self.worktree("dirty")
        (dirty.parent / "tracked").write_text("edited")
        self.reclaim()
        self.assertFalse(clean.exists())
        self.assertTrue(dirty.is_dir())

    def test_main_checkout_keeps_its_cache_even_if_detached_and_clean(self):
        self.git(self.main, "switch", "--detach")
        target = self.make_cache(self.main)
        self.reclaim()
        self.assertTrue(target.is_dir())

    def test_release_cache_and_node_modules_are_preserved(self):
        shared = self.worktree("quality-release")
        candidate = self.worktree("candidate")
        nested_modules = candidate / "deep" / "in" / "the" / "tree" / "node_modules"
        nested_modules.mkdir(parents=True)
        nested_file = nested_modules / "dependency"
        nested_file.write_text("nested")
        self.age(nested_file, nested_modules, *nested_modules.parents[:5])
        outside_modules = shared.parent / "node_modules"
        outside_modules.mkdir()
        outside_file = outside_modules / "dependency"
        outside_file.write_text("outside")
        self.reclaim()
        self.assertTrue((shared / "artifact").is_file())
        self.assertEqual(nested_file.read_text(), "nested")
        self.assertEqual(outside_file.read_text(), "outside")

    def test_target_symlink_does_not_reach_another_directory(self):
        target = self.worktree("candidate")
        (target / "artifact").unlink()
        target.rmdir()
        outside = self.root / "elsewhere"
        outside.mkdir()
        protected = outside / "keep"
        protected.write_text("data")
        target.symlink_to(outside, target_is_directory=True)
        self.reclaim()
        self.assertTrue(target.is_symlink())
        self.assertEqual(protected.read_text(), "data")

    def test_recent_write_skips_cache(self):
        target = self.worktree("candidate")
        (target / "artifact").touch()
        self.reclaim()
        self.assertTrue(target.is_dir())

    def test_recency_window_can_be_configured(self):
        target = self.worktree("candidate")
        self.reclaim(recent_minutes=90)
        self.assertTrue(target.is_dir())
        self.reclaim(recent_minutes=45)
        self.assertFalse(target.exists())

    def test_open_handle_skips_cache(self):
        target = self.worktree("candidate")

        def runner(args, **kwargs):
            if args[0] == "lsof":
                return subprocess.CompletedProcess(args, 0, "123\n", "")
            return self.run_command(args, **kwargs)

        self.reclaim(runner=runner)
        self.assertTrue(target.is_dir())

    def test_lsof_failure_or_timeout_skips_cache(self):
        target = self.worktree("candidate")
        failures = (
            subprocess.CompletedProcess([], 1, "", "lsof: permission denied"),
            subprocess.CompletedProcess([], 2, "", ""),
            subprocess.TimeoutExpired("lsof", 1),
            FileNotFoundError("lsof"),
        )
        for failure in failures:
            with self.subTest(failure=failure):
                def runner(args, **kwargs):
                    if args[0] == "lsof":
                        if isinstance(failure, Exception):
                            raise failure
                        return failure
                    return self.run_command(args, **kwargs)

                self.reclaim(runner=runner)
                self.assertTrue(target.is_dir())

    def test_process_command_referencing_worktree_skips_cache(self):
        target = self.worktree("candidate")

        def runner(args, **kwargs):
            if args[0] == "ps":
                return subprocess.CompletedProcess(args, 0, f"cargo --manifest-path {target.parent}/Cargo.toml\n", "")
            return self.run_command(args, **kwargs)

        self.reclaim(runner=runner)
        self.assertTrue(target.is_dir())

    def test_dry_run_and_apply_select_the_same_qualifying_targets(self):
        target = self.worktree("candidate")
        rejected = self.worktree("recent")
        (rejected / "artifact").touch()
        dependency = target.parent / "node_modules"
        dependency.mkdir()
        dry_output = self.reclaim(apply=False)
        self.assertIn(str(target), dry_output)
        self.assertNotIn(str(rejected), dry_output)
        self.assertTrue(target.is_dir())
        apply_output = self.reclaim()
        self.assertIn(str(target), apply_output)
        self.assertNotIn(str(rejected), apply_output)
        self.assertFalse(target.exists())
        self.assertTrue(rejected.is_dir())
        self.assertTrue(dependency.is_dir())
        self.assertTrue((target.parent / "tracked").is_file())

    def test_recheck_detects_recent_write_during_size_measurement(self):
        target = self.worktree("candidate")

        def runner(args, **kwargs):
            result = self.run_command(args, **kwargs)
            if args[0] == "du":
                (target / "artifact").touch()
            return result

        self.reclaim(runner=runner)
        self.assertTrue(target.is_dir())

    def test_recheck_detects_open_handle_during_size_measurement(self):
        target = self.worktree("candidate")
        busy = False

        def runner(args, **kwargs):
            nonlocal busy
            result = self.run_command(args, **kwargs)
            if args[0] == "du":
                busy = True
            if args[0] == "lsof" and busy:
                return subprocess.CompletedProcess(args, 0, "123\n", "")
            return result

        self.reclaim(runner=runner)
        self.assertTrue(target.is_dir())
