"""Offline merge-train scheduler tests with temporary homes and repositories."""
from contextlib import nullcontext
import hashlib
import json
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

try:
    import merge_train_agent as agent_module
except ModuleNotFoundError as error:
    if error.name != "merge_train_agent":
        raise
    agent_module = None

HEAD = "a" * 40
ORDER = f"12={HEAD}\n"
CHECKOUT = Path(__file__).resolve().parent


class AgentTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(agent_module, "merge-train launchd agent is missing")
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.run_launchctl = Mock(return_value=subprocess.CompletedProcess([], 0, "", ""))
        self.agent = agent_module.Agent(self.root / "repo", home=self.root / "home",
                                        launchctl=self.run_launchctl)
        # Never use a real repo or run the actual train in these tests.
        self.agent.refresh_main = Mock(return_value=HEAD)
        self.agent.run_tick = Mock(return_value=subprocess.CompletedProcess([], 0, "waiting\n"))
        self.agent.checkout = Mock(return_value=nullcontext((self.root / "checkout", self.root / "train.py")))

    def test_order_preserves_priority_comments_and_blank_lines(self):
        raw = f" # priority\n\n34={'b' * 40} # reviewed\n {ORDER}"
        self.assertEqual(agent_module.parse_order(raw), [f"34={'b' * 40}", f"12={HEAD}"])
        self.agent.set_order(raw)
        self.assertEqual(self.agent.status()["order"], raw)

    def test_bad_order_leaves_current_order_untouched(self):
        self.agent.set_order(ORDER)
        for invalid in (
            "15=abc", f"0={HEAD}", f"-1={HEAD}", f"12={'G' * 40}",
            ORDER.strip(), f"13={HEAD} extra", "-h",
        ):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                self.agent.set_order(ORDER + invalid + "\n")
            self.assertEqual(self.agent.order.read_text(), ORDER)

    def test_order_replace_is_atomic_and_interrupted_write_keeps_old_bytes(self):
        self.agent.set_order(ORDER)
        new = f"13={HEAD}\n"
        real_replace = Path.replace

        def inspect_replace(temporary, destination):
            self.assertEqual(temporary.parent, destination.parent)
            self.assertNotEqual(temporary, destination)
            self.assertEqual(temporary.read_text(), new)
            self.assertEqual(destination.read_text(), ORDER)
            return real_replace(temporary, destination)

        with patch.object(Path, "replace", autospec=True, side_effect=inspect_replace) as swap:
            self.agent.set_order(new)
            swap.assert_called_once()
        with patch.object(Path, "replace", side_effect=OSError("interrupted")):
            with self.assertRaisesRegex(OSError, "interrupted"):
                self.agent.set_order(ORDER)
        self.assertEqual(self.agent.order.read_text(), new)

    def test_missing_empty_and_comment_only_orders_skip_without_fetch(self):
        for order in (None, "", "# no approvals\n\n"):
            with self.subTest(order=order):
                if order is not None:
                    self.agent.set_order(order)
                self.assertEqual(self.agent.tick()["status"], "no order")
        self.agent.refresh_main.assert_not_called()
        self.agent.run_tick.assert_not_called()

    def test_malformed_order_on_disk_never_reaches_train(self):
        self.agent.set_order(ORDER)
        self.agent.order.write_text(ORDER + "not approved\n")
        self.assertEqual(self.agent.tick()["status"], "error")
        self.agent.refresh_main.assert_not_called()
        self.agent.run_tick.assert_not_called()

    def test_exit_one_latches_output_and_order_hash_until_resume(self):
        self.agent.set_order(ORDER)
        self.agent.run_tick.return_value = subprocess.CompletedProcess([], 1, "tree mismatch\n")
        self.assertEqual(self.agent.tick()["status"], "stopped")
        saved = json.loads(self.agent.stopped.read_text())
        self.assertRegex(saved["recorded_utc"], r"^\d{4}-\d{2}-\d{2}T")
        self.assertEqual(saved["output"], "tree mismatch\n")
        self.assertEqual(saved["order_sha256"], hashlib.sha256(ORDER.encode()).hexdigest())
        self.agent.set_order(f"13={HEAD}\n")
        self.agent.run_tick.return_value = subprocess.CompletedProcess([], 0, "merged\n")
        self.assertEqual(self.agent.tick()["status"], "latched")
        self.assertEqual(self.agent.status()["stopped"], saved)
        self.agent.run_tick.assert_called_once()
        self.agent.resume()
        self.assertFalse(self.agent.stopped.exists())
        self.assertEqual(self.agent.tick()["status"], "completed")
        self.assertEqual(self.agent.run_tick.call_count, 2)

    def test_exit_zero_outcomes_keep_running(self):
        self.agent.set_order(ORDER)
        for outcome in ("waiting", "pushed batch", "merged", "held"):
            self.agent.run_tick.return_value = subprocess.CompletedProcess([], 0, outcome + "\n")
            self.assertEqual(self.agent.tick()["status"], "completed")
            self.assertFalse(self.agent.stopped.exists())
        self.assertEqual(self.agent.run_tick.call_count, 4)

    def test_log_keeps_approximately_two_thousand_recent_lines(self):
        self.agent.set_order(ORDER)
        self.agent.run_tick.return_value = subprocess.CompletedProcess(
            [], 0, "".join(f"first-{line}\n" for line in range(2500))
        )
        self.agent.tick()
        self.agent.run_tick.return_value = subprocess.CompletedProcess([], 0, "last marker\n")
        self.agent.tick()
        lines = self.agent.log.read_text().splitlines()
        self.assertEqual(len(lines), 2000)
        self.assertNotIn("first-0", lines)
        self.assertIn("first-2499", lines)
        self.assertIn("last marker", lines)

    def test_disable_does_not_bootout_during_tick(self):
        self.agent.set_order(ORDER)
        self.agent.plist.parent.mkdir(parents=True)
        self.agent.plist.touch()
        def disable_during_tick(*_):
            self.assertEqual(self.agent.disable()["status"], "unload deferred")
            self.assertEqual(self.agent.tick()["status"], "busy")
            self.run_launchctl.assert_not_called()
            return subprocess.CompletedProcess([], 0, "completed in-flight work\n")

        self.agent.run_tick.side_effect = disable_during_tick
        self.assertEqual(self.agent.tick()["status"], "completed")
        self.assertEqual(self.agent.tick()["status"], "disabled")
        self.agent.run_tick.assert_called_once()
        self.agent.disable()
        self.assertEqual(self.run_launchctl.call_args.args[0][1], "bootout")


class SourceTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(agent_module, "merge-train launchd agent is missing")
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.origin = self.root / "origin.git"
        self.repo = self.root / "repo"
        self.home = self.root / "home"
        self.git(self.root, "init", "--bare", self.origin)
        self.git(self.root, "init", "-b", "main", self.repo)
        self.git(self.repo, "config", "user.name", "Fixture")
        self.git(self.repo, "config", "user.email", "fixture@example.invalid")
        self.git(self.repo, "config", "core.hooksPath", "/dev/null")
        self.git(self.repo, "remote", "add", "origin", self.origin)
        (self.repo / "scripts").mkdir()
        self.script = self.repo / "scripts/merge_train.py"
        self.launchctl = Mock(return_value=subprocess.CompletedProcess([], 0, "", ""))
        self.agent = agent_module.Agent(self.repo, home=self.home, launchctl=self.launchctl,
                                        origins=(str(self.origin),))

    def git(self, cwd, *args):
        return subprocess.check_output(["git", "-C", str(cwd), *map(str, args)], text=True,
                                       stderr=subprocess.PIPE).strip()

    def publish(self, sources):
        for name, source in sources.items():
            (self.repo / "scripts" / name).write_text(source)
        self.git(self.repo, "add", "scripts")
        self.git(self.repo, "commit", "-m", "fixture")
        self.git(self.repo, "push", "origin", "main")
        return self.git(self.repo, "rev-parse", "HEAD")

    def test_tick_executes_only_fresh_origin_main_in_a_linked_checkout(self):
        fixture = """import json, os, subprocess, sys
def git(*args):
    return subprocess.check_output(['git', *args], text=True).strip()
print(json.dumps({'marker': MARKER, 'argv': sys.argv[1:], 'cwd': os.getcwd(),
                  'rev': git('rev-parse', 'HEAD'),
                  'common': git('rev-parse', '--path-format=absolute', '--git-common-dir'),
                  'status': git('status', '--porcelain')}))
"""
        stale = self.publish({"merge_train.py": "MARKER = 'stale'\n" + fixture})
        lane = self.root / "lane"
        self.git(self.repo, "worktree", "add", "--detach", lane, stale)
        current = self.publish({"merge_train.py": "MARKER = 'protected'\n" + fixture})
        self.git(self.repo, "update-ref", "refs/remotes/origin/main", stale)
        self.script.write_text("raise SystemExit('dirty shared checkout executed')")
        (lane / "scripts/merge_train.py").write_text("raise SystemExit('dirty lane executed')")
        agent = agent_module.Agent(lane, home=self.home, launchctl=self.launchctl,
                                   origins=(str(self.origin),))
        agent.set_order(ORDER)
        self.assertEqual(agent.tick()["status"], "completed")
        data = json.loads(next(line for line in agent.log.read_text().splitlines() if line.startswith("{")))
        self.assertEqual(data["marker"], "protected")
        self.assertEqual(data["argv"], ["--order", f"12={HEAD}", "--batch", "4"])
        self.assertEqual(data["rev"], current)
        self.assertEqual(data["status"], "")
        self.assertEqual(Path(data["common"]), self.repo / ".git")
        self.assertTrue(Path(data["cwd"]).is_relative_to(agent.state))
        self.assertFalse(Path(data["cwd"]).exists())
        self.assertIn("dirty lane", (lane / "scripts/merge_train.py").read_text())

    def test_plist_registers_a_pinned_merged_runner_with_bounded_logging(self):
        names = ("merge_train_agent.py", "refresh_install.py")
        revision = self.publish({name: (CHECKOUT / name).read_text() for name in names})
        self.assertEqual(self.agent.enable()["status"], "enabled")
        plist = plistlib.loads(self.agent.plist.read_bytes())
        self.assertEqual(plist["Label"], "io.github.yuriimurha.grooviq.merge-train")
        self.assertEqual(plist["StartInterval"], 180)
        self.assertFalse(plist["RunAtLoad"])
        self.assertEqual(plist["StandardOutPath"], "/dev/null")
        self.assertEqual(plist["StandardErrorPath"], "/dev/null")
        runner = self.agent.state / "versions" / revision / names[0]
        self.assertEqual(plist["ProgramArguments"],
                         [str(Path(sys.executable).resolve()), "-S", "-B", str(runner),
                          "--repo", str(self.repo), "tick"])
        for name in names:
            self.assertEqual(runner.with_name(name).read_bytes(), (CHECKOUT / name).read_bytes())
        self.assertEqual(self.launchctl.call_args.args[0][1], "bootstrap")

    def test_enable_refuses_unmerged_runner(self):
        self.publish({"merge_train_agent.py": "# other revision\n",
                      "refresh_install.py": (CHECKOUT / "refresh_install.py").read_text()})
        with self.assertRaisesRegex(RuntimeError, "merged source"):
            self.agent.enable()
        self.launchctl.assert_not_called()
        self.assertFalse(self.agent.plist.exists())

    def test_tick_rejects_foreign_push_destination(self):
        self.publish({"merge_train.py": "print('must never run')\n"})
        self.git(self.repo, "remote", "set-url", "--push", "origin", self.origin)
        self.git(self.repo, "remote", "set-url", "--add", "--push", "origin", self.root / "foreign.git")
        self.agent.set_order(ORDER)
        outcome = self.agent.tick()
        self.assertEqual(outcome["status"], "error")
        self.assertIn("canonical Grooviq origin", outcome["error"])
        self.assertNotIn("must never run", self.agent.log.read_text())

    def test_failed_fetch_does_not_use_stale_main(self):
        self.publish({"merge_train.py": "print('must never run')\n"})
        self.origin.rename(self.root / "offline.git")
        self.agent.set_order(ORDER)
        self.assertEqual(self.agent.tick()["status"], "error")
        self.assertNotIn("must never run", self.agent.log.read_text())


if __name__ == "__main__":
    unittest.main()
