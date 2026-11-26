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
TICK_HISTORY = 2000
REPO = "YuriiMurha/grooviq"
SNAPSHOT_FILES = ("merge_train_agent.py", "refresh_install.py")
ORIGINS = (
    "https://github.com/YuriiMurha/grooviq.git",
    "https://github.com/YuriiMurha/grooviq",
    "git@github.com:YuriiMurha/grooviq.git",
)
MAIN = "refs/remotes/origin/main"
MAIN_REF = "refs/heads/main"
BATCH_BRANCH = "merge-train/batch"
BATCH_REF = f"refs/heads/{BATCH_BRANCH}"
SHA = re.compile(r"[a-f0-9]{40}")
ENTRY = re.compile(r"([1-9][0-9]*)=[a-f0-9]{40}")
BATCH = re.compile(r"batch=([1-9]|10)")
MERGED = re.compile(r"Merged #([1-9][0-9]*) at [a-f0-9]{40}")


def stamp():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def require_runtime():
    if sys.version_info < (3, 12):
        raise RuntimeError("Grooviq merge-train agent requires Python 3.12+; set REFRESH_PYTHON.")


def parse_order(raw):
    entries, seen = [], set()
    batch = None
    for number, line in enumerate(raw.splitlines(), 1):
        entry = line.partition("#")[0].strip()
        if not entry:
            continue
        if entry.startswith("batch="):
            match = BATCH.fullmatch(entry)
            if match is None or batch is not None:
                raise ValueError(f"Order line {number}: expected at most one batch=<1..10>.")
            batch = int(match[1])
            continue
        match = ENTRY.fullmatch(entry)
        if match is None:
            raise ValueError(f"Order line {number}: expected PR=<full lowercase SHA>.")
        pr = match[1]
        if pr in seen:
            raise ValueError(f"Order line {number}: PR #{pr} is listed twice.")
        seen.add(pr)
        entries.append(entry)
    return entries, 4 if batch is None else batch


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
        self.ticks = self.state / "ticks.jsonl"
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
                 "origin", f"+{MAIN_REF}:{MAIN}")
        revision = self.git("rev-parse", "--verify", f"{MAIN}^{{commit}}").decode().strip()
        if not SHA.fullmatch(revision):
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

    def run_tick(self, worktree, script, entries, batch):
        return subprocess.run([self.python, "-S", "-B", str(script), "--order", *entries, "--batch", str(batch)],
                              cwd=worktree, env=self.env, stdin=subprocess.DEVNULL,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              encoding="utf-8", errors="replace")

    def set_order(self, raw):
        entries, batch = parse_order(raw)
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        with locked(self.control_lock):
            atomic_text(safe_path(self.order), raw)
        return {"status": "order saved", "entries": len(entries), "batch": batch}

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

    def remote_tips(self):
        lines = self.git("ls-remote", "--refs", "origin", MAIN_REF, BATCH_REF).decode().splitlines()
        refs = {ref: sha for sha, ref in map(str.split, lines)}
        main, batch = refs.get(MAIN_REF), refs.get(BATCH_REF)
        if not SHA.fullmatch(main or "") or (batch is not None and not SHA.fullmatch(batch)):
            raise RuntimeError("Invalid remote merge-train refs.")
        return {"main": main, "batch": batch}

    def commit_tree(self, sha):
        args = ("rev-parse", "--verify", f"{sha}^{{tree}}")
        try:
            return self.git(*args).decode().strip()
        except subprocess.CalledProcessError:
            self.git("fetch", "--no-tags", "--no-recurse-submodules", "--no-write-fetch-head",
                     "--refmap=", "origin", sha)
            return self.git(*args).decode().strip()

    def ci_run_id(self, sha):
        result = subprocess.run(
            ["gh", "run", "list", "--repo", REPO, "--branch", BATCH_BRANCH,
             "--workflow", "ci.yml", "--limit", "100", "--json", "databaseId,headSha"],
            cwd=self.repo, env=self.env, check=True, capture_output=True, timeout=30,
        )
        for run in json.loads(result.stdout):
            if run["headSha"] == sha:
                return run["databaseId"]
        raise RuntimeError(f"No ci run found for tested batch {sha}.")

    def record_tips(self, record, phase):
        record[phase] = None
        try:
            record[phase] = self.remote_tips()
        except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
            record[f"{phase}_error"] = str(error)

    def record_landing(self, record):
        before, after = record["before"], record["after"]
        if before is None or after is None or before["main"] == after["main"]:
            return
        prs = [match[1] for line in record["stdout"] if (match := MERGED.fullmatch(line))]
        if not prs:
            return
        tested = before["batch"]
        record.update(tested_batch=tested, landed_main=after["main"], prs=prs,
                      tree=None, tree_matches_tested_batch=None, ci_run_id=None)
        try:
            record["tree"] = self.commit_tree(after["main"])
            if tested is None:
                raise RuntimeError("No batch ref was visible before the tick.")
            record["tree_matches_tested_batch"] = record["tree"] == self.commit_tree(tested)
        except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
            record["tree_error"] = str(error)
        if tested is not None:
            try:
                record["ci_run_id"] = self.ci_run_id(tested)
            except (OSError, RuntimeError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
                record["ci_error"] = str(error)

    def append_tick(self, record):
        path = safe_path(self.ticks)
        lines = deque(maxlen=TICK_HISTORY)
        if path.exists():
            with path.open(encoding="utf-8") as stream:
                lines.extend(stream)
        lines.append(json.dumps(record) + "\n")
        atomic_text(path, "".join(lines))

    def ledger(self):
        rows = []
        for line in (read_text(self.ticks) or "").splitlines():
            record = json.loads(line)
            if "landed_main" in record:
                run = record["ci_run_id"]
                url = f"https://github.com/{REPO}/actions/runs/{run}" if run else "—"
                prs = ", ".join(f"#{pr}" for pr in record["prs"])
                rows.append(f"| {record['tested_batch'] or '—'} | {url} | {record['landed_main']} | "
                            f"{record['tree'] or '—'} | {prs} |")
        return rows

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
        entries, batch = parse_order(raw or "")
        if not entries:
            return {"status": "no order"}
        revision = self.refresh_main()
        with self.checkout(revision) as (worktree, script):
            record = {"recorded_utc": stamp()}
            self.record_tips(record, "before")
            result = self.run_tick(worktree, script, entries, batch)
            status = {0: "completed", 1: "stopped", os.EX_TEMPFAIL: "skipped: busy"}.get(
                result.returncode, "error"
            )
            if result.returncode == 1:
                atomic_json(safe_path(self.stopped), {"recorded_utc": stamp(), "output": result.stdout,
                            "order_sha256": hashlib.sha256(raw.encode("utf-8")).hexdigest()})
            record.update(exit_code=result.returncode, stdout=result.stdout.splitlines())
            self.record_tips(record, "after")
            self.record_landing(record)
            self.append_tick(record)
            self.log_output(f"source {revision}, tick exit {result.returncode} ({status})", result.stdout)
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
    for command in ("enable", "disable", "status", "resume", "tick", "ledger"):
        commands.add_parser(command)
    order = commands.add_parser("set-order", help="read approved PR heads from stdin, one per line")
    order.add_argument("--make-order", help="read the whitespace-separated Make ORDER value instead")
    order.add_argument("--batch", help="write the Make BATCH value with --make-order")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("local merge-train agent requires macOS")
    try:
        require_runtime()
        agent = Agent(args.repo)
        if args.command == "set-order":
            if args.make_order is None:
                raw = sys.stdin.read()
            else:
                if args.batch is not None and not BATCH.fullmatch(f"batch={args.batch}"):
                    raise ValueError("Expected BATCH in 1..10.")
                raw = f"batch={args.batch}\n" if args.batch is not None else ""
                raw += "".join(entry + "\n" for entry in args.make_order.split())
            value = agent.set_order(raw)
        else:
            value = getattr(agent, args.command)()
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        parser.exit(1, f"Merge-train agent: {error}\n")
    if args.command == "ledger":
        for row in value:
            print(row)
        return 0
    print(json.dumps(value, indent=2))
    return int(value.get("status") in ("stopped", "error"))


if __name__ == "__main__":
    sys.exit(main())
