"""Offline merge-train scheduler tests with temporary homes and repositories."""
from contextlib import contextmanager, nullcontext, redirect_stderr, redirect_stdout
import hashlib
import io
import json
import os
from pathlib import Path
import plistlib
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

import merge_train

try:
    import merge_train_agent as agent_module
except ModuleNotFoundError as error:
    if error.name != "merge_train_agent":
        raise
    agent_module = None

HEAD = "a" * 40
TESTED = "b" * 40
LANDED = "c" * 40
TREE = "d" * 40
NEXT = "e" * 40
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
        self.agent.prune_checkouts = Mock()
        self.agent.refresh_main = Mock(return_value=HEAD)
        self.agent.remote_tips = Mock(return_value={"main": HEAD, "batch": None})
        self.agent.run_tick = Mock(return_value=subprocess.CompletedProcess([], 0, "waiting\n"))
        self.scratch = self.agent.state / "tick-fixture"
        self.agent.checkout = Mock(return_value=nullcontext((self.scratch / "checkout", self.scratch / "train.py")))

    def test_order_preserves_priority_comments_and_blank_lines(self):
        raw = f" # priority\n\n34={'b' * 40} # reviewed\n {ORDER}"
        self.assertEqual(agent_module.parse_order(raw), ([f"34={'b' * 40}", f"12={HEAD}"], 4))
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
        for order in (None, "", "# no approvals\n\n", "batch=3\n"):
            with self.subTest(order=order):
                if order is not None:
                    self.agent.set_order(order)
                self.assertEqual(self.agent.tick()["status"], "no order")
        self.agent.refresh_main.assert_not_called()
        self.agent.run_tick.assert_not_called()

    def test_batch_line_sets_tick_size_and_preserves_priority(self):
        raw = f"batch=10 # group size\n{ORDER}13={HEAD}\n"
        self.agent.set_order(raw)
        self.assertEqual(self.agent.status()["order"], raw)
        self.assertEqual(self.agent.tick()["status"], "completed")
        self.assertEqual(self.agent.run_tick.call_args.args[2:], ([f"12={HEAD}", f"13={HEAD}"], 10))

    def test_bad_batch_line_preserves_the_previous_order(self):
        self.agent.set_order(ORDER)
        for invalid in ("batch=0", "batch=11", "batch=-1", "batch=4.0", "batch=", "batch=1\nbatch=2"):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                self.agent.set_order(ORDER + invalid + "\n")
            self.assertEqual(self.agent.order.read_text(), ORDER)

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

    def test_latch_keeps_only_the_last_two_hundred_output_lines(self):
        self.agent.set_order(ORDER)
        lines = [f"line {number}\n" for number in range(250)]
        self.agent.latch({}, "".join(lines), ["fixture"])
        self.assertEqual(json.loads(self.agent.stopped.read_text())["output"], "".join(lines[-200:]))

    def test_latch_caps_output_bytes_without_splitting_utf8(self):
        self.agent.set_order(ORDER)
        self.agent.latch({}, "😀" * 20_000 + "end", ["fixture"])
        output = json.loads(self.agent.stopped.read_text())["output"]
        self.assertLessEqual(len(output.encode("utf-8")), 65_536)
        self.assertTrue(output.endswith("😀end"))
        self.assertTrue(all(char == "😀" for char in output[:-3]))

    def test_exit_zero_outcomes_keep_running(self):
        self.agent.set_order(ORDER)
        for outcome in ("waiting", "pushed batch", "merged", "held"):
            self.agent.run_tick.return_value = subprocess.CompletedProcess([], 0, outcome + "\n")
            self.assertEqual(self.agent.tick()["status"], "completed")
            self.assertFalse(self.agent.stopped.exists())
        self.assertEqual(self.agent.run_tick.call_count, 4)

    def test_exit_seventy_five_skips_busy_lock_and_next_tick_runs(self):
        self.agent.set_order(ORDER)
        self.agent.run_tick.side_effect = [
            subprocess.CompletedProcess([], 75, "fixture exit output\n"),
            subprocess.CompletedProcess([], 1, "tree mismatch\n"),
        ]
        result = self.agent.tick()
        self.assertEqual(result["status"], "skipped: busy")
        self.assertEqual(result["exit_code"], 75)
        self.assertFalse(self.agent.stopped.exists())
        self.assertIn("skipped: busy", self.agent.log.read_text())
        self.assertEqual(self.agent.tick()["status"], "stopped")
        self.assertTrue(self.agent.stopped.exists())

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

    def landed_tick(self, *, landed_tree=TREE, run_id=123):
        self.agent.set_order(ORDER)
        self.agent.remote_tips.side_effect = [
            {"main": HEAD, "batch": NEXT}, {"main": NEXT, "batch": NEXT},
        ]
        self.agent.commit_tree = Mock(side_effect=lambda sha: TREE if sha == TESTED else landed_tree)
        self.agent.ci_run_id = Mock(return_value=run_id)
        self.agent.run_tick.return_value = subprocess.CompletedProcess(
            [], 1, f"Tested batch {NEXT}\nMerged #12 at {HEAD}\nHeld #13: checks pending\n"
                   f"Tested batch {TESTED}\nMerged #14 at {LANDED}\n"
        )

    def test_ledger_uses_reported_landing_even_if_main_moved_again(self):
        self.landed_tick()
        self.assertEqual(self.agent.tick()["status"], "stopped")
        tick = json.loads(self.agent.ticks.read_text())
        self.assertEqual(tick["before"], {"main": HEAD, "batch": NEXT})
        self.assertEqual(tick["after"], {"main": NEXT, "batch": NEXT})
        self.assertEqual(tick["stdout"], self.agent.run_tick.return_value.stdout.splitlines())
        self.agent.commit_tree.assert_any_call(LANDED)
        self.agent.ci_run_id.assert_called_once_with(TESTED)
        self.assertEqual(self.agent.ledger(), [
            f"| {TESTED} | https://github.com/YuriiMurha/grooviq/actions/runs/123 | "
            f"{LANDED} | {TREE} | {TREE} | yes | complete |"
        ])

    def test_tree_mismatch_and_ci_failure_remain_visible_after_merge(self):
        self.landed_tick(landed_tree=NEXT)
        self.agent.ci_run_id.side_effect = RuntimeError("run lookup failed")
        self.agent.run_tick.return_value.stderr = f"Merge train stopped: main tree {NEXT} != tested tree {TREE}.\n"
        self.assertEqual(self.agent.tick()["status"], "stopped")
        record = json.loads((self.agent.state / "ledger.jsonl").read_text())
        self.assertIn("run lookup failed", record["ci_error"])
        self.assertEqual(self.agent.ledger(), [
            f"| {TESTED} | — | {LANDED} | {NEXT} | {TREE} | no | mismatch |"
        ])

    def test_recovered_prefix_is_partial_and_survives_tick_rotation(self):
        self.landed_tick(landed_tree=NEXT)
        self.agent.run_tick.return_value = subprocess.CompletedProcess(
            [], 0, f"Tested batch {TESTED}\nMerged #12 at {LANDED}\nRecovered landed prefix of batch {TESTED} (12).\n"
        )
        with patch.object(agent_module, "TICK_HISTORY", 1):
            self.assertEqual(self.agent.tick()["status"], "completed")
            self.agent.remote_tips.side_effect = None
            self.agent.run_tick.return_value = subprocess.CompletedProcess([], 0, "Skipped: Merged #12")
            self.agent.tick()
        self.assertEqual(len(self.agent.ticks.read_text().splitlines()), 1)
        self.assertEqual(self.agent.ledger(), [
            f"| {TESTED} | https://github.com/YuriiMurha/grooviq/actions/runs/123 | "
            f"{LANDED} | {NEXT} | {TREE} | no | partial |"
        ])

    def test_both_landings_are_appended_to_the_ledger(self):
        self.landed_tick()
        self.agent.remote_tips.side_effect = None
        self.agent.run_tick.side_effect = [
            subprocess.CompletedProcess([], 0, f"Tested batch {TESTED}\nMerged #12 at {LANDED}\n"),
            subprocess.CompletedProcess([], 0, f"Tested batch {NEXT}\nMerged #14 at {TREE}\n"),
        ]
        with patch.object(agent_module, "TICK_HISTORY", 1):
            for _ in range(2):
                self.assertEqual(self.agent.tick()["status"], "completed")
        rows = [json.loads(line) for line in self.agent.landings.read_text().splitlines()]
        self.assertEqual([(row["tested_batch"], row["landed_tip"]) for row in rows],
                         [(TESTED, LANDED), (NEXT, TREE)])

    def test_matching_trees_without_a_ci_run_are_partial(self):
        self.landed_tick()
        self.agent.ci_run_id.side_effect = RuntimeError("run lookup failed")
        self.agent.tick()
        row = json.loads(self.agent.landings.read_text())
        self.assertTrue(row["match"])
        self.assertEqual(row["status"], "partial")

    def test_tick_record_has_a_byte_cap_and_keeps_valid_json(self):
        self.agent.set_order(ORDER)
        self.agent.run_tick.return_value = subprocess.CompletedProcess([], 1, "é😀" * 100_000, "HTTP 502")
        self.assertEqual(self.agent.tick()["status"], "retry: network")
        raw = self.agent.ticks.read_bytes()
        self.assertLessEqual(len(raw), 65_536)
        record = json.loads(raw)
        self.assertEqual(record["exit_code"], 1)
        self.assertEqual(record["network_streak"], 1)
        self.assertTrue(record["output_truncated"])
        self.assertEqual(self.agent.tick()["status"], "retry: network")
        latest = json.loads(self.agent.ticks.read_text().splitlines()[-1])
        self.assertEqual(latest["network_streak"], 2)

    def test_tick_log_rotates_and_keeps_latest_records(self):
        self.agent.set_order(ORDER)
        with patch.object(agent_module, "TICK_HISTORY", 2):
            for code in (0, 75, 1):
                self.agent.run_tick.return_value = subprocess.CompletedProcess([], code, "")
                self.agent.tick()
        records = [json.loads(line) for line in self.agent.ticks.read_text().splitlines()]
        self.assertEqual([record["exit_code"] for record in records], [75, 1])

    def test_failed_post_tick_ref_lookup_preserves_the_stop(self):
        self.agent.set_order(ORDER)
        self.agent.remote_tips.side_effect = [
            {"main": HEAD, "batch": TESTED}, RuntimeError("connection unavailable"),
        ]
        self.agent.run_tick.return_value = subprocess.CompletedProcess([], 1, "partial merge\n")
        self.assertEqual(self.agent.tick()["status"], "stopped")
        self.assertTrue(self.agent.stopped.exists())
        record = json.loads(self.agent.ticks.read_text())
        self.assertIsNone(record["after"])
        self.assertIn("connection unavailable", record["after_error"])

    def test_stop_file_skips_before_reading_order_or_taking_tick_lock(self):
        stop = self.agent.home / ".grooviq-train" / "STOP"
        stop.parent.mkdir(parents=True)
        stop.touch()
        self.assertEqual(self.agent.tick()["status"], "skipped: STOP file")
        record = json.loads(self.agent.ticks.read_text())
        self.assertEqual((record["status"], record["exit_code"]), ("skipped: STOP file", 0))
        self.assertEqual(list(self.agent.state.iterdir()), [self.agent.ticks])
        self.agent.refresh_main.assert_not_called()
        self.agent.remote_tips.assert_not_called()
        self.agent.checkout.assert_not_called()
        self.agent.run_tick.assert_not_called()

    def test_help_and_status_show_emergency_disable_command(self):
        command = "launchctl bootout gui/$(id -u)/io.github.yuriimurha.grooviq.merge-train"
        for args in (["--help"], ["status"]):
            with self.subTest(args=args), patch.object(sys, "argv", ["agent", *args]), \
                    patch.object(sys, "platform", "darwin"), patch.object(agent_module, "Agent", return_value=self.agent), \
                    redirect_stdout(io.StringIO()) as output:
                if args == ["--help"]:
                    with self.assertRaises(SystemExit) as exit_status:
                        agent_module.main()
                    self.assertEqual(exit_status.exception.code, 0)
                else:
                    self.assertEqual(agent_module.main(), 0)
                lines = output.getvalue().splitlines()
                self.assertEqual(lines[:4], [
                    "touch ~/.grooviq-train/STOP", "make merge-train-agent-disable",
                    "launchctl disable gui/$(id -u)/io.github.yuriimurha.grooviq.merge-train", command,
                ])

    def test_status_reports_stop_launchd_state_and_old_tick(self):
        self.agent.set_order(ORDER)
        self.agent.kill_switch.parent.mkdir()
        self.agent.kill_switch.touch()
        self.agent.append_tick({"recorded_utc": "2000-01-01T00:00:00+00:00", "status": "fixture"})
        self.run_launchctl.side_effect = [
            subprocess.CompletedProcess([], 0, "state = running"),
            subprocess.CompletedProcess([], 0, f'"{agent_module.LABEL}" => disabled'),
        ]
        status = self.agent.status()
        self.assertTrue(status["stop_file_present"])
        self.assertTrue(status["launchd"]["loaded"])
        self.assertTrue(status["launchd"]["disabled"])
        self.assertEqual(status["last_tick"]["status"], "fixture")
        self.assertTrue(status["last_tick_stale"])
        self.assertGreater(status["last_tick_age_seconds"], 2 * agent_module.INTERVAL)


    def test_nonzero_exits_except_tempfail_latch_with_stderr_tail(self):
        self.agent.set_order(ORDER)
        for code in (1, 2, 124, -9):
            with self.subTest(code=code):
                self.agent.run_tick.return_value = subprocess.CompletedProcess(
                    [], code, "unrelated stdout\n", "old stderr\n" * 30 + "latest stderr\n"
                )
                self.assertEqual(self.agent.tick()["status"], "stopped")
                record = json.loads(self.agent.ticks.read_text().splitlines()[-1])
                self.assertEqual(record["exit_code"], code)
                self.assertEqual(record["reason"][-1], "latest stderr")
                self.assertLess(len(record["reason"]), 30)
                self.assertEqual(self.agent.tick()["status"], "latched")
                self.agent.resume()

    def test_network_failures_latch_after_three_and_clean_ticks_reset_streak(self):
        self.agent.set_order(ORDER)
        results = [
            (1, "HTTP/2 502", "retry: network"),
            (75, "Could not resolve host", "skipped: busy"),
            (1, "HTTP/2 502", "retry: network"),
            (0, "waiting", "completed"),
            (1, "HTTP/2 502", "retry: network"),
            (1, "Could not resolve host: github.com", "retry: network"),
            (1, "connection timed out", "stopped"),
        ]
        for code, message, status in results:
            self.agent.run_tick.return_value = subprocess.CompletedProcess([], code, "", message)
            self.assertEqual(self.agent.tick()["status"], status)
            self.assertEqual(self.agent.stopped.exists(), status == "stopped")
        records = [json.loads(line) for line in self.agent.ticks.read_text().splitlines()]
        self.assertEqual([record["network_streak"] for record in records], [1, 0, 1, 0, 1, 2, 3])
        self.assertIn("connection timed out", records[-1]["reason"])
        self.assertEqual(self.agent.tick()["status"], "latched")

    def test_network_failure_after_write_latches_immediately(self):
        self.agent.set_order(ORDER)
        for stdout, stderr in (
            (f"Merged #12 at {LANDED}\n", ""),
            (f"Pushed batch {TESTED}\n", ""),
            ("Removed #12 from the train\n", ""),
            ("Stopped at #12: failure\n", ""),
            ("", "Merge train stopped: Git push failed: "),
            ("", "Merge train stopped: GitHub DELETE failed: "),
        ):
            with self.subTest(stdout=stdout, stderr=stderr):
                self.agent.run_tick.return_value = subprocess.CompletedProcess([], 1, stdout, stderr + "HTTP 502")
                self.assertEqual(self.agent.tick()["status"], "stopped")
                self.assertEqual(json.loads(self.agent.ticks.read_text().splitlines()[-1])["network_streak"], 0)
                self.agent.resume()

    def test_real_train_write_markers_block_retry_on_502(self):
        self.agent.set_order(ORDER)
        for name in ("gh", "git"):
            fake = self.root / name
            fake.write_text("#!/bin/sh\necho 'HTTP 502' >&2\nexit 1\n")
            fake.chmod(0o755)
        operations = {
            method: lambda method=method: merge_train.GitHub().api("fixture", method=method)
            for method in ("POST", "PUT", "DELETE")
        }
        operations["git push"] = lambda: merge_train.Git(self.root).publish(TESTED, "")
        for name, write in operations.items():
            with self.subTest(operation=name):
                with patch.dict(os.environ, PATH=str(self.root)), redirect_stderr(io.StringIO()) as output:
                    with self.assertRaises(subprocess.CalledProcessError) as failure:
                        write()
                self.assertEqual(merge_train.error_detail(failure.exception), "HTTP 502")
                stderr = f"{output.getvalue()}Merge train stopped: {merge_train.error_detail(failure.exception)}"
                self.agent.run_tick.return_value = subprocess.CompletedProcess([], 1, "", stderr)
                self.assertEqual(self.agent.tick()["status"], "stopped")
                self.assertTrue(self.agent.stopped.exists())
                self.agent.resume()

    def test_timeout_and_signal_with_network_text_latch_without_retry(self):
        self.agent.set_order(ORDER)
        for code in (124, -9):
            with self.subTest(code=code):
                self.agent.run_tick.return_value = subprocess.CompletedProcess([], code, "", "HTTP 502")
                self.assertEqual(self.agent.tick()["status"], "stopped")
                self.assertEqual(json.loads(self.agent.ticks.read_text().splitlines()[-1])["network_streak"], 0)
                self.agent.resume()

    def test_network_streak_restarts_at_one_after_resume(self):
        self.agent.set_order(ORDER)
        self.agent.run_tick.return_value = subprocess.CompletedProcess([], 1, "", "HTTP 502")
        for expected in ("retry: network", "retry: network", "stopped"):
            self.assertEqual(self.agent.tick()["status"], expected)
        self.agent.resume()
        self.assertEqual(self.agent.tick()["status"], "retry: network")
        self.assertEqual(json.loads(self.agent.ticks.read_text().splitlines()[-1])["network_streak"], 1)

    def test_stop_created_by_pending_write_prevents_spawn_and_clears_pending(self):
        self.agent.set_order(ORDER)
        latch = self.agent.latch

        def write_pending(*args, **kwargs):
            latch(*args, **kwargs)
            if kwargs.get("pending"):
                self.agent.kill_switch.parent.mkdir()
                self.agent.kill_switch.touch()

        with patch.object(self.agent, "latch", side_effect=write_pending):
            self.assertEqual(self.agent.tick()["status"], "skipped: STOP file")
        self.agent.run_tick.assert_not_called()
        self.assertFalse(self.agent.stopped.exists())

    def test_failed_pending_write_never_starts_train(self):
        self.agent.set_order(ORDER)
        with patch.object(agent_module, "atomic_json", side_effect=OSError("disk full")):
            with self.assertRaisesRegex(OSError, "disk full"):
                self.agent.tick()
        self.agent.run_tick.assert_not_called()

    def test_sigkill_leaves_pending_record_and_next_tick_latches(self):
        self.agent.set_order(ORDER)
        def kill_agent(*_, **__):
            os.kill(os.getpid(), signal.SIGKILL)
        self.agent.run_tick.side_effect = kill_agent
        pid = os.fork()
        if pid == 0:
            try:
                self.agent.tick()
            finally:
                os._exit(2)
        _, status = os.waitpid(pid, 0)
        self.assertEqual(os.waitstatus_to_exitcode(status), -signal.SIGKILL)
        self.assertTrue(json.loads(self.agent.stopped.read_text())["pending"])
        self.agent.run_tick.reset_mock()
        self.assertEqual(self.agent.tick()["status"], "latched")
        stopped = json.loads(self.agent.stopped.read_text())
        self.assertFalse(stopped["pending"])
        self.assertIn("previous tick did not finish", stopped["reason"])
        self.agent.run_tick.assert_not_called()

    def test_resume_requires_recovering_the_pending_tick_first(self):
        self.agent.set_order(ORDER)
        self.agent.latch({}, "", ["tick in progress"], pending=True)
        pending = self.agent.stopped.read_bytes()
        with self.assertRaisesRegex(RuntimeError, "run a tick to latch it first"):
            self.agent.resume()
        self.assertEqual(self.agent.stopped.read_bytes(), pending)
        self.assertEqual(self.agent.tick()["status"], "latched")
        self.assertEqual(self.agent.resume()["status"], "resumed")

    def test_resume_cannot_clear_the_stop_record_during_a_tick(self):
        self.agent.set_order(ORDER)
        self.agent.run_tick.return_value = subprocess.CompletedProcess([], 1, "tree mismatch")
        self.agent.tick()
        with agent_module.locked(self.agent.tick_lock), self.assertRaises(agent_module.LockBusy):
            self.agent.resume()
        self.assertTrue(self.agent.stopped.exists())

    def test_stop_appearing_after_checkout_prevents_child_start(self):
        self.agent.set_order(ORDER)
        @contextmanager
        def checkout(_):
            self.agent.kill_switch.parent.mkdir()
            self.agent.kill_switch.symlink_to(self.root / "nonexistent")
            yield self.root, self.root / "train.py"

        self.agent.checkout = checkout
        self.assertEqual(self.agent.tick()["status"], "skipped: STOP file")
        self.agent.run_tick.assert_not_called()

    def test_dangling_stop_link_blocks_all_train_setup(self):
        self.agent.kill_switch.parent.mkdir(parents=True)
        self.agent.kill_switch.symlink_to(self.root / "nonexistent")
        self.assertEqual(self.agent.tick()["status"], "skipped: STOP file")
        self.agent.refresh_main.assert_not_called()

    def test_snapshot_failure_latches_and_records_git_stderr(self):
        self.agent.set_order(ORDER)
        self.agent.refresh_main.side_effect = subprocess.CalledProcessError(
            128, ["git", "remote", "get-url"], stderr=b"fatal: authentication failed\n"
        )
        self.assertEqual(self.agent.tick()["status"], "error")
        self.assertEqual(self.agent.tick()["status"], "latched")
        record = json.loads(self.agent.ticks.read_text())
        self.assertEqual(record["exit_code"], 128)
        self.assertEqual(record["reason"], ["fatal: authentication failed"])
        self.agent.run_tick.assert_not_called()

    def test_checkout_cleanup_failure_keeps_one_row_with_train_evidence(self):
        self.agent.set_order(ORDER)
        @contextmanager
        def checkout(_):
            yield self.root / "checkout", self.root / "train.py"
            raise RuntimeError("checkout cleanup failed")

        self.agent.checkout = checkout
        self.assertEqual(self.agent.tick()["status"], "error")
        self.assertEqual(self.agent.tick()["status"], "latched")
        records = [json.loads(line) for line in self.agent.ticks.read_text().splitlines()]
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]["exit_code"], 0)
        self.assertEqual(records[0]["stdout"], ["waiting"])
        self.assertIn("checkout cleanup failed", records[0]["agent_error"])

    def test_cleanup_failure_cannot_overwrite_final_train_latch(self):
        self.agent.set_order(ORDER)
        self.agent.run_tick.return_value = subprocess.CompletedProcess([], 1, "", "original train stop")
        @contextmanager
        def checkout(_):
            yield self.root, self.root / "train.py"
            raise RuntimeError("cleanup failed after train stopped")

        self.agent.checkout = checkout
        self.assertEqual(self.agent.tick()["status"], "error")
        self.assertEqual(json.loads(self.agent.stopped.read_text())["reason"], ["original train stop"])

    def test_timed_out_train_is_killed_as_a_group_and_latched(self):
        self.agent.set_order(ORDER)
        self.agent.run_tick = agent_module.Agent.run_tick.__get__(self.agent)
        self.scratch.mkdir()
        process = Mock(pid=321, returncode=-signal.SIGKILL)
        process.wait.side_effect = [subprocess.TimeoutExpired("fixture", 600), -signal.SIGKILL]

        def spawn(*args, stdout, stderr, **kwargs):
            stdout.write(b"partial")
            stderr.write(b"last error")
            return popen_context

        popen_context = Mock()
        popen_context.__enter__ = Mock(return_value=process)
        popen_context.__exit__ = Mock(return_value=False)
        with patch.object(agent_module.subprocess, "Popen", side_effect=spawn) as popen, \
                patch.object(agent_module.os, "killpg") as kill:
            self.assertEqual(self.agent.tick()["status"], "stopped")
        self.assertEqual(process.wait.call_args_list[0].kwargs, {"timeout": 600})
        self.assertEqual(process.wait.call_count, 2)
        self.assertTrue(popen.call_args.kwargs["start_new_session"])
        self.assertEqual(len(popen.call_args.kwargs["pass_fds"]), 1)
        kill.assert_called_once_with(321, signal.SIGKILL)
        record = json.loads(self.agent.ticks.read_text())
        self.assertTrue(record["timed_out"])
        self.assertIn("last error", record["reason"])
        self.assertIn("wall-clock limit", " ".join(record["reason"]))
        self.assertTrue(self.agent.stopped.exists())

    def test_timeout_retains_stdout_from_real_unbuffered_child(self):
        script = self.root / "train.py"
        script.write_text("import time\nprint('visible before hang')\ntime.sleep(60)\n")
        with patch.object(agent_module, "TICK_TIMEOUT", 0.75), self.assertRaises(subprocess.TimeoutExpired) as error:
            agent_module.Agent.run_tick(self.agent, self.root, script, [], 4)
        self.assertEqual(error.exception.stdout, "visible before hang\n")

    def test_git_allow_list_rejects_github_writes_before_subprocess(self):
        self.assertEqual(set(agent_module.GIT_COMMANDS), {
            ("remote", "get-url"), ("fetch",), ("rev-parse",), ("show",), ("ls-remote",),
            ("-c", "core.hooksPath=/dev/null", "worktree", "add"), ("worktree", "remove"), ("worktree", "list"),
        })
        with patch.object(agent_module.subprocess, "run") as runner:
            for args in (("push", "origin", "main"), ("remote", "set-url"), ("worktree", "prune")):
                with self.subTest(args=args), self.assertRaisesRegex(RuntimeError, "not allowed"):
                    self.agent.git(*args)
            runner.assert_not_called()
        self.assertEqual(agent_module.CI_RUN_COMMAND[:3], ("gh", "run", "list"))

    def test_foreground_tick_shows_stdout_and_stderr(self):
        self.agent.set_order(ORDER)
        self.agent.run_tick.return_value = subprocess.CompletedProcess([], 0, "train stdout\n", "train stderr\n")
        with patch.object(sys, "argv", ["agent", "tick"]), patch.object(sys, "platform", "darwin"), \
                patch.object(agent_module, "Agent", return_value=self.agent), \
                redirect_stdout(io.StringIO()) as output, redirect_stderr(io.StringIO()) as errors:
            self.assertEqual(agent_module.main(), 0)
        self.assertIn("train stdout", output.getvalue())
        self.assertIn("train stderr", errors.getvalue())
        self.agent.run_tick.assert_called_once()

    def test_ci_run_lookup_selects_the_exact_commit(self):
        runs = json.dumps([
            {"databaseId": 999, "headSha": NEXT}, {"databaseId": 123, "headSha": TESTED},
        ]).encode()
        with patch.object(agent_module.subprocess, "run", return_value=subprocess.CompletedProcess(
            [], 0, runs
        )) as runner:
            self.assertEqual(self.agent.ci_run_id(TESTED), 123)
            self.assertIn("ci.yml", runner.call_args.args[0])
            with self.assertRaisesRegex(RuntimeError, "No ci run"):
                self.agent.ci_run_id(HEAD)

    def test_disable_does_not_bootout_during_tick(self):
        self.agent.set_order(ORDER)
        self.agent.plist.parent.mkdir(parents=True)
        self.agent.plist.touch()
        def disable_during_tick(*_, **__):
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
                  'files': os.listdir(), 'path': os.environ.get('PATH'),
                  'developer_dir': os.environ.get('DEVELOPER_DIR'), 'git_dir': os.environ.get('GIT_DIR'),
                  'extra': sorted(k for k in os.environ if k.startswith(('PYTHON', 'GIT_CONFIG_'))
                                  or k in ('GIT_SSH_COMMAND', 'GIT_EXEC_PATH')),
                  'flags': [sys.flags.isolated, sys.flags.ignore_environment, sys.flags.no_user_site]}))
