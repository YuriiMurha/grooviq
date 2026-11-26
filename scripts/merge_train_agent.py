#!/usr/bin/env python3
"""Schedule the protected Grooviq merge train from a local approved order."""
import argparse
from collections import deque
from contextlib import contextmanager
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time

from refresh_install import LockBusy, atomic_json, atomic_text, locked, safe_path

LABEL = "io.github.yuriimurha.grooviq.merge-train"
INTERVAL = 180
TICK_TIMEOUT = 600
TICK_GRACE = 120
LOG_LINES = 2000
TICK_HISTORY = 2000
TICK_LINE_BYTES = 65_536
LAUNCHD_LOG_BYTES = 65_536
REPO = "YuriiMurha/grooviq"
SNAPSHOT_FILES = ("merge_train_agent.py", "refresh_install.py", "merge_train.py")
HOMEBREW_OPT = (Path("/opt/homebrew/opt"), Path("/usr/local/opt"))
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
TESTED_BATCH = re.compile(r"Tested batch ([a-f0-9]{40})")
MERGED = re.compile(r"Merged #([1-9][0-9]*) at ([a-f0-9]{40})")
SQUASH_ATTEMPT = re.compile(r"Squash attempt #([1-9][0-9]*) ([a-f0-9]{40}) after ([a-f0-9]{40})")
REMOVED = re.compile(r"Removed #[1-9][0-9]* from the train\b")
STOP_SKIP = "skipped: STOP file"
STOP_INSTRUCTIONS = "\n".join((
    "touch ~/.grooviq-train/STOP",
    "make merge-train-agent-disable",
    "launchctl disable gui/$(id -u)/" + LABEL,
    "launchctl bootout gui/$(id -u)/" + LABEL,
))
WRITE_OUTPUT = re.compile(r"^(?:Merged|Pushed|Removed|Stopped at|Squash attempt)\b")
WRITE_ERROR = re.compile(
    r"\b(?:Git push|GitHub (?:POST|PUT|PATCH|DELETE)|Stopped at|Merged so far)\b", re.IGNORECASE
)
FETCH = ("fetch", "--no-tags", "--no-recurse-submodules", "--no-write-fetch-head")
GIT_COMMANDS = {
    ("remote", "get-url", "origin"),
    ("remote", "get-url", "--push", "--all", "origin"),
    (*FETCH, "origin", f"+{MAIN_REF}:{MAIN}"),
    ("rev-parse", "--verify", f"{MAIN}^{{commit}}"),
    ("rev-parse", "--path-format=absolute", "--git-common-dir"),
    ("ls-remote", "--refs", "origin", MAIN_REF, BATCH_REF),
    ("worktree", "list", "--porcelain", "-z"),
}
ADD_WORKTREE = ("worktree", "add", "--detach", "--no-checkout", "--quiet")
REMOVE_WORKTREE = ("worktree", "remove", "--force")
SCRIPT_OBJECT = re.compile(r"([a-f0-9]{40}):scripts/([^/]+)")
OBJECT_REF = re.compile(r"[a-f0-9]{40}\^(?:\{tree\})?")
CI_RUN_COMMAND = ("gh", "run", "list", "--repo", REPO, "--branch", BATCH_BRANCH,
                  "--workflow", "ci.yml", "--limit", "100", "--json", "databaseId,headSha")
NETWORK_FAILURE = re.compile(
    r"\bHTTP(?:/\d+(?:\.\d+)?)?\s+5\d\d\b|"
    r"failed to connect|error connecting|connection (?:error|refused|reset|timed out|closed|aborted)|"
    r"could not resolve host|network is unreachable|no such host|i/o timeout|TLS handshake timeout|"
    r"returned error: 5\d\d\b|operation timed out|ssl_error_syscall|ssl_connect|not closed cleanly|"
    r"early eof|unexpected disconnect|no route to host|host is down",
    re.IGNORECASE,
)

AUTH_FAILURE = re.compile(
    r"permission denied|authentication failed|could not read username|terminal prompts disabled|"
    r"(?:returned error:\s*|\bHTTP(?:/\d+(?:\.\d+)?)?\s+)(?:401|403)\b",
    re.IGNORECASE,
)
SOURCE_CHANGED = "merge-train scripts changed on main since enable; re-run make merge-train-agent-enable"


