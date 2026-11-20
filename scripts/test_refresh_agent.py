"""Owned temporary fixtures only: no build, launchd registration or app launch."""
import importlib.util
import contextlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

MODULE = Path(__file__).with_name('refresh_agent.py')
spec = importlib.util.spec_from_file_location('refresh_agent', MODULE) if MODULE.exists() else None
m = importlib.util.module_from_spec(spec) if spec else None
if spec:
    spec.loader.exec_module(m)


class AgentTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(m, 'verified merged-main agent feature is not implemented')
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name).resolve()
        self.repo = self.home / 'repo'
        self.repo.mkdir()
        owner = self
        self.latest, self.installed = 'b' * 40, 'a' * 40
        self.paths = ['app/src-tauri/src/lib.rs']
        self.calls, self.fail_build, self.running = [], False, False

        class Refresh(m.Refresh):
            def idle(self):
                if owner.running:
                    raise RuntimeError('Grooviq is running; refresh deferred')

            def refresh(self):
                owner.calls.append(('refresh', owner.latest))
                if owner.fail_build:
                    raise RuntimeError('build failed')
                m.atomic_json(self.ops / 'installed.json', {'source_sha': owner.latest})
                return {'status': 'installed', 'source_sha': owner.latest, 'launched': False}

        class Fake(m.Agent):
            def toolchain(self):
                pass  # Fixture never discovers or invokes the host toolchain.

            def git(self, *args):
                owner.calls.append(('git', args))
                if args[0] == 'rev-parse': return owner.latest
                if args[0] == 'diff': return '\n'.join(owner.paths)
                if args[0] == 'show': return b'merged source\n'
                return ''

        self.agent = Fake(self.repo, self.home, refresh_type=Refresh)
        self.agent.ops.mkdir(parents=True)
        m.atomic_json(self.agent.ops / 'installed.json', {'source_sha': self.installed})

    def test_latest_main_installs_once_and_next_tick_skips(self):
        self.assertEqual(self.agent.tick()['status'], 'installed')
        self.assertEqual(self.agent.tick()['status'], 'unchanged')
        self.assertEqual([c for c in self.calls if c[0] == 'refresh'], [('refresh', self.latest)])
        self.assertFalse((self.agent.ops / 'validation-lease.json').exists())

    def test_docs_and_test_only_do_not_build_or_change_installed_source(self):
        self.paths = ['README.md', 'app/ui/e2e/bpm-save.e2e.ts', 'engine/tests/safety.rs']
        self.assertEqual(self.agent.tick()['status'], 'no product changes')
        self.assertFalse(any(c[0] == 'refresh' for c in self.calls))
        self.assertEqual(json.loads((self.agent.ops / 'installed.json').read_text())['source_sha'], self.installed)

    def test_product_inputs_are_conservative(self):
        for path in ['app/ui/src/app.css', 'app/ui/package-lock.json', 'app/src-tauri/tauri.conf.json',
                     'engine/src/lib.rs', 'dsp/Cargo.toml', 'Cargo.lock', 'app/ui/vite.config.ts',
                     'app/ui/public/icon.svg', 'app/ui/tsconfig.app.json']:
            self.assertTrue(m.product_path(path), path)
        self.assertFalse(m.product_path('docs/research.md'))

    def test_validation_lease_defers_and_preserves_foreign_bytes(self):
        lease = self.agent.ops / 'validation-lease.json'
        lease.write_bytes(b'foreign actual owner')
        self.assertEqual(self.agent.tick()['status'], 'deferred')
        self.assertEqual(lease.read_bytes(), b'foreign actual owner')
        self.assertFalse(any(c[0] == 'refresh' for c in self.calls))

    def test_active_or_unknown_resource_lease_defers_but_closed_does_not(self):
        directory = self.agent.ops / 'resource-leases'
        directory.mkdir()
        lease = directory / 'fixture.json'
        lease.write_text('{"status":"ACTIVE", "absolute_deadline_utc":"2000-01-01T00:00:00Z"}')
        self.assertEqual(self.agent.tick()['status'], 'deferred')
        lease.write_text('{')
        self.assertEqual(self.agent.tick()['status'], 'deferred')
        lease.write_text('{"status":"CLOSED_ACTUAL_HANDOFF"}')
        self.assertEqual(self.agent.tick()['status'], 'installed')

    def test_running_app_defers_without_build(self):
        self.running = True
        self.assertEqual(self.agent.tick()['status'], 'deferred')
        self.assertFalse(any(c[0] == 'refresh' for c in self.calls))

    def test_busy_installer_lock_defers_without_build(self):
        self.agent.cache.mkdir(parents=True)
        with m.locked(self.agent.cache / 'install.lock'):
            self.assertEqual(self.agent.tick()['status'], 'deferred')
        self.assertFalse(any(c[0] == 'refresh' for c in self.calls))

    def test_pending_queue_coalesces_to_newest_main(self):
        self.running = True
        self.agent.tick()
        self.latest = 'c' * 40
        self.agent.tick()
        self.assertEqual(json.loads(self.agent.queue.read_text())['source_sha'], self.latest)
        self.running = False
        self.assertEqual(self.agent.tick()['source_sha'], self.latest)

    def test_build_failure_retains_queue_and_protected_lease(self):
        self.fail_build = True
        self.assertEqual(self.agent.tick()['status'], 'failed protected')
        self.assertEqual(json.loads(self.agent.queue.read_text())['source_sha'], self.latest)
        lease = json.loads((self.agent.ops / 'validation-lease.json').read_text())
        self.assertEqual(lease['status'], 'FAILED_PROTECTED_REFRESH')
        self.assertEqual(self.agent.tick()['status'], 'deferred')
        self.assertEqual(len([c for c in self.calls if c[0] == 'refresh']), 1)

    def test_disabled_tick_never_fetches_or_builds(self):
        self.agent.state.mkdir(parents=True)
        (self.agent.state / 'disabled').write_text('disabled')
        self.assertEqual(self.agent.tick()['status'], 'disabled')
        self.assertEqual(self.calls, [])

    def test_disable_between_initial_check_and_tick_lock_prevents_admission(self):
        real_locked = m.locked
        pending = [True]
        @contextlib.contextmanager
        def interleaved(path):
            if path == self.agent.state / 'tick.lock' and pending[0]:
                pending[0] = False
                self.assertEqual(self.agent.disable()['status'], 'disabled')
            with real_locked(path):
                yield
        with patch.object(m, 'locked', side_effect=interleaved):
            self.assertEqual(self.agent.tick()['status'], 'disabled')
        self.assertFalse(any(c[0] == 'refresh' for c in self.calls))
        self.assertEqual(json.loads((self.agent.ops / 'installed.json').read_text())['source_sha'], self.installed)

    def test_disable_during_enable_bootstrap_wins_and_next_tick_does_no_work(self):
        def bootstrap(args, **_kwargs):
            if args[1] == 'bootstrap':
                self.assertEqual(self.agent.disable()['status'], 'disabled')
            return None
        with patch.object(self.agent, 'git', side_effect=self.merged_sources()), \
                patch.object(m.subprocess, 'run', side_effect=bootstrap):
            self.agent.enable()
        self.assertTrue((self.agent.state / 'disabled').exists())
        before = list(self.calls)
        self.assertEqual(self.agent.tick()['status'], 'disabled')
        self.assertEqual(self.calls, before)

    def test_install_lock_race_before_refresh_entry_is_retryable_busy(self):
        original = self.agent.perform_refresh
        def contender():
            with m.locked(self.agent.cache / 'install.lock'):
                refresh = self.agent.refresher()
                refresh.app = self.home / 'fake-app'
                return m.Refresh.refresh(refresh)
        with patch.object(self.agent, 'perform_refresh', side_effect=contender):
            self.assertEqual(self.agent.tick()['status'], 'deferred')
        self.assertFalse((self.agent.ops / 'validation-lease.json').exists())
        self.assertTrue(self.agent.queue.exists())
        self.assertEqual(self.agent.tick()['status'], 'installed')

    def test_post_entry_blocking_io_is_protected_not_initial_lock_busy(self):
        with patch.object(self.agent, 'perform_refresh', side_effect=BlockingIOError('transaction write')):
            self.assertEqual(self.agent.tick()['status'], 'failed protected')
        lease = json.loads((self.agent.ops / 'validation-lease.json').read_text())
        self.assertEqual(lease['status'], 'FAILED_PROTECTED_REFRESH')
        self.assertTrue(self.agent.queue.exists())

    def test_lock_busy_type_is_only_acquisition_not_post_yield_body(self):
        path = self.agent.ops / 'fixture.lock'
        with m.locked(path):
            with self.assertRaises(m.LockBusy) as caught:
                with m.locked(path): self.fail('duplicate owner admitted')
        self.assertEqual(caught.exception.path, path)
        original = BlockingIOError('body IO')
        with self.assertRaises(BlockingIOError) as body:
            with m.locked(path): raise original
        self.assertIs(body.exception, original)
        self.assertNotIsInstance(body.exception, m.LockBusy)

    def test_environment_has_rust_path_and_clt(self):
        refresh = self.agent.refresher()
        self.assertEqual(refresh.env['DEVELOPER_DIR'], '/Library/Developer/CommandLineTools')
        self.assertEqual(refresh.env['CARGO_BUILD_JOBS'], '2')
        self.assertEqual(refresh.env['PATH'].split(':')[0], str(self.home / '.cargo/bin'))

    def test_missing_toolchain_reports_action_without_trying_a_build(self):
        agent = m.Agent(self.repo, self.home)
        with patch.object(m.shutil, 'which', return_value=None):
            with self.assertRaisesRegex(RuntimeError, 'Missing cargo'):
                agent.toolchain()

    def test_old_python_refuses_with_action_before_fetch_or_build(self):
        with patch.object(m.sys, 'version_info', (3, 9)):
            with self.assertRaisesRegex(RuntimeError, 'requires Python 3.12'):
                self.agent.tick()
        self.assertEqual(self.calls, [])

    def test_missing_pinned_python_frontdoor_has_actionable_error(self):
        with patch.object(self.agent, 'git', side_effect=self.merged_sources()), \
                patch.object(m.sys, 'version_info', (3, 14)), \
                patch.object(m.sys, 'executable', str(self.home / 'missing-python')), \
                patch.object(m.subprocess, 'run'):
            self.agent.enable()
        import subprocess
        result = subprocess.run(['/bin/sh', str(self.home / 'bin/grooviq-refresh'), 'status'],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 127)
        self.assertIn('pinned Python 3.12+', result.stderr)

    def test_refresh_environment_never_restores_private_signing_configuration(self):
        with patch.dict('os.environ', {'APPLE_PASSWORD': 'fake-test-value'}):
            agent = m.Agent(self.repo, self.home)
            self.assertTrue('APPLE_PASSWORD' not in agent.refresher().env,
                            'private signing configuration was restored')

    def test_disable_does_not_bootout_an_active_original_coordinator(self):
        self.agent.state.mkdir(parents=True)
        m.atomic_json(self.agent.state / 'registration.json', {'source_sha': self.latest})
        with m.locked(self.agent.state / 'tick.lock'), patch.object(m.subprocess, 'run') as run:
            self.assertEqual(self.agent.disable()['status'], 'disabled')
            run.assert_not_called()
        self.assertTrue((self.agent.state / 'disabled').exists())

    def test_registration_rejects_unmerged_current_script_before_writes(self):
        with patch.object(m.subprocess, 'run'), self.assertRaisesRegex(RuntimeError, 'merged'):
            self.agent.enable()
        self.assertFalse((self.home / 'Library/LaunchAgents' / (m.LABEL + '.plist')).exists())
        self.assertFalse((self.home / 'bin/grooviq-refresh').exists())

    def merged_sources(self):
        original = self.agent.git
        def git(*args):
            if args[0] == 'show':
                return MODULE.with_name(args[1].split('/')[-1]).read_bytes()
            return original(*args)
        return git

    def test_enable_snapshots_merged_bytes_and_exposes_status_disable_without_build(self):
        with patch.object(self.agent, 'git', side_effect=self.merged_sources()), \
                patch.object(m.sys, 'version_info', (3, 14)), \
                patch.object(m.subprocess, 'run') as run:
            result = self.agent.enable()
            self.assertEqual(result['status'], 'enabled')
            self.assertTrue(self.agent.status()['enabled'])
            plist = self.home / 'Library/LaunchAgents' / (m.LABEL + '.plist')
            data = m.plistlib.loads(plist.read_bytes())
            self.assertEqual(data['StartInterval'], 300)
            self.assertFalse(data['RunAtLoad'])
            self.assertNotIn('KeepAlive', data)
            self.assertEqual(data['ProgramArguments'][-1], 'tick')
            version = self.agent.state / 'versions' / self.latest
            self.assertEqual((version / 'refresh_agent.py').read_bytes(), MODULE.read_bytes())
            self.assertTrue((self.home / 'bin/grooviq-refresh').stat().st_mode & 0o100)
            self.assertEqual(self.agent.disable()['status'], 'disabled')
            self.assertFalse(self.agent.status()['enabled'])
            self.assertTrue(version.exists())
            self.assertEqual(run.call_count, 2)
        self.assertFalse(any(c[0] == 'refresh' for c in self.calls))

    def test_foreign_frontdoor_is_preserved_and_not_registered(self):
        front = self.home / 'bin/grooviq-refresh'
        front.parent.mkdir()
        front.write_text('foreign owner')
        with patch.object(self.agent, 'git', side_effect=self.merged_sources()), \
                patch.object(m.sys, 'version_info', (3, 14)), \
                patch.object(m.subprocess, 'run') as run:
            with self.assertRaisesRegex(RuntimeError, 'foreign'):
                self.agent.enable()
            run.assert_not_called()
        self.assertEqual(front.read_text(), 'foreign owner')

    def test_failed_registration_keeps_disabled_and_never_builds(self):
        with patch.object(self.agent, 'git', side_effect=self.merged_sources()), \
                patch.object(m.sys, 'version_info', (3, 14)), \
                patch.object(m.subprocess, 'run', side_effect=OSError('bootstrap unavailable')):
            with self.assertRaises(OSError): self.agent.enable()
        self.assertTrue((self.agent.state / 'disabled').exists())
        self.assertEqual(self.agent.tick()['status'], 'disabled')
        self.assertFalse(any(c[0] == 'refresh' for c in self.calls))

    def test_foreign_operation_replacement_is_not_unlinked(self):
        def changed():
            (self.agent.ops / 'validation-lease.json').write_text('{"nonce":"foreign"}')
            return {'status': 'installed'}
        with patch.object(self.agent, 'perform_refresh', side_effect=changed):
            self.assertEqual(self.agent.tick()['status'], 'failed protected')
        self.assertEqual(json.loads((self.agent.ops / 'validation-lease.json').read_text())['nonce'], 'foreign')


if __name__ == '__main__':
    unittest.main(verbosity=2)
