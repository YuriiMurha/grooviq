import contextlib
import importlib.util
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('perf_fetch', Path(__file__).with_name('perf-fetch.py'))
fetch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fetch)


class FetchTests(unittest.TestCase):
    def test_locked_complete_graph_and_finite_network(self):
        calls = []
        def runner(command, **kwargs):
            calls.append((command, kwargs))
            return subprocess.CompletedProcess(command, 0)
        self.assertEqual(fetch.prepare(run=runner), 0)
        self.assertEqual(len(calls), 1)
        command, options = calls[0]
        self.assertEqual(command, ['cargo', 'fetch', '--locked'])
        self.assertEqual(options['timeout'], 180)
        self.assertTrue(options['check'])
        self.assertEqual(options['env']['CARGO_NET_RETRY'], '0')
        self.assertEqual(options['env']['CARGO_HTTP_TIMEOUT'], '30')
        self.assertEqual(options['env']['CARGO_NET_OFFLINE'], 'false')

    def test_dns_and_nonzero_stop_without_retry_or_measurement(self):
        for message in ['Could not resolve host: index.crates.io', 'invalid lockfile']:
            with self.subTest(message=message), tempfile.TemporaryDirectory() as tmp:
                fake = Path(tmp) / 'cargo.py'
                calls = Path(tmp) / 'calls'
                fake.write_text('import pathlib, sys\n'
                                f'pathlib.Path({str(calls)!r}).write_text(repr(sys.argv[1:]))\n'
                                f'print({message!r}, file=sys.stderr)\nsys.exit(101)\n')
                errors = io.StringIO()
                with contextlib.redirect_stderr(errors):
                    result = fetch.prepare(cargo=[sys.executable, str(fake)])
                self.assertNotEqual(result, 0)
                self.assertEqual(calls.read_text(), "['fetch', '--locked']")
                self.assertIn('infrastructure', errors.getvalue())
                self.assertIn('no benchmark results', errors.getvalue())

    def test_timeout_is_infrastructure_failure_and_one_attempt(self):
        calls = []
        def runner(command, **kwargs):
            calls.append(command)
            raise subprocess.TimeoutExpired(command, kwargs['timeout'])
        with contextlib.redirect_stderr(io.StringIO()) as errors:
            self.assertNotEqual(fetch.prepare(run=runner), 0)
        self.assertEqual(len(calls), 1)
        self.assertIn('timed out', errors.getvalue())

    def test_missing_cargo_fails_setup(self):
        def runner(command, **kwargs):
            raise FileNotFoundError('cargo')
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertNotEqual(fetch.prepare(run=runner), 0)


if __name__ == '__main__':
    unittest.main()
