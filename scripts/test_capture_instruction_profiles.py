import importlib.util
import json
import io
import shutil
import os
from pathlib import Path
import subprocess
import signal
import sys
import time
import threading
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('capture', Path(__file__).with_name('capture-instruction-profiles.py'))
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)


class CaptureTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        self.git('init', '-q')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.git('config', 'user.name', 'Fixture')
        (self.repo / 'workload').write_text('fixed workload')
        (self.repo / 'Cargo.lock').write_text('base lock')
        (self.repo / 'tests/golden/samples').mkdir(parents=True)
        (self.repo / 'tests/golden/samples/sample').write_text('owned fixture')
        self.git('add', '.')
        self.git('commit', '-qm', 'base')
        self.base = self.git('rev-parse', 'HEAD')
        (self.repo / 'Cargo.lock').write_text('head lock')
        self.git('add', 'Cargo.lock')
        self.git('commit', '-qm', 'head')
        self.head = self.git('rev-parse', 'HEAD')
        self.branch = self.git('branch', '--show-current')
        for name, value in [('BASE', self.base), ('HEAD', self.head), ('PINS', {'workload': self.git('rev-parse', 'HEAD:workload')})]:
            p = patch.object(c, name, value)
            p.start()
            self.addCleanup(p.stop)
        for name, value in [('system', 'Linux'), ('machine', 'x86_64')]:
            p = patch.object(c.platform, name, return_value=value)
            p.start()
            self.addCleanup(p.stop)
        self.out = self.root / 'evidence'
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.mode = self.root / 'mode'
        self.mode.write_text('regress')
        # Every measurement/build boundary is an actual native child process, never Cargo.
        runner = '''#!/usr/bin/env python3
import json, os, pathlib, sys, time
root = pathlib.Path(__file__).resolve().parent.parent
mode = (root / 'mode').read_text()
if mode == 'tracking' and os.environ.get('RUNNER_TRACKING_ID') != 'fixture-tracking': sys.exit(98)
name = pathlib.Path(sys.argv[0]).name
target = pathlib.Path(os.environ.get('CARGO_TARGET_DIR', root / 'unused'))
if name == 'rustc': print('rustc 1.100.0 0ef1bc386059596dada8fb65b50425aab2ea637a')
elif name == 'gungraun-runner': print('gungraun-runner 0.20.0')
elif name == 'valgrind': print('valgrind-3.22.0')
elif name == 'cargo':
 if '-V' in sys.argv: print('cargo 1.100.0')
 else:
  if mode == 'compile': sys.exit(101)
  exe = target / 'harness'
  exe.write_bytes(pathlib.Path(__file__).read_bytes()); exe.chmod(0o700)
  print(json.dumps({'reason':'compiler-artifact','target':{'name':'instructions'},'executable':str(exe)}))
elif name == 'harness':
 # Gungraun 0.20 defaults direct launches without Cargo's --bench marker to
 # successful test mode: no Valgrind, metrics or output files (runner/meta.rs).
 if '--bench' not in sys.argv or mode == 'no-measurements': sys.exit(0)
 head = '--baseline=base' in sys.argv
 # Verify BASE evidence was saved before HEAD can overwrite its binary.
 evidence = root / 'evidence/base/binary-0'
 if head and not evidence.exists(): sys.exit(77)
 if head and mode == 'crash': sys.exit(101)
 binary = target / 'build/pkg/out/exact-binary'
 binary.parent.mkdir(parents=True, exist_ok=True)
 binary.write_bytes(b'\\x7fELF' + (b'HEAD' if head else b'BASE'))
 for case in ('voices_16','init_4','dark_pad_4','drums_4ch','beat_16ch'):
  if head and mode == 'missing' and case == 'beat_16ch': continue
  p = target / case / ('callgrind.case.out' if head else 'callgrind.case.out.base@base')
  p.parent.mkdir(exist_ok=True)
  cmd = binary if mode != 'wrongbinary' else root / 'outside'
  p.write_text(f'cmd: {cmd}\\nevents: Ir\\nsummary: {0 if mode == "zero" else 42}\\n')
  if mode == 'stale': os.utime(p, ns=(1,1))
 sys.exit(3 if head else 0)
else: print('annotation / symbols')
'''
        # harness resolves root through a symlink-free target location, so embed fixture root.
        runner = runner.replace("root = pathlib.Path(__file__).resolve().parent.parent", f"root = pathlib.Path({str(self.root)!r})")
        for tool in ('cargo', 'rustc', 'gungraun-runner', 'valgrind', 'dpkg-query', 'nm', 'objdump', 'callgrind_annotate'):
            p = self.bin / tool
            p.write_text(runner)
            p.chmod(0o700)
        p = patch.dict(os.environ, {'PATH': str(self.bin) + os.pathsep + os.environ['PATH']})
        p.start()
        self.addCleanup(p.stop)

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.repo).decode().strip()

    def capture(self):
        return c.capture(self.repo, self.base, self.head, self.out)

    def test_invalid_options_execute_nothing(self):
        with patch.object(c, 'run', side_effect=AssertionError('process invoked')):
            for value in (';touch /tmp/no', 'a'*39, '--help'):
                with self.assertRaises(c.CaptureError):
                    c.validate(self.repo, value, self.head, self.out)
        self.assertFalse(self.out.exists())

    def test_dirty_and_workload_guard(self):
        (self.repo / 'stray').write_text('preserve')
        with self.assertRaises(c.CaptureError): self.capture()
        self.assertEqual((self.repo / 'stray').read_text(), 'preserve')
        (self.repo / 'stray').unlink()
        with patch.object(c, 'PINS', {'workload': '0'*40}):
            with self.assertRaises(c.CaptureError): self.capture()
        self.assertFalse(self.out.exists())

    def test_regression_preserves_base_hashes_and_ref(self):
        report = self.capture()
        self.assertEqual(report['status'], 'completed_with_regression_unmatched_kit')
        self.assertEqual([r['exit'] for r in report['runs']], [0, 3])
        self.assertNotEqual(report['runs'][0]['lock_blob'], report['runs'][1]['lock_blob'])
        self.assertEqual((self.out / 'base/binary-0').read_bytes(), b'\x7fELFBASE')
        self.assertEqual((self.out / 'head/binary-0').read_bytes(), b'\x7fELFHEAD')
        for path, item in report['files'].items():
            self.assertEqual(c.sha(self.out / path), item['sha256'])
        self.assertEqual(self.git('branch', '--show-current'), self.branch)
        self.assertEqual(self.git('rev-parse', 'HEAD'), self.head)
        self.assertEqual(self.git('status', '--porcelain'), '')

    def test_failures_restore_ref_and_remain_failed(self):
        for mode in ('compile', 'crash', 'missing', 'zero', 'stale', 'wrongbinary', 'no-measurements'):
            with self.subTest(mode=mode):
                self.mode.write_text(mode)
                with self.assertRaises(c.CaptureError): self.capture()
                self.assertEqual(json.loads((self.out / 'manifest.json').read_text())['status'], 'failed')
                self.assertEqual(self.git('branch', '--show-current'), self.branch)
                import shutil
                shutil.rmtree(self.out)

    def test_disk_failure_restores_ref(self):
        with patch.object(c, 'copy_evidence', side_effect=OSError('disk full')):
            with self.assertRaises(OSError): self.capture()
        self.assertEqual(self.git('branch', '--show-current'), self.branch)
        self.assertEqual(json.loads((self.out / 'manifest.json').read_text())['status'], 'failed')

    def test_platform_guard_executes_nothing(self):
        with patch.object(c.platform, 'system', return_value='Darwin'), patch.object(c, 'run', side_effect=AssertionError('process invoked')):
            with self.assertRaises(c.CaptureError): self.capture()

    def test_runner_tracking_retained_without_metadata_dump(self):
        self.mode.write_text('tracking')
        with patch.dict(os.environ, {'RUNNER_TRACKING_ID': 'fixture-tracking', 'CAPTURE_FAKE_TOKEN': 'never-copy-this'}):
            report = self.capture()
        self.assertTrue(report['restoration']['succeeded'])
        manifest = (self.out / 'manifest.json').read_text()
        self.assertNotIn('fixture-tracking', manifest)
        self.assertNotIn('never-copy-this', manifest)

    def test_restore_lock_reports_failed_settlement(self):
        original_run = c.run
        def locked(args, *a, **kw):
            if args == ['git', 'checkout', '--quiet', self.branch]:
                (self.repo / '.git/index.lock').write_text('owned test lock')
            return original_run(args, *a, **kw)
        with patch.object(c, 'run', side_effect=locked):
            with self.assertRaises(c.CaptureError): self.capture()
        report = json.loads((self.out / 'manifest.json').read_text())
        self.assertEqual(report['status'], 'failed_restore')
        self.assertEqual(report['capture_result'], 'completed_with_regression_unmatched_kit')
        self.assertEqual(report['original_ref']['commit'], self.head)
        self.assertEqual(report['settled_ref']['branch'], 'HEAD')
        self.assertFalse(report['restoration']['succeeded'])
        self.assertTrue((self.repo / '.git/index.lock').exists())

    def native_descendant_probe(self, mode):
        pidfile = self.root / 'descendant.pid'
        marker = self.root / 'late-marker'
        child = f"import os,time,pathlib,signal; signal.signal(signal.SIGTERM, signal.SIG_IGN); pathlib.Path({str(pidfile)!r}).write_text(str(os.getpid())); time.sleep(.7); pathlib.Path({str(marker)!r}).write_text('late')"
        parent = f"import subprocess,sys,time; subprocess.Popen([sys.executable,'-c',{child!r}]); time.sleep(3)"
        timer = None
        try:
            if mode == 'timeout':
                with self.assertRaises(subprocess.TimeoutExpired):
                    c.run([sys.executable, '-c', parent], self.root, timeout=.3)
            else:
                with c.cancellation(.3 if mode == 'deadline' else 3):
                    if mode != 'deadline':
                        timer = threading.Timer(.3, lambda: os.kill(os.getpid(), mode))
                        timer.start()
                    with self.assertRaises(c.CaptureError):
                        if mode == signal.SIGTERM:
                            with (self.root / 'child.log').open('xb') as log:
                                c.save_command([sys.executable, '-c', parent], self.root, None, log, self.root, timeout=2)
                        else:
                            c.run([sys.executable, '-c', parent], self.root, timeout=2)
            self.assertTrue(pidfile.exists(), 'native descendant must actually start')
            pid = int(pidfile.read_text())
            state = subprocess.run(['ps', '-p', str(pid), '-o', 'stat='], capture_output=True, text=True).stdout.strip()
            self.assertTrue(not state or state.startswith('Z'), f'descendant remains live: {state}')
            self.assertFalse(marker.exists())
        finally:
            if timer:
                timer.cancel()
                timer.join()
            if pidfile.exists():
                try: os.kill(int(pidfile.read_text()), signal.SIGKILL)
                except ProcessLookupError: pass

    def test_unsettled_process_preserves_target_and_checkout(self):
        try:
            with patch.object(c, 'save_command', side_effect=c.ProcessSettlementError('owned test settlement failure')):
                with self.assertRaises(c.ProcessSettlementError): self.capture()
            report = json.loads((self.out / 'manifest.json').read_text())
            self.assertEqual(report['status'], 'failed_process_cleanup')
            self.assertFalse(report['restoration']['succeeded'])
            self.assertEqual(self.git('rev-parse', '--abbrev-ref', 'HEAD'), 'HEAD')
            self.assertTrue(Path(report['target']).is_dir())
        finally:
            if (self.out / 'manifest.json').exists():
                report = json.loads((self.out / 'manifest.json').read_text())
                shutil.rmtree(report['target'])

    def test_timeout_stops_native_descendant(self):
        self.native_descendant_probe('timeout')

    def test_deadline_stops_native_descendant(self):
        self.native_descendant_probe('deadline')

    def test_sigterm_stops_native_descendant(self):
        self.native_descendant_probe(signal.SIGTERM)

    def test_sigint_stops_native_descendant(self):
        self.native_descendant_probe(signal.SIGINT)

    def test_tooling_change_plans_safety_gate_without_builds(self):
        scripts = self.repo / 'scripts'
        scripts.mkdir()
        shutil.copyfile(Path(__file__).with_name('check-changed.mjs'), scripts / 'check-changed.mjs')
        (self.repo / 'Makefile').write_text('all:\n\ttrue\n')
        self.git('add', 'scripts/check-changed.mjs', 'Makefile')
        self.git('commit', '-qm', 'selector fixture')
        (self.repo / 'Makefile').write_text('all:\n\tfalse\n')
        home = self.root / 'home'
        cargo = home / '.cargo/bin/cargo'
        cargo.parent.mkdir(parents=True)
        cargo.write_text('#!/usr/bin/env python3\nprint(\'{"packages":[]}\')\n')
        cargo.chmod(0o700)
        result = subprocess.run(['node', str(scripts / 'check-changed.mjs'), '--plan', '--base', 'HEAD'],
                                cwd=self.repo, env={**os.environ, 'HOME': str(home)}, text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('instruction capture safety tests: make test-instruction-profiles', result.stdout)
        self.assertNotIn('cargo clippy', result.stdout)
        self.assertNotIn('npm ci', result.stdout)

    def test_output_ownership_and_budget(self):
        outside = self.root / 'outside'
        outside.mkdir()
        (outside / 'keep').write_text('external')
        self.out.symlink_to(outside, target_is_directory=True)
        with self.assertRaises(c.CaptureError): self.capture()
        self.assertEqual((outside / 'keep').read_text(), 'external')
        self.out.unlink()
        self.out.mkdir()
        source = self.root / 'source'
        source.write_bytes(b'12345')
        with patch.object(c, 'LIMIT', 4):
            with self.assertRaises(c.CaptureError): c.copy_evidence(source, self.out / 'copy', self.root, self.out)
        self.assertFalse((self.out / 'copy').exists())
        source.unlink()
        source.symlink_to(outside / 'keep')
        with self.assertRaises(c.CaptureError): c.copy_evidence(source, self.out / 'copy', self.root, self.out)


def run_safety_suite(suite, stream=None):
    expected = suite.countTestCases()
    result = unittest.TextTestRunner(stream=stream, verbosity=2).run(suite)
    return (expected > 0 and result.testsRun == expected and result.wasSuccessful()
            and not result.skipped and not result.expectedFailures and not result.unexpectedSuccesses)


class SafetyGateTests(unittest.TestCase):
    def test_empty_skipped_expected_failure_and_failed_suites_rejected(self):
        class Cases(unittest.TestCase):
            def test_ok(self): pass
            @unittest.skip('must reject')
            def test_skip(self): pass
            @unittest.expectedFailure
            def test_expected(self): self.fail('must reject')
            def test_fail(self): self.fail('must reject')
        for name in ('test_skip', 'test_expected', 'test_fail'):
            self.assertFalse(run_safety_suite(unittest.TestSuite([Cases(name)]), io.StringIO()))
        self.assertFalse(run_safety_suite(unittest.TestSuite(), io.StringIO()))
        self.assertTrue(run_safety_suite(unittest.TestSuite([Cases('test_ok')]), io.StringIO()))


if __name__ == '__main__':
    suite = unittest.defaultTestLoader.loadTestsFromModule(sys.modules[__name__])
    sys.exit(0 if run_safety_suite(suite) else 1)
