#!/usr/bin/env python3
"""Schedule the protected Grooviq merge train from a local approved order."""
import argparse
from collections import deque
from contextlib import contextmanager
import datetime
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tempfile

from refresh_install import LockBusy, atomic_json, atomic_text, locked, safe_path

LABEL = "io.github.yuriimurha.grooviq.merge-train"
INTERVAL = 180
LOG_LINES = 2000
SNAPSHOT_FILES = ("merge_train_agent.py", "refresh_install.py")
ORIGINS = (
    "https://github.com/YuriiMurha/grooviq.git",
    "https://github.com/YuriiMurha/grooviq",
    "git@github.com:YuriiMurha/grooviq.git",
)
MAIN = "refs/remotes/origin/main"
ENTRY = re.compile(r"([1-9][0-9]*)=[a-f0-9]{40}")


def stamp():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def require_runtime():
    if sys.version_info < (3, 12):
        raise RuntimeError("Grooviq merge-train agent requires Python 3.12+; set REFRESH_PYTHON.")


def parse_order(raw):
    entries, seen = [], set()
    for number, line in enumerate(raw.splitlines(), 1):
        entry = line.partition("#")[0].strip()
        if not entry:
            continue
        match = ENTRY.fullmatch(entry)
        if match is None:
            raise ValueError(f"Order line {number}: expected PR=<full lowercase SHA>.")
        pr = match[1]
        if pr in seen:
            raise ValueError(f"Order line {number}: PR #{pr} is listed twice.")
        seen.add(pr)
        entries.append(entry)
    return entries


def read_text(path):
    try:
        return safe_path(path).read_bytes().decode("utf-8")
    except FileNotFoundError:
        return None


