#!/usr/bin/env python3
"""Quality coordinator's one-shot refresh. No checkout content enters the build."""
import contextlib
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import tarfile
import time
import uuid


def safe_path(path):
    path = Path(path).absolute()
    for parent in [path, *path.parents]:
        if parent.is_symlink():
            raise RuntimeError(f'Refusing symlink: {parent}')
    return path


def safe_tree(path):
    path = safe_path(path)
    if path.exists() and path.is_dir():
        for child in path.rglob('*'):
            if child.is_symlink():
                raise RuntimeError(f'Refusing symlink: {child}')
    return path


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def manifest(path):
    safe_tree(path)
    return {str(p.relative_to(path)): (digest(p), p.stat().st_mode & 0o777)
            for p in sorted(path.rglob('*')) if p.is_file()}


class LockBusy(BlockingIOError):
    def __init__(self, path):
        self.path = Path(path)
        super().__init__('Initial lock admission busy: ' + str(path))


@contextlib.contextmanager
def locked(path):
    safe_tree(path)
    with path.open('a+') as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise LockBusy(path) from error
        yield


def place(stage, app, previous, verify):
    for path in (stage, app, previous):
        safe_tree(path)
    if previous.exists():
        raise RuntimeError('Previous slot occupied')
    had_old = app.exists()
    if had_old:
        app.rename(previous)
    try:
        stage.rename(app)
        verify(app)
    except BaseException:
        if app.exists():
            app.rename(stage)
        if had_old:
            previous.rename(app)
        raise


def atomic_json(path, value):
    atomic_text(path, json.dumps(value, indent=2) + '\n')


def atomic_text(path, text):
    temporary = path.with_suffix('.tmp')
    safe_tree(temporary)
    with temporary.open('w') as stream:
        stream.write(text)
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)
    directory = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


