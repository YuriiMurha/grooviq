"""Reclaim idle Grooviq worktree build caches or check the repo volume."""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import shutil
import stat
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path


GIB = 1024**3
RELEASE_CACHE = Path("~/Library/Caches/Grooviq/quality-release").expanduser()
PROBE_TIMEOUT = 10
LSOF_TIMEOUT = 30


@dataclass(frozen=True)
class Worktree:
    path: Path
    head: str
    branch: str | None

    @classmethod
    def parse(cls, record):
        fields = dict(field.partition(" ")[::2] for field in record.split("\0"))
        return cls(
            Path(fields["worktree"]),
            fields.get("HEAD", ""),
            fields.get("branch"),
        )


def command(args, *, cwd=None, timeout=PROBE_TIMEOUT):
    return subprocess.run(
        args, cwd=cwd, timeout=timeout, capture_output=True, text=True,
        errors="surrogateescape",
    )


def checked(run, args, **kwargs):
    result = run(args, **kwargs)
    result.check_returncode()
    return result.stdout


def merge_skip_reason(run, repo, worktree, remote_url, remote_refs, main_commits):
    branch = worktree.branch.removeprefix("refs/heads/")
    if f"refs/remotes/origin/{branch}" in remote_refs:
        return "remote branch still exists"
    output = checked(
        run,
        ["gh", "pr", "list", "--repo", remote_url, "--state", "merged",
         "--head", branch, "--base", "main", "--json",
         "number,headRefOid,isCrossRepository,mergeCommit"],
        cwd=repo,
    )
    # The merge OID pins the squash commit even if the PR title was changed.
    for pr in json.loads(output):
        if pr["isCrossRepository"] or (pr["mergeCommit"] or {}).get("oid") not in main_commits:
            continue
        head = pr["headRefOid"]
        if not re.fullmatch(r"[0-9a-f]{40}", head or ""):
            continue
        probe = run(["git", "cat-file", "-t", head], cwd=repo)
        if probe.returncode != 0:
            number = pr["number"]
            if type(number) is not int or number < 1:
                continue
            checked(
                run,
                ["git", "fetch", "--no-tags", "--no-auto-maintenance", "--no-write-fetch-head",
                 "origin", f"refs/pull/{number}/head"],
                cwd=repo, timeout=60,
            )
        contains_local = run(["git", "merge-base", "--is-ancestor", worktree.head, head], cwd=repo)
        if contains_local.returncode == 0:
            return None
        if contains_local.returncode != 1:
            return "PR head ancestry unavailable"
    return "no merged PR containing the local tip"


def worktree_skip_reason(run, worktree):
    head, ref = checked(
        run, ["git", "rev-parse", "HEAD", "--symbolic-full-name", "HEAD"],
        cwd=worktree.path,
    ).splitlines()
    if head != worktree.head or ref != (worktree.branch or "HEAD"):
        return "worktree HEAD or branch changed"
    if worktree.branch is None and checked(
        run,
        ["git", "--no-optional-locks", "status", "--porcelain", "--ignore-submodules=none"],
        cwd=worktree.path,
    ):
        return "dirty detached worktree"
    return None


def recently_written(run, target, recent_minutes):
    cutoff = datetime.now(timezone.utc) - timedelta(minutes=recent_minutes)
    recent = checked(
        run,
        ["find", str(target), "-maxdepth", "3", "-newermt",
         cutoff.strftime("%Y-%m-%d %H:%M:%S UTC"), "-print", "-quit"],
    )
    return bool(recent)


