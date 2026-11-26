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
from types import SimpleNamespace
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
    def test_free_space_at_threshold_passes(self):
        from reclaim_caches import disk_check

        with tempfile.TemporaryDirectory() as directory:
            with patch.object(shutil, "disk_usage", return_value=SimpleNamespace(
                free=20 * 1024**3,
            )), redirect_stdout(io.StringIO()):
                self.assertEqual(disk_check(Path(directory), 20), 0)

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
            "number": 1,
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
            return self.lsof_record(args, "cwd", "/")
        return subprocess.run(args, capture_output=True, text=True, **kwargs)

    def lsof_record(self, args, descriptor, path):
        return subprocess.CompletedProcess(args, 0, f"p123\0\nf{descriptor}\0n{path}\0\n", "")

    def after_measurement(self, action):
        from reclaim_caches import measure_cache

        def measure_then_change(*args):
            result = measure_cache(*args)
            action()
            return result

        return patch("reclaim_caches.measure_cache", side_effect=measure_then_change)

    def reclaim(self, *, apply=True, runner=None, recent_minutes=45, expected_status=0):
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
        self.assertEqual(status, expected_status, output.getvalue())
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
        artifact = target / "debug" / "deps" / "x"
        artifact.parent.mkdir(parents=True)
        artifact.touch()
        self.age(target, target / "debug", artifact.parent)
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
                return self.lsof_record(args, "5", target / "artifact")
            return self.run_command(args, **kwargs)

        self.reclaim(runner=runner)
        self.assertTrue(target.is_dir())

    def test_lsof_failure_or_timeout_skips_cache(self):
        target = self.worktree("candidate")
        failures = (
            subprocess.CompletedProcess([], 0, "p1\0\nfcwd\0n/\0\n", "lsof: permission denied"),
            subprocess.CompletedProcess([], 2, "p1\0\nfcwd\0n/\0\n", ""),
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

                self.assertIn("lsof snapshot", self.reclaim(runner=runner))
                self.assertTrue(target.is_dir())

    def test_process_command_referencing_worktree_skips_cache(self):
        target = self.worktree("candidate")

        def runner(args, **kwargs):
            if args[0] == "ps":
                return subprocess.CompletedProcess(args, 0, f"cargo --manifest-path {target.parent}/Cargo.toml\n", "")
            return self.run_command(args, **kwargs)

        self.reclaim(runner=runner)
        self.assertTrue(target.is_dir())

    def test_empty_process_snapshot_skips_cache(self):
        target = self.worktree("candidate")

        def runner(args, **kwargs):
            if args[0] == "ps":
                return subprocess.CompletedProcess(args, 0, "", "")
            return self.run_command(args, **kwargs)

        output = self.reclaim(runner=runner)
        self.assertTrue(target.is_dir())
        self.assertIn("process snapshot empty", output)

    def test_dry_run_and_apply_select_the_same_qualifying_targets(self):
        target = self.worktree("candidate")
        rejected = self.worktree("recent")
        (rejected / "artifact").touch()
        dependency = target.parent / "node_modules"
        dependency.mkdir()
        dry_output = self.reclaim(apply=False)
        self.assertIn(str(target), dry_output)
        self.assertIn(f"Skipped {rejected}: recent write", dry_output)
        self.assertTrue(target.is_dir())
        apply_output = self.reclaim()
        self.assertIn(str(target), apply_output)
        self.assertIn(f"Skipped {rejected}: recent write", apply_output)
        self.assertFalse(target.exists())
        self.assertTrue(rejected.is_dir())
        self.assertTrue(dependency.is_dir())
        self.assertTrue((target.parent / "tracked").is_file())

    def test_recheck_detects_recent_write_during_size_measurement(self):
        target = self.worktree("candidate")
        with self.after_measurement(lambda: (target / "artifact").touch()):
            self.reclaim()
        self.assertTrue(target.is_dir())

    def test_recheck_detects_open_handle_during_size_measurement(self):
        target = self.worktree("candidate")
        observed = []

        def runner(args, **kwargs):
            if args[0] == "lsof":
                observed.append(args)
                if len(observed) > 1:
                    return self.lsof_record(args, "5", target / "artifact")
            return self.run_command(args, **kwargs)

        self.assertIn("open handle", self.reclaim(runner=runner))
        self.assertTrue(target.is_dir())

    def test_cwd_and_executable_paths_are_component_matched_in_one_dry_run_snapshot(self):
        cwd = self.worktree("cwd")
        executable = self.worktree("executable")
        sibling = self.worktree("exe")
        observed = []

        def runner(args, **kwargs):
            if args[0] == "lsof":
                observed.append(args)
                cwd_record = self.lsof_record(args, "cwd", cwd.parent).stdout
                exe_record = self.lsof_record(args, "txt", executable.parent / "tool").stdout
                return subprocess.CompletedProcess(args, 0, cwd_record + exe_record, "")
            return self.run_command(args, **kwargs)

        output = self.reclaim(apply=False, runner=runner)
        self.assertEqual(len(observed), 1)
        self.assertIn(f"Skipped {cwd}: process cwd", output)
        self.assertIn(f"Skipped {executable}: open handle", output)
        self.assertIn("Would reclaim", output)
        self.assertNotIn(f"Skipped {sibling}:", output)
        self.assertIn("  1 process cwd", output)
        self.assertIn("  1 open handle", output)

    def test_recent_write_during_final_snapshot_keeps_cache(self):
        target = self.worktree("candidate")
        observed = []

        def runner(args, **kwargs):
            if args[0] == "lsof":
                observed.append(args)
                if len(observed) > 1:
                    (target / "artifact").touch()
            return self.run_command(args, **kwargs)

        self.assertIn("recent write", self.reclaim(runner=runner))
        self.assertTrue(target.is_dir())

    def test_cross_device_target_is_skipped(self):
        target = self.worktree("candidate")
        real_stat = Path.stat

        def parent_on_other_device(path, *args, **kwargs):
            info = real_stat(path, *args, **kwargs)
            if path == target.parent:
                fields = list(info)
                fields[2] = info.st_dev + 1
                return os.stat_result(fields)
            return info

        with patch.object(Path, "stat", parent_on_other_device):
            self.reclaim()
        self.assertTrue(target.is_dir())

    def test_merged_pr_includes_local_head_after_remote_merge(self):
        target = self.worktree("landed", detached=False)
        pr = self.merge_branch(target)
        local = self.git(target.parent, "rev-parse", "HEAD")
        self.git(self.remote, "update-ref", "refs/heads/server-side", local)
        server = self.root / "server"
        self.git(self.root, "clone", "-b", "server-side", str(self.remote), str(server))
        self.git(server, "config", "user.name", "Cache Test")
        self.git(server, "config", "user.email", "cache-test@example.invalid")
        self.git(server, "config", "commit.gpgsign", "false")
        self.git(server, "merge", "--no-ff", "-m", "Merge main at GitHub", "origin/main")
        pr["headRefOid"] = self.git(server, "rev-parse", "HEAD")
        pr["number"] = 42
        self.git(server, "push", "origin", "HEAD:refs/pull/42/head")
        self.git(self.remote, "update-ref", "-d", "refs/heads/server-side")
        self.assertNotEqual(pr["headRefOid"], local)
        absent = subprocess.run(
            ["git", "cat-file", "-t", pr["headRefOid"]], cwd=self.main, capture_output=True,
        )
        self.assertNotEqual(absent.returncode, 0)

        self.reclaim()
        self.assertFalse(target.exists())

    def test_fork_pr_does_not_prove_same_named_branch_merged(self):
        target = self.worktree("candidate", detached=False)
        self.merge_branch(target)["isCrossRepository"] = True
        self.reclaim()
        self.assertTrue(target.is_dir())

    def test_nested_worktrees_keep_both_caches(self):
        outer = self.worktree("outer")
        inner = self.worktree("outer/target/inner")
        self.age(outer, *outer.rglob("*"))
        self.reclaim()
        self.assertTrue(outer.is_dir())
        self.assertTrue(inner.is_dir())

    def test_worktree_below_node_modules_keeps_cache(self):
        target = self.worktree("vendor/node_modules/checkout")
        self.reclaim()
        self.assertTrue(target.is_dir())

    def test_apply_refuses_symlink_unsafe_rmtree(self):
        target = self.worktree("candidate")
        with patch.object(shutil.rmtree, "avoids_symlink_attacks", False):
            self.reclaim(expected_status=1)
        self.assertTrue(target.is_dir())

    def test_ref_switch_before_initial_check_keeps_cache(self):
        target = self.worktree("candidate")

        def runner(args, **kwargs):
            result = self.run_command(args, **kwargs)
            if args[:3] == ["git", "worktree", "list"]:
                self.git(target.parent, "switch", "-c", "local")
            return result

        self.reclaim(runner=runner)
        self.assertTrue(target.is_dir())

    def test_ref_switch_during_measurement_keeps_cache(self):
        target = self.worktree("candidate")
        with self.after_measurement(
            lambda: self.git(target.parent, "commit", "--allow-empty", "-m", "New detached HEAD"),
        ):
            self.reclaim()
        self.assertTrue(target.is_dir())

    def replace_cache(self, target):
        target.rename(self.root / "old-target")
        self.make_cache(target.parent)

    def test_dry_run_skips_target_replaced_during_measurement(self):
        target = self.worktree("candidate")
        with self.after_measurement(lambda: self.replace_cache(target)):
            output = self.reclaim(apply=False)
        self.assertIn(f"Skipped {target}: target changed", output)
        self.assertTrue(target.is_dir())

    def test_apply_skips_target_replaced_at_parent_open(self):
        target = self.worktree("candidate")
        real_open = os.open

        def open_path(path, *args, **kwargs):
            if path == target.parent and args[0] & os.O_NOFOLLOW:
                self.replace_cache(target)
            return real_open(path, *args, **kwargs)

        with patch("reclaim_caches.os.open", side_effect=open_path):
            self.reclaim()
        self.assertTrue(target.is_dir())
        self.assertTrue((self.root / "old-target").is_dir())

    def test_apply_removes_renamed_cache_and_preserves_recreated_target(self):
        target = self.worktree("candidate")
        real_rmtree = shutil.rmtree

        def recreate_then_delete(path, *, dir_fd=None):
            self.assertEqual(path, f".target-reclaim-{os.getpid()}")
            self.assertFalse(target.exists())
            self.make_cache(target.parent)
            return real_rmtree(path, dir_fd=dir_fd)

        recreate_then_delete.avoids_symlink_attacks = True
        with patch("reclaim_caches.shutil.rmtree", recreate_then_delete):
            self.reclaim()
        self.assertTrue(target.is_dir())
        self.assertFalse(target.with_name(f".target-reclaim-{os.getpid()}").exists())

    def test_cli_and_make_accept_only_explicit_apply(self):
        target = self.worktree("candidate")
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        for name, body in {
            "lsof": "printf 'p1\\000\\nfcwd\\000n/\\000\\n'\n",
            "ps": "printf 'logd\\n'\n",
        }.items():
            shim = bin_dir / name
            shim.write_text("#!/bin/sh\n" + body)
            shim.chmod(0o755)
        env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}")

        def cli(*args, **overrides):
            return subprocess.run(
                args, cwd=self.main, env=env | overrides, text=True, capture_output=True, check=True,
            ).stdout

        for value in ("", "0", "yes"):
            with self.subTest(value=value):
                output = cli(sys.executable, "-B", str(SCRIPT), "reclaim-caches", f"--apply={value}")
                self.assertIn("Would reclaim", output)
                self.assertTrue(target.is_dir())

        (self.main / "scripts").mkdir()
        shutil.copyfile(SCRIPT, self.main / "scripts" / SCRIPT.name)
        shutil.copyfile(SCRIPT.parents[1] / "Makefile", self.main / "Makefile")
        output = cli("make", "reclaim-caches", RECLAIM_APPLY="1", APPLY="1")
        self.assertIn("Would reclaim", output)
        self.assertTrue(target.is_dir())
        output = cli("make", "reclaim-caches", 'RECLAIM_APPLY=yes" --apply=1 #')
        self.assertIn("Would reclaim", output)
        self.assertTrue(target.is_dir())
        cli("make", "reclaim-caches", "RECLAIM_APPLY=1")
        self.assertFalse(target.exists())


class CacheIdentityTests(unittest.TestCase):
    def test_measurement_sums_allocated_blocks_without_following_symlinks(self):
        from reclaim_caches import measure_cache

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            target = root / "target"
            target.mkdir()
            external = root / "external"
            external.mkdir()
            (external / "file").write_bytes(os.urandom(16384))
            artifact = target / "artifact"
            with artifact.open("wb") as file:
                file.truncate(2 * 1024**3)
            link = target / "link"
            link.symlink_to(external, target_is_directory=True)
            paths = (target, artifact, link)
            expected = sum(path.lstat().st_blocks for path in paths) * 512
            self.assertEqual(measure_cache(target, target.lstat().st_dev), (expected, None))

    def test_rejects_directory_under_symlinked_parent(self):
        from reclaim_caches import cache_identity

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            real = root / "real"
            (real / "target").mkdir(parents=True)
            alias = root / "alias"
            alias.symlink_to(real, target_is_directory=True)
            self.assertIsNone(cache_identity(alias / "target"))

    def test_rejects_canonical_non_directory(self):
        from reclaim_caches import cache_identity

        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory).resolve() / "target"
            target.touch()
            self.assertIsNone(cache_identity(target))
