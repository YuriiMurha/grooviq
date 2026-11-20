#!/usr/bin/env python3
"""Optional merged-main refresh coordinator. Never runs from a commit hook."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shlex
import shutil
import subprocess
import sys
import uuid

from refresh_install import Refresh, atomic_json, atomic_text, locked, safe_path

LABEL = 'io.github.yuriimurha.grooviq.refresh'
SNAPSHOT_FILES = ('refresh_agent.py', 'refresh_install.py')
TERMINAL = ('closed', 'withdrawn', 'expired_notrun', 'completed')


def stamp():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def require_runtime():
    if sys.version_info < (3, 12):
        raise RuntimeError('Grooviq refresh requires Python 3.12+; install Homebrew python and re-enable from the reviewed checkout. Python 3.9 cannot safely extract the source archive.')


def product_path(name):
    """Test/docs changes alone do not rebuild the installed application."""
    parts = Path(name).parts
    if any(p in ('tests', 'benches', 'e2e', '__tests__') for p in parts):
        return False
    if name.endswith(('.test.ts', '.spec.ts', '.md')):
        return False
    return (name in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml')
            or name.startswith(('dsp/', 'engine/', 'project/', 'app/src-tauri/', 'app/ui/src/'))
            or name in ('app/ui/package.json', 'app/ui/package-lock.json', 'app/ui/index.html')
            or (name.startswith('app/ui/vite') and name.endswith(('.ts', '.js', '.mjs'))))


class Agent:
    def __init__(self, repo, home=None, refresh_type=Refresh):
        self.repo = safe_path(repo)
        self.home = safe_path(home or Path.home())
        self.ops = safe_path(self.home / 'Library/Application Support/Grooviq Ops')
        self.state = safe_path(self.ops / 'automation')
        self.cache = safe_path(self.home / 'Library/Caches/Grooviq/quality-release')
        self.app = Path('/Applications/Grooviq.app')
        self.queue = self.state / 'pending.json'
        self.refresh_type = refresh_type
        self.env = dict(os.environ)
        self.env.update(DEVELOPER_DIR='/Library/Developer/CommandLineTools',
                        PATH=str(self.home / '.cargo/bin') + ':/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin')

    def git(self, *args):
        result = subprocess.run(['git', '-C', str(self.repo), *args], env=self.env,
                                check=True, capture_output=True, timeout=30)
        return result.stdout if args[0] == 'show' else result.stdout.decode().strip()

    def refresher(self):
        refresh = self.refresh_type(self.repo, self.cache, self.ops, self.app)
        # Refresh strips private signing settings. Do not merge the unfiltered
        # parent environment back over that deliberate boundary.
        refresh.env.update(PATH=self.env['PATH'], DEVELOPER_DIR=self.env['DEVELOPER_DIR'])
        refresh.env.update(CARGO_TARGET_DIR=str(self.cache / 'target'), CARGO_BUILD_JOBS='2',
                           CARGO_INCREMENTAL='0', CI='1')
        return refresh

    def toolchain(self):
        for tool in ('cargo', 'node', 'npm'):
            if shutil.which(tool, path=self.env['PATH']) is None:
                raise RuntimeError('Missing ' + tool + '; install the toolchain before enabling refresh. Rust PATH is ~/.cargo/bin; Node/npm use Homebrew or /usr/local/bin.')

    def read(self, path, default=None):
        safe_path(path)
        return json.loads(path.read_text()) if path.exists() else default

    def leases(self):
        # A physical exclusive operation file is an owner obligation, even after
        # a deadline. This agent never deletes or expires another actor's lease.
        for name in ('validation-lease.json', 'browser-lease.json', 'refresh-paused'):
            if os.path.lexists(self.ops / name):
                return name
        for name in ('resource-leases', 'browser-leases'):
            root = safe_path(self.ops / name)
            if not root.exists():
                continue
            for path in root.glob('*.json'):
                try:
                    status = self.read(path).get('status', '').lower()
                    if not status.startswith(TERMINAL):
                        return str(path)
                except (OSError, ValueError, AttributeError):
                    return str(path)
        return None

    def status(self):
        return {'enabled': (self.state / 'registration.json').exists()
                and not (self.state / 'disabled').exists(),
                'installed': self.read(self.ops / 'installed.json', {}),
                'pending': self.read(self.queue, None),
                'last_tick': self.read(self.state / 'last-tick.json', None),
                'blocking_lease': self.leases()}

    def record(self, value):
        value = dict(value, recorded_utc=stamp())
        atomic_json(self.state / 'last-tick.json', value)
        return value

    def perform_refresh(self):
        return self.refresher().refresh()

    def tick(self):
        require_runtime()
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        if (self.state / 'disabled').exists():
            return self.record({'status': 'disabled'})
        try:
            with locked(self.state / 'tick.lock'):
                return self._tick()
        except BlockingIOError:
            return {'status': 'deferred', 'reason': 'another refresh coordinator is active'}

    def _tick(self):
        lease_path = self.ops / 'validation-lease.json'
        nonce, owned = uuid.uuid4().hex, None
        try:
            self.git('fetch', 'origin', '--prune')
            latest = self.git('rev-parse', 'origin/main')
            if not re.fullmatch('[0-9a-f]{40}', latest):
                raise RuntimeError('invalid origin/main SHA')
            installed = self.read(self.ops / 'installed.json', {}).get('source_sha')
            if not installed or not re.fullmatch('[0-9a-f]{40}', installed):
                raise RuntimeError('verified initial installation required')
            if installed == latest:
                if self.queue.exists():
                    self.queue.unlink()  # Only this coordinator's pending metadata.
                return self.record({'status': 'unchanged', 'source_sha': installed})
            paths = self.git('diff', '--name-only', installed, latest, '--').splitlines()
            if not any(product_path(path) for path in paths):
                return self.record({'status': 'no product changes', 'observed_main': latest,
                                    'installed_source': installed})
            atomic_json(self.queue, {'source_sha': latest, 'observed_utc': stamp()})
            reason = self.leases()
            if reason:
                return self.record({'status': 'deferred', 'reason': 'active or unresolved lease: ' + reason})
            self.refresher().idle()
            self.toolchain()
            self.cache.mkdir(parents=True, exist_ok=True)
            with locked(self.cache / 'install.lock'):
                pass  # Refresh itself re-acquires this same nonblocking lock.
            try:
                fd = os.open(lease_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            except FileExistsError:
                return self.record({'status': 'deferred', 'reason': 'operation lease acquired by another owner'})
            owned = os.fstat(fd)
            with os.fdopen(fd, 'w') as stream:
                json.dump({'nonce': nonce, 'status': 'ACTIVE_STANDARD_REFRESH',
                           'pid': os.getpid(), 'source_sha': latest, 'started_utc': stamp()}, stream)
                stream.flush()
                os.fsync(stream.fileno())
            result = self.perform_refresh()  # Existing owner, cache, jobs2, transaction and app checks.
            current = lease_path.lstat()
            if ((current.st_dev, current.st_ino) != (owned.st_dev, owned.st_ino)
                    or self.read(lease_path).get('nonce') != nonce):
                raise RuntimeError('operation lease changed; preserve foreign owner')
            lease_path.unlink()
            owned = None
            if self.queue.exists():
                self.queue.unlink()
            return self.record(result)
        except Exception as error:
            if owned is not None:
                try:
                    current = lease_path.lstat()
                    if ((current.st_dev, current.st_ino) == (owned.st_dev, owned.st_ino)
                            and self.read(lease_path).get('nonce') == nonce):
                        value = self.read(lease_path)
                        value.update(status='FAILED_PROTECTED_REFRESH', error=str(error))
                        atomic_json(lease_path, value)
                except (OSError, ValueError):
                    pass
                return self.record({'status': 'failed protected', 'reason': str(error)})
            if isinstance(error, BlockingIOError) or 'running' in str(error).lower():
                return self.record({'status': 'deferred', 'reason': str(error)})
            return self.record({'status': 'failed', 'reason': str(error)})

    def _owned_existing(self, path, field, registration):
        safe_path(path)
        if path.exists():
            expected = registration.get(field)
            if expected is None or hashlib.sha256(path.read_bytes()).hexdigest() != expected:
                raise RuntimeError('foreign frontdoor/LaunchAgent preserved: ' + str(path))

    def enable(self):
        """Provision only bytes already present on fetched, protected origin/main."""
        require_runtime()
        self.git('fetch', 'origin', '--prune')
        revision = self.git('rev-parse', 'origin/main')
        if not re.fullmatch('[0-9a-f]{40}', revision):
            raise RuntimeError('invalid merged revision')
        payloads = {}
        for name in SNAPSHOT_FILES:
            path = Path(__file__).with_name(name)
            merged = self.git('show', revision + ':scripts/' + name)
            if path.is_symlink() or path.read_bytes() != merged:
                raise RuntimeError('enable requires exact reviewed merged source: ' + name)
            payloads[name] = merged
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        with locked(self.state / 'tick.lock'):
            registration = self.read(self.state / 'registration.json', {})
            wrapper = safe_path(self.home / 'bin/grooviq-refresh')
            plist = safe_path(self.home / 'Library/LaunchAgents' / (LABEL + '.plist'))
            self._owned_existing(wrapper, 'wrapper_sha256', registration)
            self._owned_existing(plist, 'plist_sha256', registration)
            version = safe_path(self.state / 'versions' / revision)
            version.mkdir(parents=True, exist_ok=True, mode=0o700)
            for name, raw in payloads.items():
                target = safe_path(version / name)
                if target.exists():
                    if target.read_bytes() != raw:
                        raise RuntimeError('provisioned snapshot changed')
                else:
                    with target.open('xb') as stream:
                        stream.write(raw)
            python = str(Path(sys.executable).resolve())
            script = str(version / 'refresh_agent.py')
            prefix = [python, '-S', '-B', script, '--repo', str(self.repo)]
            wrapper.parent.mkdir(parents=True, exist_ok=True)
            plist.parent.mkdir(parents=True, exist_ok=True)
            text = ('#!/bin/sh\n# Grooviq verified merged refresh frontdoor\n'
                    + 'if [ ! -x ' + shlex.quote(python) + ' ]; then\n'
                    + '  echo "Grooviq refresh needs its pinned Python 3.12+; reinstall Homebrew python and re-enable from the reviewed checkout." >&2\n'
                    + '  exit 127\nfi\nexec ' + shlex.join(prefix) + ' "$@"\n')
            data = plistlib.dumps({'Label': LABEL, 'ProgramArguments': prefix + ['tick'],
                'StartInterval': 300, 'RunAtLoad': False,
                'StandardOutPath': str(self.state / 'launchd.stdout.log'),
                'StandardErrorPath': str(self.state / 'launchd.stderr.log')})
            # Mark disabled before changing registration; a failed bootstrap never
            # leaves a partially updated agent permitted to build.
            atomic_text(self.state / 'disabled', 'registration in progress\n')
            if registration:
                subprocess.run(['/bin/launchctl', 'bootout', 'gui/' + str(os.getuid()) + '/' + LABEL],
                               check=False, capture_output=True, timeout=15)
            atomic_text(wrapper, text)
            wrapper.chmod(0o755)
            atomic_text(plist, data.decode())
            atomic_json(self.state / 'registration.json', {'source_sha': revision,
                'wrapper_sha256': hashlib.sha256(wrapper.read_bytes()).hexdigest(),
                'plist_sha256': hashlib.sha256(plist.read_bytes()).hexdigest(),
                'snapshot': str(version), 'repo': str(self.repo), 'python': python})
            subprocess.run(['/bin/launchctl', 'bootstrap', 'gui/' + str(os.getuid()), str(plist)],
                           check=True, capture_output=True, timeout=15)
            (self.state / 'disabled').unlink()
        return {'status': 'enabled', 'interval_seconds': 300, 'frontdoor': str(wrapper),
                'source_sha': revision, 'launched_app': False}

    def disable(self):
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        atomic_text(self.state / 'disabled', 'disabled by coordinator\n')
        try:
            with locked(self.state / 'tick.lock'):
                if (self.state / 'registration.json').exists():
                    subprocess.run(['/bin/launchctl', 'bootout', 'gui/' + str(os.getuid()) + '/' + LABEL],
                                   check=False, capture_output=True, timeout=15)
        except BlockingIOError:
            # An active tick keeps its original installer/children. Future ticks
            # see disabled and do no work; never bootout that retaining owner.
            return {'status': 'disabled', 'unload': 'deferred until active refresh settles'}
        return {'status': 'disabled', 'cache_and_branches_retained': True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('command', choices=('tick', 'enable', 'status', 'disable'))
    args = parser.parse_args()
    if sys.platform != 'darwin':
        parser.error('local refresh agent requires macOS')
    value = getattr(Agent(args.repo), args.command)()
    print(json.dumps(value, indent=2))
    return 1 if value.get('status', '').startswith('failed') else 0


if __name__ == '__main__':
    sys.exit(main())