def activity_snapshot(run, worktrees):
    try:
        result = run(["lsof", "-n", "-P", "-w", "-F0fn"], timeout=LSOF_TIMEOUT)
    except subprocess.TimeoutExpired:
        return {}, "lsof snapshot timed out"
    except OSError:
        return {}, "lsof snapshot unavailable"
    if result.returncode != 0 or result.stderr.strip():
        return {}, "lsof snapshot failed"
    if not result.stdout.startswith("p") or not result.stdout.rstrip("\n").endswith("\0"):
        return {}, "lsof snapshot empty or malformed"

    activity = {}
    descriptor = ""
    prefixes = tuple(str(path) for path in worktrees)
    # In NUL-delimited field mode, lsof separates process and file records with LF.
    for field in result.stdout.split("\0"):
        field = field.removeprefix("\n")
        if field.startswith("p"):
            descriptor = ""
        elif field.startswith("f"):
            descriptor = field[1:]
        elif field.startswith("n/") and field[1:].startswith(prefixes):
            path = Path(field[1:])
            reason = "process cwd" if descriptor == "cwd" else "open handle"
            for parent in (path, *path.parents):
                if parent in worktrees and (reason == "process cwd" or parent not in activity):
                    activity[parent] = reason

    try:
        processes = checked(run, ["ps", "-wwaxo", "command="])
    except (OSError, subprocess.SubprocessError):
        return activity, "process snapshot failed"
    if not processes.strip():
        return activity, "process snapshot empty"
    for path in worktrees:
        if str(path) in processes:
            activity.setdefault(path, "process command references worktree")
    return activity, None


def cache_identity(target, info=None):
    if info is None:
        info = target.lstat()
    if stat.S_ISDIR(info.st_mode) and target == target.resolve():
        return info.st_dev, info.st_ino
    return None


def measure_cache(target, device):
    size = target.lstat().st_blocks * 512
    pending = [target]
    while pending:
        with os.scandir(pending.pop()) as entries:
            for entry in entries:
                if entry.name == "node_modules":
                    return 0, "contains node_modules"
                info = entry.stat(follow_symlinks=False)
                if info.st_dev != device:
                    return 0, "contains a different filesystem"
                size += info.st_blocks * 512
                if stat.S_ISDIR(info.st_mode):
                    pending.append(entry.path)
    return size, None