# The wrapper needs a system interpreter to report the loss of its pinned Python.
# It shares the normal journal's directory lock and atomic replacement.
MISSING_PYTHON_JOURNAL = """
use strict;
use warnings;
use Fcntl qw(LOCK_EX);
use File::Temp qw(tempfile);
use JSON::PP qw(decode_json encode_json);
use POSIX qw(strftime);

my ($state, $limit) = @ARGV;
my $path = "$state/ticks.jsonl";
die "Refusing symlinked tick journal" if -l $state || -l $path;
open(my $dir, '<', $state) or die $!;
flock($dir, LOCK_EX) or die $!;
my @rows;
if (-e $path) {
    open(my $journal, '<', $path) or die $!;
    @rows = <$journal>;
    close($journal) or die $!;
    my $last = @rows ? eval { decode_json($rows[-1]) } : undef;
    exit 0 if $last && ($last->{status} // '') eq 'error'
        && ($last->{reason}[0] // '') eq 'pinned python missing';
}
my ($stream, $temp) = tempfile('.ticks-XXXXXX', DIR => $state, UNLINK => 1);
push @rows, encode_json({recorded_utc => strftime('%Y-%m-%dT%H:%M:%SZ', gmtime),
                         status => 'error', exit_code => 127, reason => ['pinned python missing']}) . "\n";
splice(@rows, 0, @rows - $limit) if @rows > $limit;
print {$stream} @rows or die $!;
close($stream) or die $!;
rename($temp, $path) or die $!;
"""


class FetchFailed(RuntimeError):
    """The agent's read-only fetch did not complete."""


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


def read_tick_output(root):
    def read(name):
        try:
            return safe_path(root / name).read_text(encoding="utf-8", errors="replace")
        except FileNotFoundError:
            return ""

    return read("stdout"), read("stderr")