"""
        stale = self.publish({"merge_train.py": "MARKER = 'stale'\n" + fixture})
        lane = self.root / "lane"
        self.git(self.repo, "worktree", "add", "--detach", lane, stale)
        current = self.publish({"merge_train.py": "MARKER = 'protected'\n" + fixture})
        self.git(self.repo, "push", "origin", f"{stale}:refs/heads/merge-train/batch")
        self.git(self.repo, "update-ref", "refs/remotes/origin/main", stale)
        self.script.write_text("raise SystemExit('dirty shared checkout executed')")
        (lane / "scripts/merge_train.py").write_text("raise SystemExit('dirty lane executed')")
        before_worktrees = self.git(self.repo, "worktree", "list", "--porcelain")
        injected = {key: "fixture-value" for key in (
            "PATH", "DEVELOPER_DIR", "GIT_DIR", "PYTHONPATH", "PYTHONUNBUFFERED",
            "GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0", "GIT_SSH_COMMAND", "GIT_EXEC_PATH",
        )}
        with patch.dict(os.environ, injected):
            agent = agent_module.Agent(lane, home=self.home, launchctl=self.launchctl,
                                       origins=(str(self.origin),))
        agent.set_order(ORDER)
        outcome = agent.tick()
        self.assertEqual(outcome["status"], "completed", outcome)
        data = json.loads(next(line for line in agent.log.read_text().splitlines() if line.startswith("{")))
        self.assertEqual(data["marker"], "protected")
        self.assertEqual(data["argv"], ["--order", f"12={HEAD}", "--batch", "4"])
        self.assertEqual(json.loads(agent.ticks.read_text())["before"], {"main": current, "batch": stale})
        self.assertEqual(data["rev"], current)
        self.assertEqual(data["files"], [".git"])
        self.assertEqual(data["path"], "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin")
        self.assertEqual(data["developer_dir"], "/Library/Developer/CommandLineTools")
        self.assertIsNone(data["git_dir"])
        self.assertEqual(data["flags"], [1, 1, 1])
        self.assertEqual(data["extra"], [])
        self.assertEqual(Path(data["common"]), self.repo / ".git")
        self.assertTrue(Path(data["cwd"]).is_relative_to(agent.state))
        self.assertFalse(Path(data["cwd"]).exists())
        self.assertIn("dirty lane", (lane / "scripts/merge_train.py").read_text())
        self.assertEqual(self.git(self.repo, "worktree", "list", "--porcelain"), before_worktrees)

    def test_tick_prunes_leftover_worktrees_and_dirs(self):
        revision = self.publish({"merge_train.py": "print('fixture')\n"})
        self.agent.set_order(ORDER)
        before_worktrees = self.git(self.repo, "worktree", "list", "--porcelain")
        stale = self.agent.state / "tick-leftover"
        stale.mkdir()
        self.git(self.repo, "worktree", "add", "--detach", "--no-checkout", stale / "checkout", revision)
        (self.agent.state / "tick-dir-only").mkdir()
        outcome = self.agent.tick()
        self.assertEqual(outcome["status"], "completed", outcome)
        self.assertEqual(list(self.agent.state.glob("tick-*")), [])
        self.assertEqual(self.git(self.repo, "worktree", "list", "--porcelain"), before_worktrees)

    def check_orphan_finishes(self, interrupt):
        stdout = [f"Tested batch {TESTED}", f"Merged #12 at {HEAD}", f"Merged #14 at {LANDED}"]
        self.publish({"merge_train.py": f"""from pathlib import Path
