"""Exercise the real sandbox: python3 -m unittest discover -s scripts/tests."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import unittest.mock

ROOT = Path(__file__).resolve().parents[2]


class SandboxTests(unittest.TestCase):
    def run_sandbox(self, code, *args):
        return subprocess.run([str(ROOT / "scripts/dev"), "python3", "-c", code, *args],
                              capture_output=True, text=True)

    def test_commands_keep_literal_arguments(self):
        value = "spaces ' quotes $(exit 99) `exit 98`"
        result = self.run_sandbox("import sys; print(sys.argv[1])", value)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), value, "the launcher changed a command argument")

    def test_command_failure_is_returned(self):
        result = self.run_sandbox("raise SystemExit(37)")
        self.assertEqual(result.returncode, 37, "the launcher concealed a command failure")

    def test_private_home_files_cannot_be_read(self):
        with tempfile.TemporaryDirectory(dir=Path.home()) as private:
            secret = Path(private) / "sentinel"
            secret.write_text("private fixture")
            result = self.run_sandbox("import pathlib,sys; pathlib.Path(sys.argv[1]).read_text()", str(secret))
            self.assertNotEqual(result.returncode, 0, "the sandbox exposed a private home file")

    def test_writes_outside_the_project_are_denied(self):
        with tempfile.TemporaryDirectory(dir=Path.home()) as private:
            target = Path(private) / "sentinel"
            result = self.run_sandbox("import pathlib,sys; pathlib.Path(sys.argv[1]).write_text('escaped')", str(target))
            self.assertNotEqual(result.returncode, 0, "the sandbox allowed a write outside the project")
            self.assertFalse(target.exists(), "the denied write created a file")

    def test_project_files_are_writable(self):
        with tempfile.TemporaryDirectory(dir=ROOT / ".wisp-dev") as scratch:
            target = Path(scratch) / "sentinel"
            result = self.run_sandbox("import pathlib,sys; pathlib.Path(sys.argv[1]).write_text('inside')", str(target))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(target.read_text(), "inside", "the project write did not complete")

    def test_inherited_secrets_are_not_forwarded(self):
        with unittest.mock.patch.dict(os.environ, {"WISP_TEST_NOT_A_SECRET": "visible", "PRIVATE_TEST_TOKEN": "secret"}):
            result = self.run_sandbox("import os; assert 'PRIVATE_TEST_TOKEN' not in os.environ, 'a private shell token leaked'; assert os.environ['WISP_TEST_NOT_A_SECRET'] == 'visible', 'explicit WISP configuration was lost'")
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_local_tcp_requests_can_complete(self):
        result = self.run_sandbox("""
import socket
with socket.socket() as listener:
    listener.bind(('127.0.0.1', 0))
    listener.listen()
    with socket.create_connection(listener.getsockname(), timeout=5) as client:
        connection, _ = listener.accept()
        with connection:
            client.sendall(b'wisp')
            assert connection.recv(4) == b'wisp', 'local traffic was lost'
""")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_direct_external_network_is_denied(self):
        result = self.run_sandbox("""
import errno, socket
with socket.socket() as client:
    client.settimeout(2)
    try:
        client.connect(('192.0.2.1', 80))
    except OSError as error:
        assert error.errno in (errno.EPERM, errno.EACCES, errno.ENETUNREACH), error
    else:
        raise AssertionError('direct external traffic escaped the proxy')
""")
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