class Agent:
    def __init__(self, repo, home=None, *, launchctl=subprocess.run, origins=ORIGINS):
        self.repo = safe_path(repo)
        self.home = safe_path(home or Path.home())
        self.state = safe_path(self.home / "Library/Application Support/Grooviq Ops/merge-train")
        self.plist = safe_path(self.home / "Library/LaunchAgents" / (LABEL + ".plist"))
        self.order = self.state / "order"
        self.stopped = self.state / "stopped"
        self.log = self.state / "tick.log"
        self.disabled = self.state / "disabled"
        self.tick_lock = self.state / "tick.lock"
        self.control_lock = self.state / "control.lock"
        self.launchctl = launchctl
        self.origins = origins
        self.domain = f"gui/{os.getuid()}"
        self.python = str(Path(sys.executable).resolve())
        self.env = dict(os.environ, DEVELOPER_DIR="/Library/Developer/CommandLineTools",
                        PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
                        GIT_TERMINAL_PROMPT="0")
        # The working directory and its Git linkage select the repo for every child.
        for name in ("GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE",
                     "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES"):
            self.env.pop(name, None)

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.repo), *args], env=self.env,
                              check=True, capture_output=True, timeout=90).stdout

    def refresh_main(self):
        fetch = self.git("remote", "get-url", "origin").decode().strip()
        push = self.git("remote", "get-url", "--push", "--all", "origin").decode().splitlines()
        if fetch not in self.origins or not push or any(url not in self.origins for url in push):
            raise RuntimeError("Automatic merge train requires the canonical Grooviq origin.")
        self.git("fetch", "--no-tags", "--no-recurse-submodules", "--no-write-fetch-head",
                 "origin", f"+refs/heads/main:{MAIN}")
        revision = self.git("rev-parse", "--verify", f"{MAIN}^{{commit}}").decode().strip()
        if not re.fullmatch("[0-9a-f]{40}", revision):
            raise RuntimeError("Invalid fetched origin/main SHA.")
        return revision

    @contextmanager
    def checkout(self, revision):
        # The tick's flock and mismatch file live in the shared Git directory.
        with tempfile.TemporaryDirectory(prefix="tick-", dir=self.state) as scratch:
            root = Path(scratch)
            script = root / "merge_train.py"
            script.write_bytes(self.git("show", f"{revision}:scripts/merge_train.py"))
            worktree = root / "checkout"
            self.git("-c", "core.hooksPath=/dev/null", "worktree", "add", "--detach", "--quiet",
                     str(worktree), revision)
            try:
                yield worktree, script
            finally:
                self.git("worktree", "remove", str(worktree))

    def run_tick(self, worktree, script, entries):
        return subprocess.run([self.python, "-S", "-B", str(script), "--order", *entries, "--batch", "4"],
                              cwd=worktree, env=self.env, stdin=subprocess.DEVNULL,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              encoding="utf-8", errors="replace")

    def set_order(self, raw):
        entries = parse_order(raw)
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        with locked(self.control_lock):
            atomic_text(safe_path(self.order), raw)
        return {"status": "order saved", "entries": len(entries)}

    def status(self):
        stop = read_text(self.stopped)
        return {"enabled": self.plist.is_file() and not self.disabled.exists(),
                "order": read_text(self.order), "stopped": json.loads(stop) if stop else None,
                "log": str(self.log)}

    def resume(self):
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        with locked(self.tick_lock):
            safe_path(self.stopped).unlink(missing_ok=True)
        return {"status": "resumed"}

    def log_output(self, message, output=""):
        # launchd holds its own descriptors open. Rotate the captured file here.
        log = safe_path(self.log)
        lines = deque(maxlen=LOG_LINES)
        if log.exists():
            with log.open(encoding="utf-8") as stream:
                lines.extend(line.rstrip("\n") for line in stream)
        lines.append(f"{stamp()} {message}")
        lines.extend(line[:4096] for line in output.splitlines())
        atomic_text(log, "\n".join(lines) + "\n")

    def tick(self):
        require_runtime()
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        try:
            with locked(self.tick_lock):
                try:
                    return self._tick()
                except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
                    detail = getattr(error, "stderr", None)
                    if isinstance(detail, bytes):
                        detail = detail.decode("utf-8", "replace")
                    self.log_output(f"agent error: {error}", detail or "")
                    return {"status": "error", "error": str(error)}
        except LockBusy:
            return {"status": "busy"}

    def _tick(self):
        if safe_path(self.disabled).exists():
            return {"status": "disabled"}
        if safe_path(self.stopped).exists():
            return {"status": "latched"}
        raw = read_text(self.order)
        entries = parse_order(raw or "")
        if not entries:
            return {"status": "no order"}
        revision = self.refresh_main()
        with self.checkout(revision) as (worktree, script):
            result = self.run_tick(worktree, script, entries)
            if result.returncode == 1:
                atomic_json(safe_path(self.stopped), {"recorded_utc": stamp(), "output": result.stdout,
                            "order_sha256": hashlib.sha256(raw.encode("utf-8")).hexdigest()})
            self.log_output(f"source {revision}, tick exit {result.returncode}", result.stdout)
        status = {0: "completed", 1: "stopped"}.get(result.returncode, "error")
        return {"status": status, "exit_code": result.returncode, "log": str(self.log)}

    def launch(self, *args, check=True):
        return self.launchctl(["/bin/launchctl", *args], check=check, capture_output=True, timeout=15)

    def unload(self):
        service = f"{self.domain}/{LABEL}"
        result = self.launch("bootout", service, check=False)
        if result.returncode and self.launch("print", service, check=False).returncode == 0:
            result.check_returncode()

    def enable(self):
        require_runtime()
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        with locked(self.control_lock), locked(self.tick_lock):
            revision = self.refresh_main()
            payloads = {}
            for name in SNAPSHOT_FILES:
                path = safe_path(Path(__file__).with_name(name))
                raw = self.git("show", f"{revision}:scripts/{name}")
                if path.read_bytes() != raw:
                    raise RuntimeError(f"Enable requires exact reviewed merged source: {name}")
                payloads[name] = raw.decode("utf-8")
            common = Path(self.git("rev-parse", "--path-format=absolute", "--git-common-dir").decode().strip())
            if common.name != ".git" or not common.is_dir():
                raise RuntimeError("Automatic merge train requires a stable shared repository.")
            stable_repo = safe_path(common.parent)
            version = safe_path(self.state / "versions" / revision)
            version.mkdir(parents=True, exist_ok=True, mode=0o700)
            for name, source in payloads.items():
                atomic_text(safe_path(version / name), source)
            args = [self.python, "-S", "-B", str(version / SNAPSHOT_FILES[0]),
                    "--repo", str(stable_repo), "tick"]
            plist = plistlib.dumps({"Label": LABEL, "ProgramArguments": args, "StartInterval": INTERVAL,
                                    "RunAtLoad": False, "StandardOutPath": os.devnull,
                                    "StandardErrorPath": os.devnull}).decode("utf-8")
            atomic_text(safe_path(self.disabled), "registration in progress\n")
            self.plist.parent.mkdir(parents=True, exist_ok=True)
            if self.plist.exists():
                self.unload()
            atomic_text(safe_path(self.plist), plist)
            self.launch("bootstrap", self.domain, str(self.plist))
            self.disabled.unlink()
        return {"status": "enabled", "interval_seconds": INTERVAL, "source_sha": revision}

    def disable(self):
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        with locked(self.control_lock):
            atomic_text(safe_path(self.disabled), "disabled by operator\n")
            try:
                with locked(self.tick_lock):
                    if self.plist.exists():
                        self.unload()
                        self.plist.unlink()
            except LockBusy:
                # Bootout would signal the child doing the merge. Future runs skip.
                return {"status": "unload deferred"}
        return {"status": "disabled"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[1])
    commands = parser.add_subparsers(dest="command", required=True)
    for command in ("enable", "disable", "status", "resume", "tick"):
        commands.add_parser(command)
    order = commands.add_parser("set-order", help="read approved PR heads from stdin, one per line")
    order.add_argument("--make-order", help="read the whitespace-separated Make ORDER value instead")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("local merge-train agent requires macOS")
    try:
        require_runtime()
        agent = Agent(args.repo)
        if args.command == "set-order":
            raw = sys.stdin.read() if args.make_order is None else "".join(
                entry + "\n" for entry in args.make_order.split()
            )
            value = agent.set_order(raw)
        else:
            value = getattr(agent, args.command)()
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        parser.exit(1, f"Merge-train agent: {error}\n")
    print(json.dumps(value, indent=2))
    return int(value.get("status") in ("stopped", "error"))


if __name__ == "__main__":
    sys.exit(main())