import sys
import time

root = Path(__file__).parent
print({stdout[0]!r})
print({stdout[1]!r})
(root / "merged-1").touch()
deadline = time.monotonic() + 10
while not (root / "continue").exists():
    if time.monotonic() >= deadline:
        sys.exit("fixture did not receive permission to continue")
    time.sleep(0.01)
print({stdout[2]!r})
print("finished", file=sys.stderr)
(root / "finished").touch()
"""})
        self.agent.set_order(ORDER)
        before = self.git(self.repo, "worktree", "list", "--porcelain")
        self.agent.commit_tree = Mock(return_value=TREE)
        self.agent.ci_run_id = Mock(return_value=123)
        supervisor_source = f"""import sys
sys.path.insert(0, {str(CHECKOUT)!r})
from merge_train_agent import Agent
Agent({str(self.repo)!r}, home={str(self.home)!r}, origins=({str(self.origin)!r},)).tick()
"""

        def wait_for(predicate):
            deadline = time.monotonic() + 4
            while time.monotonic() < deadline:
                if result := predicate():
                    return result
                time.sleep(0.01)
            self.fail("Timed out waiting for fixture")

        with subprocess.Popen([sys.executable, "-B", "-c", supervisor_source],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL) as supervisor:
            try:
                merged_one = wait_for(lambda: next(self.agent.state.glob("tick-*/merged-1"), None))
                scratch = merged_one.parent
                supervisor.send_signal(interrupt)
                self.assertNotEqual(supervisor.wait(timeout=4), 0)
                self.assertEqual(self.agent.tick()["status"], "busy")
                self.assertTrue((scratch / "checkout" / ".git").is_file())
                (scratch / "continue").touch()
                wait_for(lambda: (scratch / "finished").exists())

                def next_tick():
                    result = self.agent.tick()
                    return result if result["status"] != "busy" else None

                self.assertEqual(wait_for(next_tick)["status"], "latched")
                stopped = json.loads(self.agent.stopped.read_text())
                self.assertEqual(stopped["reason"], ["previous tick did not finish"])
                self.assertFalse(stopped["pending"])
                self.assertEqual(stopped["output"].splitlines(), [*stdout, "finished"])
                row = json.loads(self.agent.landings.read_text())
                self.assertEqual(row["tested_batch"], TESTED)
                self.assertEqual(row["landed_tip"], LANDED)
                self.assertEqual((row["tested_tree"], row["landed_tree"]), (TREE, TREE))
                self.assertEqual(row["ci_run_id"], 123)
                self.assertTrue(row["match"])
                self.agent.ci_run_id.assert_called_once_with(TESTED)
                self.agent.commit_tree.assert_any_call(LANDED)
                self.agent.commit_tree.assert_any_call(TESTED)
                log = self.agent.log.read_text()
                self.assertIn("orphaned tick output", log)
                self.assertIn(stdout[2], log)
                self.assertIn("finished", log)
                self.assertFalse(scratch.exists())
                self.assertEqual(self.git(self.repo, "worktree", "list", "--porcelain"), before)
            finally:
                if supervisor.poll() is None:
                    supervisor.kill()
                for scratch in self.agent.state.glob("tick-*"):
                    (scratch / "continue").touch()

    def test_orphan_train_finishes_and_reattaches_output_after_sigterm(self):
        self.check_orphan_finishes(signal.SIGTERM)

    def test_orphan_train_finishes_and_reattaches_output_after_sigint(self):
        self.check_orphan_finishes(signal.SIGINT)

    def test_plist_registers_a_pinned_merged_runner_with_bounded_logging(self):
        names = ("merge_train_agent.py", "refresh_install.py")
        revision = self.publish({name: (CHECKOUT / name).read_text() for name in names})
        self.assertEqual(self.agent.enable()["status"], "enabled")
        plist = plistlib.loads(self.agent.plist.read_bytes())
        self.assertEqual(plist["Label"], "io.github.yuriimurha.grooviq.merge-train")
        self.assertEqual(plist["StartInterval"], 180)
        self.assertFalse(plist["RunAtLoad"])
        self.assertEqual(plist["StandardOutPath"], str(self.agent.state / "launchd.stderr.log"))
        self.assertEqual(plist["StandardErrorPath"], plist["StandardOutPath"])
        runner = self.agent.state / "versions" / revision / names[0]
        wrapper = Path(plist["ProgramArguments"][0])
        self.assertTrue(os.access(wrapper, os.X_OK))
        self.assertIn("-E -s", wrapper.read_text())
        self.assertTrue(self.agent.status()["pinned_python_exists"])
        for name in names:
            self.assertEqual(runner.with_name(name).read_bytes(), (CHECKOUT / name).read_bytes())
        self.assertEqual(self.launchctl.call_args_list[0].args[0][1], "bootstrap")

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

    def test_foreign_fetch_destination_is_rejected_with_canonical_push(self):
        self.publish({"merge_train.py": "print('must never run')\n"})
        foreign = self.root / "foreign.git"
        self.git(self.root, "clone", "--bare", self.origin, foreign)
        self.git(self.repo, "remote", "set-url", "--push", "origin", self.origin)
        self.git(self.repo, "remote", "set-url", "origin", foreign)
        self.agent.set_order(ORDER)
        self.assertEqual(self.agent.tick()["status"], "error")
        self.assertNotIn("must never run", self.agent.log.read_text())

    def test_failed_fetch_does_not_use_stale_main_and_counts_toward_streak(self):
        self.publish({"merge_train.py": "print('must never run')\n"})
        self.origin.rename(self.root / "offline.git")
        self.agent.set_order(ORDER)
        for expected in ("retry: network", "retry: network", "stopped"):
            self.assertEqual(self.agent.tick()["status"], expected)
        self.assertNotIn("must never run", self.agent.log.read_text())

    def test_wrapper_logs_missing_interpreter_and_caps_existing_log(self):
        names = ("merge_train_agent.py", "refresh_install.py")
        self.publish({name: (CHECKOUT / name).read_text() for name in names})
        self.agent.python = str(self.root / "missing-python")
        self.agent.enable()
        self.assertFalse(self.agent.status()["pinned_python_exists"])
        plist = plistlib.loads(self.agent.plist.read_bytes())
        log = self.agent.state / "launchd.stderr.log"
        log.write_bytes(b'x' * 100_000)
        with log.open("ab") as stream:
            result = subprocess.run(plist["ProgramArguments"], stdout=stream, stderr=stream)
        self.assertEqual(result.returncode, 127)
        self.assertLess(log.stat().st_size, 65_536)
        self.assertIn("pinned Python", log.read_text())


if __name__ == "__main__":
    result = unittest.main(exit=False).result
    print(f"Agent suite ran {result.testsRun} tests, skipped {len(result.skipped)}", flush=True)
    sys.exit(not (result.wasSuccessful() and result.testsRun and not result.skipped))
