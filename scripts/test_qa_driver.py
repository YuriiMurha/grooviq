"""Exercise the public QA CLI against a private disposable protocol peer."""
from pathlib import Path
import json
import socket
import subprocess
import sys
import tempfile
import threading
import unittest

DRIVER = Path(__file__).with_name("qa-driver.py")


class DriverTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="gq", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        (self.home / ".qa-driver").mkdir()
        self.sentinel = self.home / "caller-data"
        self.sentinel.write_text("keep")

    def exchange(self, *args):
        received = []
        with socket.socket(socket.AF_UNIX) as listener:
            listener.bind(str(self.home / ".qa-driver/control.sock"))
            listener.listen(1)
            listener.settimeout(5)

            def serve():
                with listener.accept()[0] as peer:
                    peer.settimeout(5)
                    data = bytearray()
                    while chunk := peer.recv(4096):
                        data.extend(chunk)
                    received.append(json.loads(data))
                    peer.sendall(b'{"ok":true}')

            worker = threading.Thread(target=serve)
            worker.start()
            try:
                result = subprocess.run(
                    [sys.executable, str(DRIVER), "--home", str(self.home), *args],
                    capture_output=True, text=True, timeout=8,
                )
            finally:
                worker.join(timeout=6)
            self.assertFalse(worker.is_alive())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), {"ok": True})
        self.assertEqual(self.sentinel.read_text(), "keep")
        return received

    def test_guarded_quit_is_distinct_from_forced_shutdown(self):
        self.assertEqual(self.exchange("--quit"), [{"quit": True}])

    def test_forced_shutdown_remains_available(self):
        self.assertEqual(self.exchange("--shutdown"), [{"shutdown": True}])

    def test_conflicting_operations_fail_before_connecting(self):
        result = subprocess.run(
            [sys.executable, str(DRIVER), "--home", str(self.home), "--quit", "--shutdown"],
            capture_output=True, text=True, timeout=5,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("not allowed", result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertEqual(self.sentinel.read_text(), "keep")


if __name__ == "__main__":
    unittest.main()