class Agent:
    def __init__(self, repo, home=None, *, launchctl=subprocess.run, origins=ORIGINS):
        self.repo = safe_path(repo)
        self.home = safe_path(home or Path.home())
        self.state = safe_path(self.home / "Library/Application Support/Grooviq Ops/merge-train")
        self.plist = safe_path(self.home / "Library/LaunchAgents" / (LABEL + ".plist"))
        self.order = self.state / "order"
        self.kill_switch = self.home / ".grooviq-train" / "STOP"
        self.stopped = self.state / "stopped"
        self.log = self.state / "tick.log"
        self.ticks = self.state / "ticks.jsonl"
        self.landings = self.state / "ledger.jsonl"
        self.launchd_log = self.state / "launchd.stderr.log"
        self.registration = self.state / "registration.json"
        self.disabled = self.state / "disabled"
        self.tick_lock = self.state / "tick.lock"
        self.control_lock = self.state / "control.lock"
        self.launchctl = launchctl
        self.origins = origins
        self.domain = f"gui/{os.getuid()}"
        executable = Path(sys.executable)
        self.python = str(executable if any(executable.is_relative_to(root) for root in HOMEBREW_OPT)
                          else executable.resolve())
        self.env = {name: value for name, value in os.environ.items()
                    if name in ("TMPDIR", "LANG", "USER", "SSH_AUTH_SOCK") or name.startswith("GH_")}
        self.env.update(HOME=str(self.home), DEVELOPER_DIR="/Library/Developer/CommandLineTools",
                        PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
                        GIT_TERMINAL_PROMPT="0")

    def git_allowed(self, args):
        # Validate caller arguments before adding trusted global Git options.
        if args in GIT_COMMANDS:
            return True
        if len(args) == 2 and args[0] in ("show", "rev-parse"):
            script = SCRIPT_OBJECT.fullmatch(args[1])
            return script is not None and script[2] in SNAPSHOT_FILES
        if len(args) == 3 and args[:2] == ("rev-parse", "--verify"):
            return OBJECT_REF.fullmatch(args[2]) is not None
        if len(args) == len(FETCH) + 3 and args[:-1] == (*FETCH, "--refmap=", "origin"):
            return SHA.fullmatch(args[-1]) is not None
        if len(args) == len(ADD_WORKTREE) + 2 and args[:-2] == ADD_WORKTREE and SHA.fullmatch(args[-1]):
            path = Path(args[-2])
        elif len(args) == len(REMOVE_WORKTREE) + 1 and args[:-1] == REMOVE_WORKTREE:
            path = Path(args[-1])
        else:
            return False
        return (path.name == "checkout" and path.parent.name.startswith("tick-")
                and path.parent.parent == self.state)

    def git(self, *args):
        if not self.git_allowed(args):
            raise RuntimeError("Git command is not allowed in the merge-train agent.")
        command = ["git", "--no-replace-objects", "-c", "core.hooksPath=/dev/null", "-C", str(self.repo)]
        return subprocess.run([*command, *args], env=self.env, check=True, capture_output=True, timeout=90).stdout

    def refresh_main(self):
        fetch = self.git("remote", "get-url", "origin").decode().strip()
        push = self.git("remote", "get-url", "--push", "--all", "origin").decode().splitlines()
        if fetch not in self.origins or not push or any(url not in self.origins for url in push):
            raise RuntimeError("Automatic merge train requires the canonical Grooviq origin.")
        try:
            self.git(*FETCH, "origin", f"+{MAIN_REF}:{MAIN}")
        except (OSError, subprocess.SubprocessError) as error:
            raise FetchFailed(f"Pre-train fetch failed: {error}") from error
        revision = self.git("rev-parse", "--verify", f"{MAIN}^{{commit}}").decode().strip()
        if not SHA.fullmatch(revision):
            raise RuntimeError("Invalid fetched origin/main SHA.")
        return revision

    def prune_checkouts(self):
        for field in self.git("worktree", "list", "--porcelain", "-z").split(b"\0"):
            if field.startswith(b"worktree "):
                path = Path(os.fsdecode(field.removeprefix(b"worktree ")))
                if (path.name == "checkout" and path.parent.name.startswith("tick-")
                        and path.parent.parent == self.state):
                    self.git(*REMOVE_WORKTREE, str(safe_path(path)))
        for root in self.state.glob("tick-*"):
            shutil.rmtree(safe_path(root))

    @contextmanager
    def checkout(self, revision):
        # The tick's flock and mismatch file live in the shared Git directory.
        root = Path(tempfile.mkdtemp(prefix="tick-", dir=self.state))
        script, worktree = root / "merge_train.py", root / "checkout"
        interrupted = False
        added = False
        try:
            source = f"{revision}:scripts/merge_train.py"
            blob = self.git("rev-parse", source).decode().strip()
            raw = self.git("show", source)
            # Git hashes a blob's type and byte count along with its contents.
            digest = hashlib.sha1(f"blob {len(raw)}\0".encode("ascii") + raw).hexdigest()
            if digest != blob:
                raise RuntimeError("Train script bytes do not match the verified blob.")
            script.write_bytes(raw)
            self.git(*ADD_WORKTREE, str(worktree), revision)
            added = True
            yield worktree, script
        except KeyboardInterrupt:
            # A foreground interrupt can leave the train running with our inherited lock.
            interrupted = True
            raise
        finally:
            if not interrupted:
                if added:
                    # An empty index from --no-checkout looks dirty to Git.
                    self.git(*REMOVE_WORKTREE, str(worktree))
                shutil.rmtree(root)

    def run_tick(self, worktree, script, entries, batch, *, lock_fd=None):
        deadline = time.monotonic() + TICK_TIMEOUT
        args = [self.python, "-I", "-u", "-S", "-B", str(script), "--order", *entries, "--batch", str(batch),
                "--agent-deadline", str(deadline)]
        root = script.parent
        timed_out = False
        # The train can keep writing even if the supervisor exits between squashes.
        with (root / "stdout").open("xb") as stdout_file, (root / "stderr").open("xb") as stderr_file:
            with subprocess.Popen(args, cwd=worktree, env=self.env, stdin=subprocess.DEVNULL,
                                  stdout=stdout_file, stderr=stderr_file, start_new_session=True,
                                  pass_fds=() if lock_fd is None else (lock_fd,)) as process:
                try:
                    process.wait(timeout=TICK_TIMEOUT + TICK_GRACE)
                except subprocess.TimeoutExpired:
                    timed_out = True
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    process.wait()
        stdout, stderr = read_tick_output(root)
        if timed_out:
            raise subprocess.TimeoutExpired(args, TICK_TIMEOUT + TICK_GRACE, output=stdout, stderr=stderr)
        result = process.returncode
        if result >= 0 and time.monotonic() >= deadline:
            result = 124
        return subprocess.CompletedProcess(args, result, stdout, stderr)

    def set_order(self, raw):
        entries, batch = parse_order(raw)
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        with locked(self.control_lock):
            atomic_text(safe_path(self.order), raw)
        return {"status": "order saved", "entries": len(entries), "batch": batch}

    def status(self):
        stop = read_text(self.stopped)
        service = self.launch("print", f"{self.domain}/{LABEL}", check=False)
        disabled = self.launch("print-disabled", self.domain, check=False)
        disabled_match = re.search(rf'"{re.escape(LABEL)}"\s*=>\s*(disabled|enabled|true|false)\b',
                                   os.fsdecode(disabled.stdout or b""))
        plist = safe_path(self.plist)
        registration = json.loads(read_text(self.registration) or "{}")
        python = registration.get("python")
        ticks = (read_text(self.ticks) or "").splitlines()
        last_tick = json.loads(ticks[-1]) if ticks else None
        age = None
        if last_tick:
            recorded = datetime.datetime.fromisoformat(last_tick["recorded_utc"])
            age = int((datetime.datetime.now(datetime.timezone.utc) - recorded).total_seconds())
        return {"enabled": plist.is_file() and not self.disabled.exists(),
                "stop_file_present": os.path.lexists(self.kill_switch),
                "launchd": {"loaded": service.returncode == 0,
                            "disabled": (disabled_match[1] in ("disabled", "true") if disabled_match
                                         else False if disabled.returncode == 0 else None),
                            "print": os.fsdecode(service.stdout or service.stderr or b""),
                            "print_disabled_exit": disabled.returncode},
                "source_sha": registration.get("source_sha"),
                "pinned_python": python, "pinned_python_exists": bool(python and Path(python).is_file() and os.access(python, os.X_OK)),
                "last_tick": last_tick, "last_tick_age_seconds": age,
                "last_tick_stale": age is not None and age > 2 * INTERVAL,
                "order": read_text(self.order), "stopped": json.loads(stop) if stop else None,
                "log": str(self.log), "launchd_log": str(self.launchd_log)}

    def resume(self):
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        with locked(self.tick_lock):
            stop = read_text(self.stopped)
            if stop is not None and json.loads(stop).get("pending"):
                raise RuntimeError("Previous tick is pending; run a tick to latch it first.")
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
        return self.commit_object(sha, "^{tree}")

    def commit_object(self, sha, suffix):
        args = ("rev-parse", "--verify", sha + suffix)
        try:
            return self.git(*args).decode().strip()
        except subprocess.CalledProcessError:
            self.git(*FETCH, "--refmap=", "origin", sha)
            return self.git(*args).decode().strip()

    def ci_run_id(self, sha):
        result = subprocess.run(
            CI_RUN_COMMAND,
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

    def reconcile_landing(self, stdout, stderr):
        # The last announced PUT may have reached GitHub even if its response was lost.
        lines = stdout.splitlines()
        attempt = next((match for line in reversed(lines) if (match := SQUASH_ATTEMPT.fullmatch(line))), None)
        if attempt is None or any(match[1] == attempt[1] for line in lines
                                  if (match := MERGED.fullmatch(line))):
            return stdout, stderr
        number, approved, parent = attempt.groups()
        try:
            result = subprocess.run(["gh", "api", "--method", "GET", f"repos/{REPO}/pulls/{number}"],
                                    cwd=self.repo, env=self.env, check=True, capture_output=True, timeout=30)
            pull = json.loads(result.stdout)
            landed = pull["merge_commit_sha"]
            if (pull["merged"] is True and SHA.fullmatch(landed or "") and pull["head"]["sha"] == approved
                    and pull["base"]["ref"] == "main" and self.commit_object(landed, "^") == parent):
                return "\n".join((*lines, f"Merged #{number} at {landed}", "")), stderr
            raise RuntimeError("GitHub did not yet confirm the approved squash and parent")
        except (OSError, RuntimeError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
            return stdout, f"{stderr.rstrip()}\nUnconfirmed squash of #{number}: {error}.\n".lstrip()

    def record_landing(self, record):
        merges = [match for line in record["stdout"] if (match := MERGED.fullmatch(line))]
        if not merges:
            return
        batches = [match[1] for line in record["stdout"] if (match := TESTED_BATCH.fullmatch(line))]
        tested = batches[-1] if batches else None
        landed = merges[-1][2]
        row = dict(recorded_utc=record["recorded_utc"], tested_batch=tested, landed_tip=landed,
                   landed_prefix=[[int(match[1]), match[2]] for match in merges],
                   landed_tree=None, tested_tree=None, match=None, ci_run_id=None, status="partial")
        for key, sha in (("landed_tree", landed), ("tested_tree", tested)):
            if sha is not None:
                try:
                    row[key] = self.commit_tree(sha)
                except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
                    row[f"{key}_error"] = str(error)
        if tested is not None:
            try:
                row["ci_run_id"] = self.ci_run_id(tested)
            except (OSError, RuntimeError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
                row["ci_error"] = str(error)
        if row["landed_tree"] and row["tested_tree"]:
            row["match"] = row["landed_tree"] == row["tested_tree"]
            output = record["stdout"] + record["stderr"]
            partial = record.get("recovered_pending") or record.get("timed_out") or record["exit_code"] < 0 or any(
                "Stopped at #" in line or line.startswith(("Recovered landed prefix", "Unconfirmed squash"))
                for line in output
            )
            if not partial:
                if row["match"] and row["ci_run_id"] is not None:
                    row["status"] = "complete"
                elif not row["match"]:
                    row["status"] = "mismatch"
        with safe_path(self.landings).open("a", encoding="utf-8") as stream:
            stream.write(json.dumps(row) + "\n")
            stream.flush()
            os.fsync(stream.fileno())

    def append_tick(self, record):
        path = safe_path(self.ticks)
        # STOP skips the tick lock. Serialize journal updates independently without a lock file.
        directory = os.open(self.state, os.O_RDONLY)
        try:
            fcntl.flock(directory, fcntl.LOCK_EX)
            lines = deque(maxlen=TICK_HISTORY)
            if path.exists():
                with path.open(encoding="utf-8") as stream:
                    lines.extend(stream)
            if record["status"] == STOP_SKIP and lines and json.loads(lines[-1])["status"] == STOP_SKIP:
                return
            line = json.dumps(record) + "\n"
            if len(line.encode("utf-8")) > TICK_LINE_BYTES:
                bounded = {key: record[key] for key in ("recorded_utc", "status", "exit_code",
                           "network_streak", "order_sha256", "timed_out", "recovered_pending",
                           "before", "after") if key in record}
                if "reason" in record:
                    bounded["reason"] = [line[:384] for line in record["reason"][:20]]
                if "agent_error" in record:
                    bounded["agent_error"] = record["agent_error"][:1024]
                bounded["output_truncated"] = True
                line = json.dumps(bounded) + "\n"
                while len(line.encode("utf-8")) > TICK_LINE_BYTES and bounded.get("reason"):
                    bounded["reason"].pop()
                    line = json.dumps(bounded) + "\n"
            lines.append(line)
            atomic_text(path, "".join(lines))
        finally:
            os.close(directory)

    def network_streak(self):
        for line in reversed((read_text(self.ticks) or "").splitlines()):
            previous = json.loads(line)
            if "network_streak" in previous:
                return 1 if previous["status"] == "stopped" else previous["network_streak"] + 1
        return 1

    def latch(self, record, output, reason, *, pending=False, scratch=None):
        tail = "".join(output.splitlines(keepends=True)[-200:]).encode("utf-8")[-65_536:]
        stop = {"recorded_utc": stamp(), "output": tail.decode("utf-8", errors="ignore"),
                "order_sha256": record.get("order_sha256"), "reason": reason, "pending": pending}
        if scratch is not None:
            stop["scratch"] = scratch
        atomic_json(safe_path(self.stopped), stop)

    def ledger(self):
        rows = []
        for line in (read_text(self.landings) or "").splitlines():
            row = json.loads(line)
            run = row["ci_run_id"]
            url = f"https://github.com/{REPO}/actions/runs/{run}" if run else "—"
            match = {True: "yes", False: "no", None: "—"}[row["match"]]
            rows.append(f"| {row['tested_batch'] or '—'} | {url} | {row['landed_tip']} | "
                        f"{row['landed_tree'] or '—'} | {row['tested_tree'] or '—'} | "
                        f"{match} | {row['status']} |")
        return rows

    def tick(self, *, show_output=False):
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        if os.path.lexists(self.kill_switch):
            self.append_tick({"recorded_utc": stamp(), "exit_code": 0, "status": STOP_SKIP})
            return {"status": STOP_SKIP, "exit_code": 0}
        require_runtime()
        try:
            with locked(self.tick_lock) as lock:
                record = {}
                try:
                    outcome = self._tick(record, show_output, lock.fileno())
                    if record:
                        self.append_tick(record)
                    if outcome["status"] == "stopped":
                        outcome["reason"] = [line[:384] for line in record["reason"][:20]]
                    if outcome["status"] in ("completed", "skipped: busy", "retry: network",
                                              STOP_SKIP):
                        safe_path(self.stopped).unlink(missing_ok=True)
                    return outcome
                except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
                    detail = getattr(error, "stderr", None)
                    if isinstance(detail, bytes):
                        detail = detail.decode("utf-8", "replace")
                    reason = (detail or str(error)).splitlines()[-20:]
                    saved = read_text(self.stopped)
                    if saved is None or json.loads(saved).get("pending"):
                        self.latch(record, detail or str(error), reason)
                    record.setdefault("recorded_utc", stamp())
                    record.setdefault("exit_code", getattr(error, "returncode", 1))
                    record.setdefault("reason", reason)
                    record.update(status="error", agent_error=str(error), network_streak=0)
                    self.log_output(f"agent error: {error}", detail or "")
                    self.append_tick(record)
                    return {"status": "error", "error": str(error)}
        except LockBusy:
            return {"status": "busy"}

    def _tick(self, record, show_output, lock_fd):
        stop = read_text(self.stopped)
        if stop is not None:
            saved = json.loads(stop)
            if saved.get("pending"):
                stdout, stderr = "", ""
                scratch = saved.get("scratch")
                if scratch and scratch.startswith("tick-") and Path(scratch).name == scratch:
                    stdout, stderr = read_tick_output(self.state / scratch)
                    stdout, stderr = self.reconcile_landing(stdout, stderr)
                reason = ["previous tick did not finish"]
                self.latch(saved, saved["output"] + stdout + stderr, reason)
                record.update(recorded_utc=stamp(), exit_code=1, status="stopped", network_streak=0,
                              reason=reason, recovered_pending=True,
                              stdout=stdout.splitlines(), stderr=stderr.splitlines())
                self.record_landing(record)
                self.log_output("orphaned tick output", stdout + stderr)
            self.prune_checkouts()
            return {"status": "latched"}
        self.prune_checkouts()
        if safe_path(self.disabled).exists():
            return {"status": "disabled"}
        raw = read_text(self.order)
        entries, batch = parse_order(raw or "")
        if not entries:
            return {"status": "no order"}
        record.update(recorded_utc=stamp(), order_sha256=hashlib.sha256(raw.encode("utf-8")).hexdigest())
        try:
            revision = self.refresh_main()
        except FetchFailed as error:
            cause = error.__cause__
            detail = os.fsdecode(getattr(cause, "stderr", "") or str(error))
            # This fetch has made no train writes. Bound retries for unknown transport errors too.
            network = not AUTH_FAILURE.search(detail)
            status = self.record_result(record, "pre-train fetch", 1, "", detail, network=network)
            return {"status": status, "exit_code": 1, "log": str(self.log)}
        try:
            self.verify_sources(revision)
        except RuntimeError as error:
            status = self.record_result(record, f"source {revision}", 1, "", str(error))
            return {"status": status, "exit_code": 1, "log": str(self.log)}
        self.record_tips(record, "before")
        with self.checkout(revision) as (worktree, script):
            if os.path.lexists(self.kill_switch):
                record.update(status=STOP_SKIP, exit_code=0)
                return {"status": STOP_SKIP, "exit_code": 0}
            self.latch(record, "", ["tick in progress"], pending=True, scratch=script.parent.name)
            if os.path.lexists(self.kill_switch):
                record.update(status=STOP_SKIP, exit_code=0)
                return {"status": STOP_SKIP, "exit_code": 0}
            try:
                # Keep the scheduler lock held until an orphaned train exits.
                result = self.run_tick(worktree, script, entries, batch, lock_fd=lock_fd)
            except subprocess.TimeoutExpired as error:
                result = subprocess.CompletedProcess(error.cmd, 124, error.stdout, error.stderr)
            stdout, stderr = self.reconcile_landing(result.stdout or "", result.stderr or "")
            if show_output:
                print(stdout, end="", flush=True)
                print(stderr, end="", file=sys.stderr, flush=True)
            code = result.returncode
            stdout_lines, stderr_lines = stdout.splitlines(), stderr.splitlines()
            network = (code == 1 and NETWORK_FAILURE.search(stdout + "\n" + stderr)
                       and not any(WRITE_OUTPUT.match(line) for line in stdout_lines + stderr_lines)
                       and not WRITE_ERROR.search(stderr))
            status = self.record_result(record, f"source {revision}", code, stdout, stderr,
                                        network=network, timed_out=code == 124)
            self.record_tips(record, "after")
            self.record_landing(record)
        return {"status": status, "exit_code": record["exit_code"], "log": str(self.log)}

    def record_result(self, record, source, code, stdout, stderr, *, network=False, timed_out=False):
        stdout_lines, stderr_lines = stdout.splitlines(), stderr.splitlines()
        streak = self.network_streak() if network else 0
        status = {0: "completed", os.EX_TEMPFAIL: "skipped: busy"}.get(code, "stopped")
        reason = (stderr_lines or stdout_lines)[-20:]
        if code == 0:
            anomalies = [line for line in stdout_lines + stderr_lines if "BYPASS" in line]
            anomalies.extend(line for line in stdout_lines if REMOVED.match(line))
            if anomalies:
                status, code, reason = "stopped", 1, anomalies[-20:]
        if timed_out:
            reason.insert(0, f"Tick exceeded the {TICK_TIMEOUT}-second wall-clock limit.")
        if 0 < streak < 3:
            status = "retry: network"
        if streak >= 3:
            reason.insert(0, "Three consecutive ticks reported a network or HTTP 5xx failure.")
        if status == "stopped":
            reason = reason or [f"Train exited {code}."]
            self.latch(record, stdout + stderr, reason)
            record["reason"] = reason
        record.update(exit_code=code, status=status, timed_out=timed_out, stdout=stdout_lines, stderr=stderr_lines)
        if code != os.EX_TEMPFAIL:
            record["network_streak"] = streak
        self.log_output(f"{source}, tick exit {code} ({status})", stdout + stderr)
        return status

    def launch(self, *args, check=True):
        return self.launchctl(["/bin/launchctl", *args], check=check, capture_output=True, timeout=15)

    def unload(self):
        service = f"{self.domain}/{LABEL}"
        result = self.launch("bootout", service, check=False)
        if result.returncode and self.launch("print", service, check=False).returncode == 0:
            result.check_returncode()

    def script_blobs(self, revision):
        return {name: self.git("rev-parse", f"{revision}:scripts/{name}").decode().strip()
                for name in SNAPSHOT_FILES}

    def verify_sources(self, revision):
        registration = json.loads(read_text(self.registration) or "{}")
        registered = self.plist.is_file() or bool(registration)
        if registered:
            try:
                blobs = self.script_blobs(revision)
            except subprocess.CalledProcessError as error:
                raise RuntimeError(SOURCE_CHANGED) from error
            if blobs != registration.get("script_blobs"):
                raise RuntimeError(SOURCE_CHANGED)
        for name in SNAPSHOT_FILES[:2] if registered else SNAPSHOT_FILES:
            raw = self.git("show", f"{revision}:scripts/{name}")
            if Path(__file__).with_name(name).read_bytes() != raw:
                raise RuntimeError(f"Tick requires exact reviewed merged source: {name}")

    def enable(self):
        require_runtime()
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        common = Path(self.git("rev-parse", "--path-format=absolute", "--git-common-dir").decode().strip())
        if common.name != ".git" or not common.is_dir():
            raise RuntimeError("Automatic merge train requires a stable shared repository.")
        stable_repo = safe_path(common.parent)
        with locked(self.control_lock), locked(self.tick_lock), locked(common / "merge-train.lock"):
            stop = read_text(self.stopped)
            entries, _ = parse_order(read_text(self.order) or "")
            revision = self.refresh_main()
            outcome = {"status": "enabled", "interval_seconds": INTERVAL, "source_sha": revision,
                       "order_entries": entries}
            if stop is not None:
                if json.loads(stop).get("pending"):
                    outcome["next_step"] = ("Run make merge-train-agent-tick to recover the pending tick. "
                                            "Investigate the stop, then run make merge-train-agent-resume.")
                else:
                    outcome["next_step"] = ("The agent is still latched. Investigate the stop, "
                                            "then run make merge-train-agent-resume.")
            payloads = {}
            for name in SNAPSHOT_FILES:
                path = safe_path(Path(__file__).with_name(name))
                raw = self.git("show", f"{revision}:scripts/{name}")
                if path.read_bytes() != raw:
                    raise RuntimeError(f"Enable requires exact reviewed merged source: {name}")
                payloads[name] = raw.decode("utf-8")
            blobs = self.script_blobs(revision)
            version = safe_path(self.state / "versions" / revision)
            version.mkdir(parents=True, exist_ok=True, mode=0o700)
            for name, source in payloads.items():
                atomic_text(safe_path(version / name), source)
            (self.home / ".grooviq-train").mkdir(parents=True, exist_ok=True, mode=0o700)
            wrapper = safe_path(version / "launchd-tick")
            log = safe_path(self.launchd_log)
            args = [self.python, "-E", "-s", "-S", "-B", str(version / SNAPSHOT_FILES[0]),
                    "--repo", str(stable_repo), "tick", "--quiet"]
            script = ("#!/bin/sh\n"
                      f"log={shlex.quote(str(log))}\n"
                      'if [ -L "$log" ]; then echo "Refusing symlinked launchd log." >&2; exit 1; fi\n'
                      'size=$(/usr/bin/wc -c 2>/dev/null < "$log") || size=0\n'
                      f'if [ "$size" -ge {LAUNCHD_LOG_BYTES} ]; then : > "$log"; fi\n'
                      f"if [ ! -f {shlex.quote(self.python)} ] || [ ! -x {shlex.quote(self.python)} ]; then\n"
                      '  echo "Grooviq merge train needs its pinned Python 3.12+; reinstall Python and re-enable." >&2\n'
                      f"  /usr/bin/env -i /usr/bin/perl -f - {shlex.quote(str(self.state))} {TICK_HISTORY}"
                      " <<'GROOVIQ_MISSING_PYTHON'\n"
                      f"{MISSING_PYTHON_JOURNAL}GROOVIQ_MISSING_PYTHON\n"
                      "  exit 127\nfi\n"
                      f"exec {shlex.join(args)}\n")
            atomic_text(wrapper, script)
            wrapper.chmod(0o755)
            plist = plistlib.dumps({"Label": LABEL, "ProgramArguments": [str(wrapper)],
                                    "StartInterval": INTERVAL,
                                    "RunAtLoad": False, "StandardOutPath": str(log),
                                    "StandardErrorPath": str(log)}).decode("utf-8")
            atomic_text(safe_path(self.disabled), "registration in progress\n")
            self.plist.parent.mkdir(parents=True, exist_ok=True)
            if self.plist.exists():
                self.unload()
            atomic_text(safe_path(self.plist), plist)
            atomic_json(safe_path(self.registration),
                        {"python": self.python, "source_sha": revision, "script_blobs": blobs})
            self.launch("enable", f"{self.domain}/{LABEL}")
            self.launch("bootstrap", self.domain, str(self.plist))
            self.disabled.unlink()
        return outcome

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
                # Bootout interrupts the supervisor. Let the in-flight train finish.
                return {"status": "unload deferred",
                        "reason": "Disable recorded; re-run make merge-train-agent-disable after the active tick ends."}
        return {"status": "disabled"}


class AgentHelp(argparse.ArgumentParser):
    def format_help(self):
        return f"{STOP_INSTRUCTIONS}\n\n{super().format_help()}"


def main():
    parser = AgentHelp(description=__doc__,
                                    formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[1])
    commands = parser.add_subparsers(dest="command", required=True)
    for command in ("enable", "disable", "status", "resume", "ledger"):
        commands.add_parser(command)
    commands.add_parser("tick").add_argument("--quiet", action="store_true")
    order = commands.add_parser("set-order", help="read approved PR heads from stdin, one per line")
    order.add_argument("--make-order", help="read the whitespace-separated Make ORDER value instead")
    order.add_argument("--batch", help="write the Make BATCH value with --make-order")
    args = parser.parse_args()
    if args.command == "status":
        print(STOP_INSTRUCTIONS, flush=True)
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
        elif args.command == "tick":
            value = agent.tick(show_output=not args.quiet)
        else:
            value = getattr(agent, args.command)()
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        parser.exit(1, f"Merge-train agent: {str(error)[:2048]}\n")
    if args.command == "ledger":
        for row in value:
            print(row)
        return 0
    if args.command != "tick" or not args.quiet or value["status"] in ("stopped", "error"):
        print(json.dumps(value, indent=2)[:4096] if args.command == "tick" and args.quiet
              else json.dumps(value, indent=2))
    return int(value.get("status") in ("stopped", "error"))


if __name__ == "__main__":
    sys.exit(main())