class Refresh:
    def __init__(self, repo, cache, ops, app):
        self.repo, self.cache, self.ops, self.app = map(Path, (repo, cache, ops, app))
        self.env = os.environ.copy()
        self.env.update(DEVELOPER_DIR='/Library/Developer/CommandLineTools',
                        CARGO_TARGET_DIR=str(self.cache / 'target'), CARGO_BUILD_JOBS='2',
                        CARGO_INCREMENTAL='0', CI='1')
        self.env['PATH'] = str(Path.home() / '.cargo/bin') + ':' + self.env.get('PATH', '')
        # A local refresh never requests a private signing identity.
        for key in list(self.env):
            if key.startswith(('APPLE_', 'CSC_', 'TAURI_SIGNING_')):
                del self.env[key]

    def run(self, args, **kwargs):
        return subprocess.run(args, env=self.env, check=True, **kwargs)

    def git(self, *args):
        return self.run(['git', '-C', str(self.repo), *args], capture_output=True,
                        text=True).stdout.strip()

    def idle(self):
        output = self.run(['ps', '-axo', 'pid=,comm='], capture_output=True, text=True).stdout
        if any('/Contents/MacOS/grooviq' in line or line.split()[-1:] == ['grooviq']
               for line in output.splitlines()):
            raise RuntimeError('Grooviq is running; refresh deferred')

    def verify(self, bundle):
        safe_tree(bundle)
        info = plistlib.loads((bundle / 'Contents/Info.plist').read_bytes())
        if (info.get('CFBundleIdentifier') != 'io.github.yuriimurha.grooviq'
                or info.get('CFBundleExecutable') != 'grooviq'):
            raise RuntimeError('Unexpected bundle identity')
        if not (bundle / 'Contents/MacOS/grooviq').is_file():
            raise RuntimeError('Missing executable')
        self.run(['codesign', '--verify', '--deep', '--strict', str(bundle)])

    def build(self, revision, log):
        safe_tree(self.cache / 'target')
        source = safe_path(self.cache / 'source')
        marker = safe_path(self.cache / 'source-owned.json')
        # Legacy cache may contain precisely the dependency link made by refresh-once.py.
        link = source / 'app/ui/node_modules'
        safe_path(link.parent)
        if link.is_symlink():
            if (not marker.is_file() or os.readlink(link) != str(self.cache / 'ui-dependencies/node_modules')):
                raise RuntimeError('Unmanaged dependency link')
            link.unlink()
        safe_tree(source)
        if source.exists():
            if not marker.is_file():
                raise RuntimeError('Unmanaged source directory')
            shutil.rmtree(source)
        # Publish ownership before creating partial extraction state, so interruption is retryable.
        atomic_json(marker, {'source_sha': revision})
        source.mkdir()
        archive = safe_path(self.cache / 'source.tar')
        self.run(['git', '-C', str(self.repo), 'archive', '--format=tar', '-o', str(archive), revision])
        with tarfile.open(archive) as stream:
            # Refuse links even when tar's data filter would allow an internal link.
            if any(not (x.isfile() or x.isdir()) for x in stream.getmembers()):
                raise RuntimeError('Archive contains links or special files')
            stream.extractall(source, filter='data')
        archive.unlink()
        atomic_json(marker, {'source_sha': revision})
        deps = self.cache / 'ui-dependencies'
        # node_modules legitimately contains npm executable links. Never delete this cache.
        safe_path(deps)
        if deps.is_symlink() or (deps / 'node_modules').is_symlink():
            raise RuntimeError('External dependency cache')
        deps.mkdir(exist_ok=True)
        for child in deps.rglob('*'):
            if child.is_symlink() and not child.resolve().is_relative_to(deps.resolve()):
                raise RuntimeError('External dependency link')
        ui = source / 'app/ui'
        package_hash = hashlib.sha256((ui / 'package.json').read_bytes() +
                                     (ui / 'package-lock.json').read_bytes()).hexdigest()
        lock = deps / 'lock.sha256'
        safe_tree(lock)
        if not lock.exists() or lock.read_text() != package_hash or not (deps / 'node_modules/.bin/tauri').exists():
            for name in ('package.json', 'package-lock.json'):
                safe_tree(deps / name)
                shutil.copy2(ui / name, deps / name)
            self.run(['npm', 'ci', '--no-audit', '--no-fund'], cwd=deps, stdout=log, stderr=subprocess.STDOUT)
            lock.write_text(package_hash)
        (ui / 'node_modules').symlink_to(deps / 'node_modules', target_is_directory=True)
        self.run(['npm', 'run', 'tauri', 'build', '--', '--bundles', 'app', '--ci', '--',
                  '--locked', '--jobs', '2'], cwd=ui, stdout=log, stderr=subprocess.STDOUT)
        return self.cache / 'target/release/bundle/macos/Grooviq.app'

    def reconcile(self):
        intent_path = safe_path(self.ops / 'install-intent.json')
        if not intent_path.exists():
            return
        intent = json.loads(intent_path.read_text())
        stage, previous = Path(intent['stage']), Path(intent['previous'])
        if (stage.parent != self.app.parent
                or not re.fullmatch(r'\.Grooviq-quality-stage-[0-9a-f]{32}\.app', stage.name)
                or previous.parent != self.ops / 'previous'
                or not re.fullmatch(r'[0-9a-f]{32}-Grooviq\.app', previous.name)):
            raise RuntimeError('Foreign transaction paths; retained for inspection')
        receipt = safe_path(self.ops / 'installed.json')
        current = receipt.read_text() if receipt.exists() else None
        committed = json.loads(current) == intent['record'] if current is not None else False
        if not committed and current != intent['old_receipt']:
            raise RuntimeError('Foreign receipt; transaction retained')
        def identity(path):
            safe_tree(path)
            if not path.exists():
                return None
            self.verify(path)
            return json.loads(json.dumps(manifest(path)))
        # Validate every occupied path before the first mutation.
        cleanup = intent.get('phase') == 'cleanup'
        app_id, prior_id = map(identity, (self.app, previous))
        stage_id = None if cleanup else identity(stage)
        old, new = intent['old_manifest'], intent['new_manifest']
        if stage_id not in (None, new) or prior_id not in (None, old):
            raise RuntimeError('Foreign transaction artifact; retained')
        if (not cleanup and committed and app_id == old and stage_id == new
                and prior_id is None):
            # Older versions rolled the app back after receipt publication. Repair that
            # exact recorded state, including retry after a failed receipt directory sync.
            if intent['old_receipt'] is None:
                receipt.unlink()
            else:
                atomic_text(receipt, intent['old_receipt'])
            committed = False
        if cleanup:
            if (app_id != intent['terminal_app'] or prior_id != intent['terminal_previous']
                    or current != intent['terminal_receipt']):
                raise RuntimeError('Terminal cleanup identity mismatch')
        elif committed:
            if app_id != new or prior_id != old:
                raise RuntimeError('Committed transaction identity mismatch')
        else:
            valid = ((app_id == old and prior_id is None and stage_id in (None, new))
                     or (app_id is None and prior_id == old and stage_id == new)
                     or (app_id == new and prior_id == old and stage_id is None))
            if not valid:
                raise RuntimeError('Interrupted transaction identity mismatch')
            if app_id == new and stage_id is None and (prior_id is not None or old is None) and self.app.exists():
                self.app.rename(stage)
            if prior_id is not None:
                previous.rename(self.app)
        # Persist terminal identities before deletion; a partial stage is no longer a bundle.
        if not cleanup:
            intent.update(phase='cleanup', terminal_app=identity(self.app),
                          terminal_previous=identity(previous),
                          terminal_receipt=receipt.read_text() if receipt.exists() else None)
            atomic_json(intent_path, intent)
        if stage.exists():
            safe_tree(stage)
            remaining = json.loads(json.dumps(manifest(stage)))
            directories = {str(parent) for name in new for parent in Path(name).parents}
            for child in stage.rglob('*'):
                name = str(child.relative_to(stage))
                if ((child.is_file() and remaining.get(name) != new.get(name))
                        or (child.is_dir() and name not in directories)
                        or not (child.is_file() or child.is_dir())):
                    raise RuntimeError('Foreign cleanup artifact; retained')
            shutil.rmtree(stage)
        intent_path.unlink()

    def refresh(self):
        # Only canonical locations are exposed by the CLI; constructor injection is for fixtures.
        for path in (self.cache, self.ops, self.app):
            safe_path(path)
        self.cache.mkdir(parents=True, exist_ok=True)
        self.ops.mkdir(parents=True, exist_ok=True)
        with locked(self.cache / 'install.lock'):
            self.idle()
            self.reconcile()
            self.git('fetch', 'origin', '--prune')
            revision = self.git('rev-parse', 'origin/main')
            receipt = safe_path(self.ops / 'installed.json')
            prior = json.loads(receipt.read_text()) if receipt.exists() else {}
            if self.app.exists():
                if not prior.get('installed_executable_sha256'):
                    raise RuntimeError('Unmanaged installed application')
                self.verify(self.app)
                if digest(self.app / 'Contents/MacOS/grooviq') != prior['installed_executable_sha256']:
                    raise RuntimeError('Installed application differs from receipt')
            if prior.get('source_sha') == revision and self.app.exists():
                return {'status': 'unchanged', 'source_sha': revision}
            if prior.get('source_sha') and self.app.exists():
                changed = self.git('diff', '--name-only', prior['source_sha'], revision, '--',
                                   'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml',
                                   'app', 'dsp', 'engine', 'project')
                if not changed:
                    return {'status': 'no app-relevant changes', 'source_sha': prior['source_sha'],
                            'main_sha': revision}
            stamp = datetime.datetime.now(datetime.timezone.utc).isoformat()
            start = time.monotonic()
            with safe_path(self.ops / 'refresh-build.log').open('w') as log:
                bundle = self.build(revision, log)
            self.run(['codesign', '--force', '--sign', '-', '--timestamp=none', str(bundle)])
            self.verify(bundle)
            expected = manifest(bundle)
            stage = self.app.parent / ('.Grooviq-quality-stage-' + uuid.uuid4().hex + '.app')
            previous = self.ops / 'previous' / (uuid.uuid4().hex + '-Grooviq.app')
            safe_tree(previous.parent)
            previous.parent.mkdir(exist_ok=True)
            try:
                self.run(['ditto', str(bundle), str(stage)])
                def verify_copy(path):
                    if manifest(path) != expected:
                        raise RuntimeError('Bundle copy manifest mismatch')
                    self.verify(path)
                verify_copy(stage)
                self.idle()
                record = dict(source_sha=revision, started_utc=stamp, cargo_jobs=2,
                              build_seconds=round(time.monotonic() - start, 3),
                              installed_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                              installed_executable_sha256=digest(stage / 'Contents/MacOS/grooviq'),
                              previous_app=str(previous), status='installed', launched=False)
                # Receipt write participates in rollback if disk space/permissions fail.
                def finish(path):
                    verify_copy(path)
                    try:
                        atomic_json(receipt, record)
                    except OSError as error:
                        # Replacement may already be visible. Keep the verified new app with
                        # its matching receipt; report the failed durability operation honestly.
                        if not receipt.exists() or json.loads(receipt.read_text()) != record:
                            raise
                        record['publication_warning'] = str(error)
                intent_path = safe_path(self.ops / 'install-intent.json')
                atomic_json(intent_path, dict(stage=str(stage), previous=str(previous),
                            old_receipt=receipt.read_text() if receipt.exists() else None,
                            old_manifest=manifest(self.app) if self.app.exists() else None,
                            new_manifest=expected, record=record))
                place(stage, self.app, previous, finish)
                try:
                    intent_path.unlink()
                except OSError as error:
                    record['journal_warning'] = str(error)
            finally:
                if stage.exists() and not (self.ops / 'install-intent.json').exists():
                    safe_tree(stage)
                    shutil.rmtree(stage)
            # Placement and its receipt are committed; legacy cleanup cannot undo that success.
            old = Path(prior['previous_app']) if prior.get('previous_app') else None
            if old and old != previous and old.parent == previous.parent and old.exists():
                try:
                    self.verify(old)
                    shutil.rmtree(old)
                except (OSError, RuntimeError, subprocess.CalledProcessError, plistlib.InvalidFileException) as error:
                    record['cleanup_warning'] = str(error)
                    record['retained_previous_app'] = str(old)
            try:
                record['main_after_install'] = self.git('rev-parse', 'origin/main')
            except (OSError, subprocess.CalledProcessError) as error:
                record['metadata_warning'] = str(error)
            return record


if __name__ == '__main__':
    import sys
    if sys.platform != 'darwin':
        raise SystemExit('Local refresh requires macOS')
    home = Path.home()
    print(json.dumps(Refresh(Path(__file__).resolve().parents[1],
          home / 'Library/Caches/Grooviq/quality-release',
          home / 'Library/Application Support/Grooviq Ops',
          Path('/Applications/Grooviq.app')).refresh(), indent=2))
