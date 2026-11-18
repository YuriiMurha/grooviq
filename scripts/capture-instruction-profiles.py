#!/usr/bin/env python3
"""Bounded historical #179 capture. Requires a clean, disposable Linux checkout.

Selected revisions execute their build scripts on the host; this is not a sandbox.
Only the verified first pair is accepted. No workload overlay or codegen flags.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import resource
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time

BASE = '014db45a96789bce99a27b1661d5a5204950ca6c'
HEAD = 'bab875e7063ca5cc3427f4fa54c4f4b698758ac7'
LIMIT = 128 * 1024 * 1024
PINS = {
    'tests/golden/benches/instructions.rs': '1326c523f7a773f17f117451071d609fbad22799',
    'tests/golden/src/perf.rs': '58bf15c66e6b50eb52558511fdf4c4c3a87579a5',
    'tests/golden/src/lib.rs': 'bf12515db9dd39838c122a066cdd000d0c8dd32e',
    'engine/src/processor.rs': '99b5a0e612f1f2f3fecf14dc148b9f317b357a01',
    'Cargo.toml': '840f4f794aba20965d9a69db046d5e546715664b',
    'rust-toolchain.toml': '640a93b2aa0b414440705d09f9cf5384443aa022',
}
CASES = ('voices_16', 'init_4', 'dark_pad_4', 'drums_4ch', 'beat_16ch')
TOTALS = ((27658416, 15986725, 37927429, 62800446, 435741806),
          (27658416, 15986725, 37927429, 63434983, 436995569))


class CaptureError(RuntimeError):
    pass


def run(args, cwd, env=None, allowed=(0,)):
    p = subprocess.run(args, cwd=cwd, env=env, capture_output=True, timeout=1500)
    if p.returncode not in allowed:
        raise CaptureError(f'{args[0]} exited {p.returncode}: {p.stderr[-2000:].decode(errors="replace")}')
    return p.stdout.decode(errors='strict').strip()


def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def regular(path, root):
    if path.is_symlink() or not path.is_file() or not path.resolve().is_relative_to(root.resolve()):
        raise CaptureError(f'unsafe evidence path: {path}')
    # Reject symlinked ancestors as well.
    if any(p.is_symlink() for p in path.parents if p != root.parent):
        raise CaptureError(f'symlink ancestor: {path}')


def budget(out):
    total = 0
    for p in out.rglob('*'):
        if p.is_symlink():
            raise CaptureError('symlink in output')
        if p.is_file():
            total += p.stat().st_size
    if total > LIMIT:
        raise CaptureError('artifact exceeds 128 MiB; required evidence must not be dropped')
    return total


def file_limit(out):
    remaining = max(0, LIMIT - budget(out) - 1024 * 1024)
    return lambda: resource.setrlimit(resource.RLIMIT_FSIZE, (remaining, remaining))


def copy_evidence(src, dest, root, out):
    regular(src, root)
    if budget(out) + src.stat().st_size > LIMIT:
        raise CaptureError('artifact would exceed 128 MiB')
    with src.open('rb') as r, dest.open('xb') as w:
        shutil.copyfileobj(r, w)
    return {'source': str(src), 'file': str(dest.relative_to(out)),
            'bytes': dest.stat().st_size, 'sha256': sha(dest)}


def profiles(target, label, started):
    suffix = '.out.base@base' if label == 'base' else '.out'
    found = {}
    for p in target.rglob('callgrind.*'):
        if not p.name.endswith(suffix):
            continue
        regular(p, target)
        text = p.read_text()
        if 'events:' not in text or 'summary:' not in text:
            continue
        case = [c for c in CASES if c in str(p.relative_to(target)).split('/')[-2] or c in p.name]
        if len(case) != 1 or case[0] in found:
            raise CaptureError(f'unknown or duplicate profile: {p}')
        if p.stat().st_mtime_ns < started:
            raise CaptureError('stale profile')
        events = re.search(r'^events: (.+)$', text, re.M).group(1).split()
        values = re.search(r'^summary: (.+)$', text, re.M).group(1).split()
        count = int(values[events.index('Ir')])
        if count <= 0:
            raise CaptureError('zero Ir')
        cmd = re.search(r'^cmd: (.+)$', text, re.M)
        if not cmd:
            raise CaptureError('profile has no executable command')
        binary = Path(shlex.split(cmd.group(1))[0])
        regular(binary, target)
        with binary.open('rb') as f:
            if f.read(4) != b'\x7fELF':
                raise CaptureError('profile command is not an ELF binary')
        found[case[0]] = (p, binary, count)
    if set(found) != set(CASES):
        raise CaptureError(f'expected five fresh profiles, got {sorted(found)}')
    return found


def validate(repo, base, head, output):
    # Validate all external options before any process is invoked.
    if not all(re.fullmatch('[0-9a-f]{40}', s) for s in (base, head)) or (base, head) != (BASE, HEAD):
        raise CaptureError('only the verified historical first pair is supported')
    if platform.system() != 'Linux' or platform.machine() != 'x86_64':
        raise CaptureError('requires provisioned Linux x86_64 measurement host')
    if repo.is_symlink() or repo.resolve() != repo.absolute() or not (repo / '.git').exists():
        raise CaptureError('requires canonical managed repository path')
    if output.exists() or output.is_symlink() or output.parent.resolve() != output.parent.absolute():
        raise CaptureError('output must be a new directory under a canonical existing parent')
    if not output.parent.is_dir() or output.is_relative_to(repo) or repo.is_relative_to(output):
        raise CaptureError('output must be outside repository')
    git = lambda *a: run(['git', *a], repo)
    if git('rev-parse', '--show-toplevel') != str(repo) or git('status', '--porcelain', '--untracked-files=all'):
        raise CaptureError('requires clean checkout including untracked files')
    for revision in (base, head):
        if git('cat-file', '-t', revision) != 'commit':
            raise CaptureError('revision is not a commit')
        for path, blob in PINS.items():
            if git('rev-parse', f'{revision}:{path}') != blob:
                raise CaptureError(f'historical workload mismatch: {path}')
    return git


def capture(repo, base, head, output):
    git = validate(repo, base, head, output)
    original = git('symbolic-ref', '--quiet', '--short', 'HEAD') if git('rev-parse', '--abbrev-ref', 'HEAD') != 'HEAD' else git('rev-parse', 'HEAD')
    env = {k: v for k, v in os.environ.items() if k in ('PATH', 'HOME', 'RUSTUP_HOME', 'CARGO_HOME', 'LANG', 'TMPDIR')}
    env.update(CARGO_BUILD_JOBS='2', CARGO_INCREMENTAL='0')
    tools = {}
    for name, args in {'rustc': ['rustc', '+1.100.0', '-Vv'], 'cargo': ['cargo', '+1.100.0', '-V'],
                       'runner': ['gungraun-runner', '--version'], 'valgrind': ['valgrind', '--version'],
                       'valgrind_package': ['dpkg-query', '-W', 'valgrind'], 'uname': ['uname', '-a']}.items():
        tools[name] = run(args, repo, env)
    if '0ef1bc386059596dada8fb65b50425aab2ea637a' not in tools['rustc'] or '0.20.0' not in tools['runner']:
        raise CaptureError('compiler/runner mismatch')
    output.mkdir(mode=0o700)
    report = {'status': 'failed', 'tools': tools, 'environment': {k: os.environ.get(k) for k in ('ImageOS', 'ImageVersion', 'RUNNER_ARCH')},
              'source_blobs': PINS, 'cpu': Path('/proc/cpuinfo').read_text() if Path('/proc/cpuinfo').exists() else 'unavailable',
              'flags': {'CARGO_BUILD_JOBS': '2', 'CARGO_INCREMENTAL': '0'}, 'runs': [],
              'limits': 'Historical runner image may be unavailable; totals must match before attribution. Selected commits execute build scripts; no sandbox.'}
    try:
        # Private fresh target prevents stale profile reuse; never clean a caller cache.
        with tempfile.TemporaryDirectory(prefix='grooviq-370-target-') as tmp:
            target = Path(tmp).resolve()
            env['CARGO_TARGET_DIR'] = str(target)
            for i, (label, revision) in enumerate((('base', base), ('head', head))):
                git('checkout', '--quiet', '--detach', revision)
                dest = output / label
                dest.mkdir()
                record = {'commit': revision, 'lock_blob': git('rev-parse', f'{revision}:Cargo.lock'),
                          'lock_sha256': sha(repo / 'Cargo.lock'), 'fixture_tree': git('rev-parse', f'{revision}:tests/golden/samples'), 'artifacts': []}
                report['runs'].append(record)
                build = ['cargo', '+1.100.0', 'bench', '--locked', '--no-run', '--message-format=json', '-p', 'grooviq-golden', '--bench', 'instructions']
                built = run(build, repo, env)
                artifacts = [json.loads(line) for line in built.splitlines() if line.startswith('{')]
                executables = {a['executable'] for a in artifacts if a.get('reason') == 'compiler-artifact'
                               and a.get('target', {}).get('name') == 'instructions' and a.get('executable')}
                if len(executables) != 1:
                    raise CaptureError('expected one Cargo instructions executable')
                executable = Path(executables.pop())
                regular(executable, target)
                args = [str(executable), '--save-baseline=base' if i == 0 else '--baseline=base']
                record['build_command'] = build
                record['command'] = args
                started = time.time_ns()
                with (dest / 'run.log').open('xb') as log:
                    p = subprocess.run(args, cwd=repo, env=env, stdout=log, stderr=subprocess.STDOUT, timeout=1500, preexec_fn=file_limit(output))
                record['exit'] = p.returncode
                if p.returncode not in ((0,) if i == 0 else (0, 3)):
                    raise CaptureError(f'{label} benchmark failed with exit {p.returncode}')
                selected = profiles(target, label, started)
                record['totals'] = {c: v[2] for c, v in selected.items()}
                binaries = {}
                for case, (raw, binary, _) in selected.items():
                    record['artifacts'].append(copy_evidence(raw, dest / f'{case}.callgrind', target, output))
                    if binary not in binaries:
                        binaries[binary] = dest / f'binary-{len(binaries)}'
                        record['artifacts'].append(copy_evidence(binary, binaries[binary], target, output))
                        for tool in ('nm', 'objdump'):
                            flags = ['-C', str(binary)] if tool == 'nm' else ['-d', '-C', str(binary)]
                            with (dest / f'{binaries[binary].name}.{tool}').open('xb') as f:
                                subprocess.run([tool, *flags], stdout=f, stderr=subprocess.STDOUT, check=True, timeout=90, preexec_fn=file_limit(output))
                    for inclusive in ('no', 'yes'):
                        with (dest / f'{case}.inclusive-{inclusive}.txt').open('xb') as f:
                            subprocess.run(['callgrind_annotate', '--show=Ir', '--threshold=100', f'--inclusive={inclusive}', '--tree=both', str(raw)],
                                           stdout=f, stderr=subprocess.STDOUT, check=True, timeout=90, preexec_fn=file_limit(output))
                    budget(output)
                if git('status', '--porcelain', '--untracked-files=all'):
                    raise CaptureError('benchmark modified checkout')
            report['historical_totals_match'] = all(r['totals'] == dict(zip(CASES, TOTALS[i])) for i, r in enumerate(report['runs']))
            report['status'] = 'completed_with_regression' if report['runs'][1]['exit'] == 3 else 'completed'
            report['historical_kit_match'] = ('1:3.22.0-0ubuntu3' in tools['valgrind_package'] and os.environ.get('ImageVersion') == '20261111.127.1')
            if not report['historical_totals_match'] or not report['historical_kit_match']:
                report['status'] += '_unmatched_kit'
    finally:
        # Never discard tracked or untracked writes from selected build scripts.
        try:
            git('checkout', '--quiet', original)
        finally:
            report['files'] = {str(p.relative_to(output)): {'sha256': sha(p), 'bytes': p.stat().st_size}
                               for p in output.rglob('*') if p.is_file() and not p.is_symlink()}
            (output / 'manifest.json').write_text(json.dumps(report, indent=2) + '\n')
            budget(output)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--base', default=BASE)
    parser.add_argument('--head', default=HEAD)
    a = parser.parse_args()
    def expired(signum, frame):
        raise CaptureError('capture exceeded 29 minute deadline')
    signal.signal(signal.SIGALRM, expired)
    signal.signal(signal.SIGTERM, expired)
    signal.alarm(29 * 60)
    try:
        print(json.dumps(capture(a.repo, a.base, a.head, a.output), indent=2))
    finally:
        signal.alarm(0)


if __name__ == '__main__':
    main()
