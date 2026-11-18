import importlib.util
import pathlib
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('refresh', pathlib.Path(__file__).with_name('refresh_install.py'))
m = importlib.util.module_from_spec(spec)
if spec.loader and pathlib.Path(spec.origin).exists():
    spec.loader.exec_module(m)


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name).resolve()
        self.old = self.root / 'Grooviq.app'
        self.stage = self.root / 'stage.app'
        self.backup = self.root / 'prior.app'
        for p, content in [(self.old, b'old'), (self.stage, b'new')]:
            p.mkdir()
            (p / 'binary').write_bytes(content)

    def test_failed_placement_restores_prior_app(self):
        def verify(p):
            if p == self.old:
                raise RuntimeError('installed verification failed')
        with self.assertRaises(RuntimeError):
            m.place(self.stage, self.old, self.backup, verify)
        self.assertEqual((self.old / 'binary').read_bytes(), b'old')

    def test_symlink_guard_preserves_external_data(self):
        external = self.root / 'external'
        external.mkdir()
        (external / 'precious').write_text('keep')
        link = self.root / 'link'
        link.symlink_to(external)
        with self.assertRaises(RuntimeError):
            m.safe_tree(link)
        self.assertEqual((external / 'precious').read_text(), 'keep')

    def test_manifest_detects_corruption_and_modes(self):
        first = m.manifest(self.stage)
        (self.stage / 'binary').write_bytes(b'corrupt')
        self.assertNotEqual(first, m.manifest(self.stage))

    def test_lock_contention(self):
        with m.locked(self.root / 'lock'):
            with self.assertRaises(BlockingIOError):
                with m.locked(self.root / 'lock'):
                    self.fail('second coordinator entered')




class TransactionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = pathlib.Path(self.tmp.name).resolve()
        self.app = root / 'Applications/Grooviq.app'
        self.app.parent.mkdir()
        self.cache, self.ops = root / 'cache', root / 'ops'
        self.ops.mkdir()
        self.cache.mkdir()
        self.fixture = root / 'fixture.app'
        self.make_app(self.app, b'old')
        self.make_app(self.fixture, b'new')
        self.calls = []
        owner = self

        class Fake(m.Refresh):
            def git(self, *args):
                owner.calls.append(('git', args))
                return 'engine/src/lib.rs' if args[0] == 'diff' else 'b' * 40

            def run(self, args, **kwargs):
                owner.calls.append(tuple(args))
                if args[0] == 'ditto':
                    m.shutil.copytree(args[1], args[2])
                return type('Result', (), {'stdout': ''})()

            def build(self, revision, log):
                owner.calls.append(('build', revision))
                return owner.fixture

        self.refresh = Fake(root, self.cache, self.ops, self.app)
        m.atomic_json(self.ops / 'installed.json', {
            'source_sha': 'a' * 40,
            'installed_executable_sha256': m.digest(self.app / 'Contents/MacOS/grooviq')})

    def make_app(self, p, value):
        (p / 'Contents/MacOS').mkdir(parents=True)
        (p / 'Contents/MacOS/grooviq').write_bytes(value)
        (p / 'Contents/Info.plist').write_bytes(m.plistlib.dumps({
            'CFBundleIdentifier': 'io.github.yuriimurha.grooviq',
            'CFBundleExecutable': 'grooviq'}))

    def old_preserved(self):
        self.assertEqual((self.app / 'Contents/MacOS/grooviq').read_bytes(), b'old')

    def test_build_failure_preserves_old(self):
        with patch.object(self.refresh, 'build', side_effect=RuntimeError('build failed')):
            with self.assertRaisesRegex(RuntimeError, 'build failed'):
                self.refresh.refresh()
        self.old_preserved()

    def test_running_app_refuses_before_build(self):
        with patch.object(self.refresh, 'idle', side_effect=RuntimeError('running')):
            with self.assertRaisesRegex(RuntimeError, 'running'):
                self.refresh.refresh()
        self.assertFalse(any(x[0] == 'build' for x in self.calls))
        self.old_preserved()

    def test_corrupt_copy_preserves_old(self):
        run = self.refresh.run
        def corrupt(args, **kwargs):
            result = run(args, **kwargs)
            if args[0] == 'ditto':
                (pathlib.Path(args[2]) / 'Contents/MacOS/grooviq').write_bytes(b'bad')
            return result
        with patch.object(self.refresh, 'run', side_effect=corrupt):
            with self.assertRaisesRegex(RuntimeError, 'manifest mismatch'):
                self.refresh.refresh()
        self.old_preserved()

    def test_receipt_failure_rolls_back(self):
        atomic = m.atomic_json
        def fail_receipt(path, value):
            if path == self.ops / 'installed.json':
                raise OSError('disk full')
            return atomic(path, value)
        with patch.object(m, 'atomic_json', side_effect=fail_receipt):
            with self.assertRaisesRegex(OSError, 'disk full'):
                self.refresh.refresh()
        self.old_preserved()

    def test_success_records_exact_hash_and_no_launch(self):
        record = self.refresh.refresh()
        installed = self.app / 'Contents/MacOS/grooviq'
        self.assertEqual(record['installed_executable_sha256'], m.digest(installed))
        self.assertEqual(installed.read_bytes(), b'new')
        self.assertEqual((pathlib.Path(record['previous_app']) / 'Contents/MacOS/grooviq').read_bytes(), b'old')
        self.assertFalse(any(x[0] in ('open', 'osascript', 'kill', 'sudo') for x in self.calls))
        self.assertIn(('build', 'b' * 40), self.calls)
        self.assertEqual(self.refresh.refresh()['status'], 'unchanged')
        self.assertEqual(len([x for x in self.calls if x[0] == 'build']), 1)

    def test_unverifiable_prior_backup_does_not_fail_completed_install(self):
        legacy = self.ops / 'previous/legacy-Grooviq.app'
        self.make_app(legacy, b'legacy')
        receipt = self.ops / 'installed.json'
        prior = m.json.loads(receipt.read_text())
        prior['previous_app'] = str(legacy)
        m.atomic_json(receipt, prior)
        verify = self.refresh.verify
        def reject_legacy(path):
            if path == legacy:
                raise RuntimeError('unsealed legacy signature')
            return verify(path)
        with patch.object(self.refresh, 'verify', side_effect=reject_legacy):
            record = self.refresh.refresh()
        self.assertEqual(record['status'], 'installed')
        self.assertEqual((self.app / 'Contents/MacOS/grooviq').read_bytes(), b'new')
        self.assertEqual((legacy / 'Contents/MacOS/grooviq').read_bytes(), b'legacy')
        self.assertIn('unsealed legacy signature', record['cleanup_warning'])
        self.assertEqual(record['retained_previous_app'], str(legacy))
        saved = m.json.loads(receipt.read_text())
        self.assertEqual(saved['installed_executable_sha256'], m.digest(self.app / 'Contents/MacOS/grooviq'))
        self.assertEqual(saved['source_sha'], 'b' * 40)

    def test_signature_failures_preserve_app_and_receipt(self):
        for boundary in ('source', 'stage', 'installed'):
            with self.subTest(boundary=boundary):
                receipt = (self.ops / 'installed.json').read_bytes()
                run = self.refresh.run
                def reject(args, **kwargs):
                    if args[:2] == ['codesign', '--verify']:
                        path = pathlib.Path(args[-1])
                        new_installed = path == self.app and (path / 'Contents/MacOS/grooviq').read_bytes() == b'new'
                        if ((boundary == 'source' and path == self.fixture)
                                or (boundary == 'stage' and path.name.startswith('.Grooviq-quality-stage-'))
                                or (boundary == 'installed' and new_installed)):
                            raise m.subprocess.CalledProcessError(1, args)
                    return run(args, **kwargs)
                with patch.object(self.refresh, 'run', side_effect=reject):
                    with self.assertRaises(m.subprocess.CalledProcessError):
                        self.refresh.refresh()
                self.old_preserved()
                self.assertEqual((self.ops / 'installed.json').read_bytes(), receipt)

    def stop_during_placement(self, window):
        pid = m.os.fork()
        if pid == 0:
            def interrupt(stage, app, previous, verify):
                app.rename(previous)
                if window in ('new-placed', 'receipt-written'):
                    stage.rename(app)
                if window == 'receipt-written':
                    verify(app)
                m.os._exit(74)
            with patch.object(m, 'place', side_effect=interrupt):
                self.refresh.refresh()
            m.os._exit(75)
        _, status = m.os.waitpid(pid, 0)
        self.assertEqual(m.os.waitstatus_to_exitcode(status), 74)

    def test_interrupted_committed_receipt_finishes_without_rebuild(self):
        self.stop_during_placement('receipt-written')
        self.assertEqual(self.refresh.refresh()['status'], 'unchanged')
        self.assertEqual((self.app / 'Contents/MacOS/grooviq').read_bytes(), b'new')
        self.assertFalse((self.ops / 'install-intent.json').exists())

    def test_recovery_refuses_foreign_artifact_and_path(self):
        self.stop_during_placement('new-placed')
        intent_path = self.ops / 'install-intent.json'
        intent = m.json.loads(intent_path.read_text())
        prior = pathlib.Path(intent['previous'])
        binary = prior / 'Contents/MacOS/grooviq'
        binary.write_bytes(b'foreign')
        receipt = (self.ops / 'installed.json').read_bytes()
        with self.assertRaisesRegex(RuntimeError, 'Foreign transaction artifact'):
            self.refresh.refresh()
        self.assertEqual(binary.read_bytes(), b'foreign')
        self.assertEqual((self.ops / 'installed.json').read_bytes(), receipt)
        intent['previous'] = str(self.fixture)
        m.atomic_json(intent_path, intent)
        with self.assertRaisesRegex(RuntimeError, 'Foreign transaction paths'):
            self.refresh.refresh()
        self.assertTrue(self.fixture.exists())

    def test_interrupted_placement_recovers_both_windows(self):
        for window in ('old-moved', 'new-placed'):
            with self.subTest(window=window):
                receipt = (self.ops / 'installed.json').read_bytes()
                self.stop_during_placement(window)
                # Recovery executes before a subsequent build; refuse that build to inspect it.
                with patch.object(self.refresh, 'build', side_effect=RuntimeError('after recovery')):
                    with self.assertRaisesRegex(RuntimeError, 'after recovery'):
                        self.refresh.refresh()
                self.old_preserved()
                self.assertEqual((self.ops / 'installed.json').read_bytes(), receipt)
                self.assertFalse((self.ops / 'install-intent.json').exists())

    def test_malformed_legacy_and_metadata_failure_are_warnings(self):
        legacy = self.ops / 'previous/legacy-Grooviq.app'
        self.make_app(legacy, b'legacy')
        (legacy / 'Contents/Info.plist').write_bytes(b'invalid plist')
        receipt = self.ops / 'installed.json'
        prior = m.json.loads(receipt.read_text())
        prior['previous_app'] = str(legacy)
        m.atomic_json(receipt, prior)
        git = self.refresh.git
        calls = 0
        def fail_final(*args):
            nonlocal calls
            if args[0] == 'rev-parse':
                calls += 1
                if calls == 2:
                    raise m.subprocess.CalledProcessError(1, args)
            return git(*args)
        with patch.object(self.refresh, 'git', side_effect=fail_final):
            record = self.refresh.refresh()
        self.assertEqual(record['status'], 'installed')
        self.assertIn('cleanup_warning', record)
        self.assertIn('metadata_warning', record)
        self.assertTrue(legacy.exists())
        self.assertEqual(m.json.loads(receipt.read_text())['installed_executable_sha256'],
                         m.digest(self.app / 'Contents/MacOS/grooviq'))

    def test_unmanaged_app_refusal(self):
        (self.ops / 'installed.json').unlink()
        with self.assertRaisesRegex(RuntimeError, 'Unmanaged'):
            self.refresh.refresh()
        self.old_preserved()

    def test_actual_process_probe_refuses_running_app(self):
        with patch.object(self.refresh, 'run', return_value=type('Result', (), {
                'stdout': '123 /Applications/Grooviq.app/Contents/MacOS/grooviq\n'})()):
            with self.assertRaisesRegex(RuntimeError, 'running'):
                self.refresh.idle()

    def test_no_relevant_change_coalesces_without_advancing_receipt(self):
        git = self.refresh.git
        with patch.object(self.refresh, 'git', side_effect=lambda *a: '' if a[0] == 'diff' else git(*a)):
            result = self.refresh.refresh()
        self.assertEqual(result['status'], 'no app-relevant changes')
        self.assertEqual(result['source_sha'], 'a' * 40)
        self.assertFalse(any(x[0] == 'build' for x in self.calls))
        self.old_preserved()

    def test_cache_symlink_refusal(self):
        self.cache.rmdir()
        external = self.cache.parent / 'external'
        external.mkdir()
        self.cache.symlink_to(external)
        with self.assertRaisesRegex(RuntimeError, 'symlink'):
            self.refresh.refresh()
        self.assertEqual(list(external.iterdir()), [])


