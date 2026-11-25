"""Reclaim idle Grooviq worktree build caches or check the repo volume."""

from __future__ import annotations

import argparse
import json
import math
import os
import shutil
import stat
import subprocess
import sys
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path


GIB = 1024**3
RELEASE_CACHE = Path("~/Library/Caches/Grooviq/quality-release").expanduser()
PROBE_TIMEOUT = 10
LSOF_TIMEOUT = 5


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


def merged_worktree(run, repo, worktree, remote_url, remote_refs, main_commits):
    branch = worktree.branch.removeprefix("refs/heads/")
    if f"refs/remotes/origin/{branch}" in remote_refs:
        return False
    output = checked(
        run,
        ["gh", "pr", "list", "--repo", remote_url, "--state", "merged",
         "--head", branch, "--base", "main", "--json",
         "headRefOid,isCrossRepository,mergeCommit"],
        cwd=repo,
    )
    # The merge OID pins the actual squash subject on main even if the PR is renamed.
    return any(
        pr["headRefOid"] == worktree.head
        and not pr["isCrossRepository"]
        and main_commits.get((pr["mergeCommit"] or {}).get("oid"))
        for pr in json.loads(output)
    )


def same_worktree(run, worktree):
    head, ref = checked(
        run, ["git", "rev-parse", "HEAD", "--symbolic-full-name", "HEAD"],
        cwd=worktree.path,
    ).splitlines()
    if head != worktree.head or ref != (worktree.branch or "HEAD"):
        return False
    if worktree.branch is not None:
        return True
    return not checked(
        run,
        ["git", "--no-optional-locks", "status", "--porcelain", "--ignore-submodules=none"],
        cwd=worktree.path,
    )


def recently_written(run, target, recent_minutes):
    cutoff = datetime.now(timezone.utc) - timedelta(minutes=recent_minutes)
    recent = checked(
        run,
        ["find", str(target), "-maxdepth", "3", "-newermt",
         cutoff.strftime("%Y-%m-%d %H:%M:%S UTC"), "-print", "-quit"],
    )
    return bool(recent)


def idle(run, target, recent_minutes):
    processes = checked(run, ["ps", "-wwaxo", "command="])
    if (
        not processes or str(target.parent) in processes
        or recently_written(run, target, recent_minutes)
    ):
        return False
    try:
        handles = run(["lsof", "-t", "+D", str(target)], timeout=LSOF_TIMEOUT)
    except (OSError, subprocess.TimeoutExpired):
        return False
    # lsof returns 1 with no diagnostics when there are no matches.
    return (
        handles.returncode == 1 and not handles.stdout and not handles.stderr
        and not recently_written(run, target, recent_minutes)
    )


def cache_identity(target):
    info = target.lstat()
    if stat.S_ISDIR(info.st_mode) and target == target.resolve():
        return info.st_dev, info.st_ino
    return None


def contains_protected_dir(target, device):
    pending = [target]
    while pending:
        with os.scandir(pending.pop()) as entries:
            for entry in entries:
                if entry.name == "node_modules":
                    return True
                if entry.is_dir(follow_symlinks=False):
                    if entry.stat(follow_symlinks=False).st_dev != device:
                        return True
                    pending.append(entry.path)
    return False


def reclaim_caches(
    repo, *, apply=False, recent_minutes=45, release_cache=RELEASE_CACHE, run=command,
):
    print(f"Mode: {'apply' if apply else 'dry run'}")
    before = shutil.disk_usage(repo).free
    print(f"Free before: {before / GIB:.2f} GiB")
    total = 0
    status = 0
    try:
        def git(*args, **kwargs):
            return checked(run, ["git", *args], cwd=repo, **kwargs)

        git("fetch", "--prune", "--no-auto-maintenance", "origin",
            "+refs/heads/*:refs/remotes/origin/*", timeout=60)
        records = git("worktree", "list", "--porcelain", "-z").split("\0\0")
        worktrees = [Worktree.parse(record) for record in records if record]
        remote_url = git("remote", "get-url", "origin").strip()
        remote_refs = set(git("for-each-ref", "--format=%(refname)", "refs/remotes/origin").splitlines())
        main_commits = dict(
            line.split("\t", 1)
            for line in git("log", "origin/main", "--format=%H%x09%s").split("\n")
            if line
        )
        cache = release_cache.resolve()
        registered_paths = {worktree.path.resolve() for worktree in worktrees}

        if apply and not shutil.rmtree.avoids_symlink_attacks:
            raise OSError("Apply requires symlink-safe cache removal")

        for worktree in worktrees[1:]:
            target = worktree.path / "target"
            if (
                target == cache or cache in target.parents or target in cache.parents
                or "node_modules" in target.parts
                or any(target == path or target in path.parents for path in registered_paths)
            ):
                continue
            try:
                if not target.is_dir():
                    continue
                identity = cache_identity(target)
                if not identity or identity[0] != target.parent.stat().st_dev:
                    continue
                if not same_worktree(run, worktree):
                    continue
                if worktree.branch is not None and not merged_worktree(
                    run, repo, worktree, remote_url, remote_refs, main_commits
                ):
                    continue
                if not idle(run, target, recent_minutes):
                    continue
                size = int(checked(run, ["du", "-sk", str(target)]).split("\t", 1)[0]) * 1024
                if (
                    not same_worktree(run, worktree)
                    or contains_protected_dir(target, identity[0])
                    or not idle(run, target, recent_minutes)
                    or cache_identity(target) != identity
                ):
                    continue
                if apply:
                    parent_fd = os.open(target.parent, os.O_DIRECTORY | os.O_NOFOLLOW)
                    try:
                        info = os.stat(target.name, dir_fd=parent_fd, follow_symlinks=False)
                        if (info.st_dev, info.st_ino) != identity:
                            continue
                        shutil.rmtree(target.name, dir_fd=parent_fd)
                    finally:
                        os.close(parent_fd)
                print(f"{'Reclaimed' if apply else 'Would reclaim'} {size / GIB:.2f} GiB: {target}")
                total += size
            except (OSError, subprocess.SubprocessError, ValueError, KeyError) as error:
                print(f"Warning: {target}: {error}")
                status = 1
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        print(f"Warning: cache scan stopped: {error}")
        status = 1
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