def remove_cache(target, identity):
    parent_fd = os.open(target.parent, os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        info = os.stat(target.name, dir_fd=parent_fd, follow_symlinks=False)
        if (info.st_dev, info.st_ino) != identity:
            return "target changed before rename"
        renamed = f".target-reclaim-{os.getpid()}"
        os.mkdir(renamed, dir_fd=parent_fd)
        try:
            os.rename(target.name, renamed, src_dir_fd=parent_fd, dst_dir_fd=parent_fd)
        except OSError:
            os.rmdir(renamed, dir_fd=parent_fd)
            raise
        info = os.stat(renamed, dir_fd=parent_fd, follow_symlinks=False)
        if (info.st_dev, info.st_ino) != identity:
            raise OSError(f"target changed during rename; preserved {renamed}")
        shutil.rmtree(renamed, dir_fd=parent_fd)
    finally:
        os.close(parent_fd)
    return None


def protected_reason(target, main_path, release_cache, worktrees):
    if target.parent == main_path:
        return "main checkout"
    if target == release_cache or release_cache in target.parents or target in release_cache.parents:
        return "shared release cache"
    if "node_modules" in target.parts:
        return "under node_modules"
    if any(target == path or target in path.parents for path in worktrees):
        return "contains a registered worktree"
    if any(path / "target" in target.parent.parents or path / "target" == target.parent for path in worktrees):
        return "inside a registered worktree's target"
    return None


def reclaim_caches(
    repo, *, apply=False, recent_minutes=45, release_cache=RELEASE_CACHE, run=command,
):
    print(f"Mode: {'apply' if apply else 'dry run'}")
    print(f"Free before: {shutil.disk_usage(repo).free / GIB:.2f} GiB")
    total = 0
    status = 0
    skipped = Counter()
    try:
        if apply and not shutil.rmtree.avoids_symlink_attacks:
            raise OSError("Apply requires symlink-safe cache removal")

        def git(*args, **kwargs):
            return checked(run, ["git", *args], cwd=repo, **kwargs)

        git("fetch", "--prune", "--no-auto-maintenance", "origin",
            "+refs/heads/*:refs/remotes/origin/*", timeout=60)
        records = git("worktree", "list", "--porcelain", "-z").split("\0\0")
        worktrees = [Worktree.parse(record) for record in records if record]
        remote_url = git("remote", "get-url", "origin").strip()
        remote_refs = set(git("for-each-ref", "--format=%(refname)", "refs/remotes/origin").splitlines())
        main_commits = set(git("log", "origin/main", "--format=%H").splitlines())
        cache = release_cache.resolve()
        registered_paths = {worktree.path.resolve() for worktree in worktrees}
        activity, snapshot_error = activity_snapshot(run, registered_paths)

        def reclaim_target(worktree, target, info):
            reason = protected_reason(target, worktrees[0].path, cache, registered_paths)
            if reason:
                return 0, reason
            identity = cache_identity(target, info)
            if identity is None:
                return 0, "target is not a canonical directory"
            if identity[0] != target.parent.stat().st_dev:
                return 0, "target is on a different filesystem"
            reason = snapshot_error or activity.get(worktree.path) or worktree_skip_reason(run, worktree)
            if reason:
                return 0, reason
            if recently_written(run, target, recent_minutes):
                return 0, "recent write"
            if worktree.branch is not None:
                reason = merge_skip_reason(run, repo, worktree, remote_url, remote_refs, main_commits)
                if reason:
                    return 0, reason

            size, reason = measure_cache(target, identity[0])
            if reason:
                return 0, reason
            if apply:
                fresh, error = activity_snapshot(run, {worktree.path})
                if error or worktree.path in fresh:
                    return 0, error or fresh[worktree.path]
            reason = worktree_skip_reason(run, worktree)
            if reason:
                return 0, reason
            if recently_written(run, target, recent_minutes):
                return 0, "recent write"
            if cache_identity(target) != identity:
                return 0, "target changed"
            if apply:
                reason = remove_cache(target, identity)
                if reason:
                    return 0, reason
            return size, None

        for worktree in worktrees:
            target = worktree.path / "target"
            try:
                info = target.lstat()
            except FileNotFoundError:
                continue
            except OSError as error:
                print(f"Skipped {target}: target inspection failed ({error})")
                skipped["target inspection failed"] += 1
                status = 1
                continue
            try:
                size, reason = reclaim_target(worktree, target, info)
            except FileNotFoundError:
                size, reason = 0, "target or worktree disappeared"
            except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError) as error:
                print(f"Skipped {target}: probe or removal failed ({error})")
                skipped["probe or removal failed"] += 1
                status = 1
                continue
            if reason:
                print(f"Skipped {target}: {reason}")
                skipped[reason] += 1
                continue
            print(f"{'Reclaimed' if apply else 'Would reclaim'} {size / GIB:.2f} GiB: {target}")
            total += size
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        print(f"Warning: cache scan stopped: {error}")
        status = 1
    print(f"Skipped: {sum(skipped.values())} target(s)")
    for reason, count in sorted(skipped.items()):
        print(f"  {count} {reason}")
    print(f"Total {'reclaimed' if apply else 'reclaimable'}: {total / GIB:.2f} GiB")
    print(f"Free after: {shutil.disk_usage(repo).free / GIB:.2f} GiB")
    return status


def nonnegative_number(value):
    number = float(value)
    if not math.isfinite(number) or number < 0:
        raise argparse.ArgumentTypeError("expected a finite, non-negative number")
    return number


def positive_minutes(value):
    minutes = int(value)
    if minutes < 1:
        raise argparse.ArgumentTypeError("expected at least one minute")
    return minutes


def disk_check(repo, minimum):
    free = shutil.disk_usage(repo).free / GIB
    print(f"{free:.2f} GiB free on the volume holding {repo}")
    if free < minimum:
        print(f"Warning: free space is below {minimum:g} GiB")
        return 1
    return 0


def main():
    if sys.version_info < (3, 11):
        print(
            "Grooviq cache maintenance requires Python 3.11 or newer. "
            "Put a supported python3 on PATH.",
            file=sys.stderr,
        )
        return 1
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    disk = commands.add_parser("disk-check")
    disk.add_argument("--min-gib", type=nonnegative_number, default=20)
    reclaim = commands.add_parser("reclaim-caches")
    reclaim.add_argument("--apply", default="")
    reclaim.add_argument("--recent-minutes", type=positive_minutes, default=45)
    args = parser.parse_args()
    if args.command == "disk-check":
        return disk_check(Path.cwd(), args.min_gib)
    return reclaim_caches(Path.cwd(), apply=args.apply == "1", recent_minutes=args.recent_minutes)


if __name__ == "__main__":
    raise SystemExit(main())