class SourceTests(unittest.TestCase):
    def test_immutable_archive_and_dependency_reuse(self):
        import io
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp).resolve()
            cache = root / 'cache'
            cache.mkdir()
            deps = cache / 'ui-dependencies'
            (deps / 'node_modules/.bin').mkdir(parents=True)
            (deps / 'node_modules/.bin/tauri').write_text('fake')
            package = b'{"name":"fixture"}'
            (deps / 'lock.sha256').write_text(m.hashlib.sha256(package + package).hexdigest())
            refresh = m.Refresh(root, cache, root / 'ops', root / 'app')
            commands = []
            def run(args, **kwargs):
                commands.append(args)
                if 'archive' in args:
                    with m.tarfile.open(cache / 'source.tar', 'w') as archive:
                        for name in ('package.json', 'package-lock.json'):
                            member = m.tarfile.TarInfo('app/ui/' + name)
                            member.size = len(package)
                            archive.addfile(member, io.BytesIO(package))
            with patch.object(refresh, 'run', side_effect=run):
                refresh.build('b' * 40, io.StringIO())
            self.assertEqual(commands[0][-1], 'b' * 40)
            self.assertFalse(any(x[:2] == ['npm', 'ci'] for x in commands))
            self.assertEqual(commands[-1][-3:], ['--locked', '--jobs', '2'])
            self.assertEqual(refresh.env['CARGO_TARGET_DIR'], str(cache / 'target'))

    def test_source_symlink_never_unlinks_external_dependency(self):
        import io
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp).resolve()
            cache = root / 'cache'
            cache.mkdir()
            external = root / 'external'
            (external / 'app/ui').mkdir(parents=True)
            link = external / 'app/ui/node_modules'
            link.symlink_to(cache / 'ui-dependencies/node_modules')
            (cache / 'source').symlink_to(external)
            m.atomic_json(cache / 'source-owned.json', {})
            refresh = m.Refresh(root, cache, root / 'ops', root / 'app')
            with self.assertRaisesRegex(RuntimeError, 'symlink'):
                refresh.build('b' * 40, io.StringIO())
            self.assertTrue(link.is_symlink())
            self.assertEqual(m.os.readlink(link), str(cache / 'ui-dependencies/node_modules'))

    def test_archive_failure_retry_remains_owned(self):
        import io
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp).resolve()
            cache = root / 'cache'
            cache.mkdir()
            refresh = m.Refresh(root, cache, root / 'ops', root / 'app')
            with patch.object(refresh, 'run', side_effect=RuntimeError('archive failed')):
                for _ in range(2):
                    with self.assertRaisesRegex(RuntimeError, 'archive failed'):
                        refresh.build('b' * 40, io.StringIO())

    def test_archive_link_rejected(self):
        import io
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp).resolve()
            cache = root / 'cache'
            cache.mkdir()
            refresh = m.Refresh(root, cache, root / 'ops', root / 'app')
            def run(args, **kwargs):
                with m.tarfile.open(cache / 'source.tar', 'w') as archive:
                    member = m.tarfile.TarInfo('outside')
                    member.type = m.tarfile.SYMTYPE
                    member.linkname = str(root)
                    archive.addfile(member)
            with patch.object(refresh, 'run', side_effect=run):
                with self.assertRaisesRegex(RuntimeError, 'links or special'):
                    refresh.build('b' * 40, io.StringIO())
            self.assertFalse((root / 'outside').exists())


if __name__ == '__main__':
    import sys
    suite = unittest.defaultTestLoader.loadTestsFromModule(sys.modules[__name__])
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(0 if result.testsRun > 0 and result.wasSuccessful() and not result.skipped else 1)
