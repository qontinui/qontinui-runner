#!/usr/bin/env python3
"""Regression test: scripts/benchmark-ipc.py never reaches the primary runner implicitly.

WHY THIS FILE EXISTS. benchmark-ipc.py used to default --runner-url to
http://localhost:9876, the PRIMARY runner, which hosts the operator's live
sessions. A bare `python scripts/benchmark-ipc.py` therefore sent several
hundred requests to it. It now has no default target, and it refuses port
9876 unless --allow-primary comes with it. The refusal is modelled on the
PRIMARY_RUNNER_PORT check in scripts/perf-harness.mjs, which has no override at
all. Plan 2026-10-11-terminal-page-performance-kpis-across-setups, Phase 6
("Close the hazardous neighbour").

What is pinned, with literals spelled out here rather than read back from the
script's own constants:

  * no --runner-url -> exit 2, and a message naming port 9876, the primary
    runner and the override;
  * a --runner-url naming port 9876 without --allow-primary -> exit 2;
  * a --runner-url naming port 9876 with --allow-primary -> accepted, with a
    warning on stderr. Any other valid URL is accepted without a warning;
  * a target that is not an http(s) URL with a host and a valid port -> exit 2;
  * run_benchmarks, the public function that builds the HTTP client, refuses
    the same targets itself, so the CLI is not the only guarded path;
  * every refusal happens before ANY connection attempt.
    socket.create_connection, socket.socket.connect and
    socket.socket.connect_ex are replaced by recorders that note the attempt
    and then raise ConnectionRefusedError, and the record must stay empty.
    A request sent before the refusal is caught even when the code swallows
    the error, because the attempt is noted before the raise. The recorders
    are themselves shown catching a swallowed urllib request (and an httpx
    one where httpx is installed);
  * the refusal needs no httpx: the CLI test runs with httpx unimportable.

No runner and no network are needed. Accepted targets never reach the network
either, because every in-process test replaces _run_benchmarks, the private
function that sends the requests.

Run (standard library only):
  python -m unittest scripts/tests/test_benchmark_ipc.py
  python scripts/tests/test_benchmark_ipc.py
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import socket
import subprocess
import sys
import tempfile
import unittest
import urllib.request
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "benchmark-ipc.py"
MODULE_NAME = "benchmark_ipc_under_test"

#: Port 9 (discard) on loopback: the address the recorder self-tests aim at.
#: Nothing is ever sent there, because the recorders refuse every attempt.
UNREACHED_URL = "http://127.0.0.1:9/"


def load_script():
    """Import scripts/benchmark-ipc.py (its hyphenated name is not importable)."""
    spec = importlib.util.spec_from_file_location(MODULE_NAME, SCRIPT)
    module = importlib.util.module_from_spec(spec)
    # Registered before exec: @dataclass resolves its string annotations
    # through sys.modules[cls.__module__].
    sys.modules[MODULE_NAME] = module
    try:
        spec.loader.exec_module(module)
    except BaseException:
        sys.modules.pop(MODULE_NAME, None)
        raise
    return module


class ConnectionRecorder:
    """Stand-ins for the stdlib's outbound-connection entry points.

    Each one notes the attempt and then raises ConnectionRefusedError, so
    nothing leaves the process and a swallowed error still leaves a record.
    """

    def __init__(self) -> None:
        self.attempts: list[str] = []

    def _refuse(self, kind: str, address) -> None:
        self.attempts.append(f"{kind} {address!r}")
        raise ConnectionRefusedError(f"test_benchmark_ipc: network disabled ({kind} {address!r})")

    def patchers(self) -> list:
        def create_connection(address, *args, **kwargs):
            self._refuse("socket.create_connection", address)

        def connect(sock, address):
            self._refuse("socket.socket.connect", address)

        def connect_ex(sock, address):
            self._refuse("socket.socket.connect_ex", address)

        return [
            mock.patch.object(socket, "create_connection", create_connection),
            mock.patch.object(socket.socket, "connect", connect),
            mock.patch.object(socket.socket, "connect_ex", connect_ex),
        ]


def start_recorder(case: unittest.TestCase) -> ConnectionRecorder:
    recorder = ConnectionRecorder()
    for patcher in recorder.patchers():
        patcher.start()
        case.addCleanup(patcher.stop)
    return recorder


class ConnectionRecorderSelfTest(unittest.TestCase):
    """The recorders are a bespoke verifier, so they are shown catching a request."""

    def setUp(self):
        self.recorder = start_recorder(self)

    def test_catches_a_swallowed_urllib_request(self):
        # No proxy, so the one attempt is aimed at UNREACHED_URL itself.
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        try:
            opener.open(UNREACHED_URL, timeout=5)
        except Exception:
            pass
        self.assertNotEqual(self.recorder.attempts, [])
        self.assertIn("127.0.0.1", self.recorder.attempts[0])

    def test_catches_a_swallowed_httpx_request(self):
        try:
            import httpx
        except ImportError:
            self.skipTest("httpx is not installed; the urllib case still covers the recorder")
        try:
            httpx.get(UNREACHED_URL, timeout=5, trust_env=False)
        except Exception:
            pass
        self.assertNotEqual(self.recorder.attempts, [])
        self.assertIn("127.0.0.1", self.recorder.attempts[0])


class BenchmarkIpcTargetTest(unittest.TestCase):
    def setUp(self):
        self.mod = load_script()
        self.addCleanup(sys.modules.pop, MODULE_NAME, None)
        self.recorder = start_recorder(self)
        self.calls: list[tuple[str, int, int]] = []

        def fake_run_benchmarks(runner_url, iterations, warmup):
            self.calls.append((runner_url, iterations, warmup))
            return 0

        patcher = mock.patch.object(self.mod, "_run_benchmarks", side_effect=fake_run_benchmarks)
        patcher.start()
        self.addCleanup(patcher.stop)

    def run_main(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = self.mod.main(list(argv))
        return code, out.getvalue(), err.getvalue()

    def assert_refused_before_any_connection(self, code: int, out: str) -> None:
        self.assertEqual(code, 2)
        self.assertEqual(self.calls, [], "a refused target must never reach _run_benchmarks")
        self.assertEqual(self.recorder.attempts, [], "a refused target must attempt no connection")
        self.assertEqual(out, "")

    def test_no_runner_url_is_refused_before_any_connection(self):
        code, out, err = self.run_main()
        self.assert_refused_before_any_connection(code, out)
        self.assertIn("refusing to default to port 9876", err)
        self.assertIn("primary runner", err)
        self.assertIn(
            "--runner-url http://127.0.0.1:9876 --allow-primary",
            err,
            "the refusal must name the override",
        )

    def test_primary_url_without_allow_primary_is_refused(self):
        for url in ("http://127.0.0.1:9876", "http://localhost:9876", "https://127.0.0.1:9876"):
            with self.subTest(runner_url=url):
                self.calls.clear()
                code, out, err = self.run_main("--runner-url", url)
                self.assert_refused_before_any_connection(code, out)
                self.assertIn("names port 9876, the primary runner", err)
                self.assertIn("--allow-primary", err)

    def test_primary_url_with_allow_primary_is_the_override(self):
        for url in ("http://127.0.0.1:9876", "http://localhost:9876", "https://127.0.0.1:9876"):
            with self.subTest(runner_url=url):
                self.calls.clear()
                code, _out, err = self.run_main("--runner-url", url, "--allow-primary")
                self.assertEqual(code, 0)
                self.assertEqual(self.calls, [(url, 100, 10)])
                self.assertIn("benchmarking the PRIMARY runner on port 9876", err)

    def test_non_primary_url_is_accepted_without_a_warning(self):
        for argv in (
            ("--runner-url", "http://127.0.0.1:9877"),
            ("--runner-url", "http://127.0.0.1:9899"),
            ("--runner-url", "http://127.0.0.1"),
            ("--runner-url", "http://127.0.0.1:9877", "--allow-primary"),
        ):
            with self.subTest(argv=argv):
                self.calls.clear()
                code, _out, err = self.run_main(*argv, "-n", "5", "-w", "1")
                self.assertEqual(code, 0)
                self.assertEqual(self.calls, [(argv[1], 5, 1)])
                self.assertEqual(err, "")

    def test_malformed_target_is_refused_before_any_connection(self):
        for bad in (
            "127.0.0.1:9877",  # no scheme
            "localhost:9876",  # no scheme; must not sneak the primary through
            "ftp://127.0.0.1:9877",  # not http(s)
            "http://",  # no host
            "http://127.0.0.1:99999",  # port out of range
            "",  # empty
        ):
            with self.subTest(runner_url=bad):
                self.calls.clear()
                code, out, err = self.run_main("--runner-url", bad, "--allow-primary")
                self.assert_refused_before_any_connection(code, out)
                self.assertIn("--runner-url", err)

    def test_run_benchmarks_refuses_on_its_own(self):
        """The public function guards itself; main() is not the only door."""
        for runner_url in (None, "http://127.0.0.1:9876", "localhost:9877"):
            with self.subTest(runner_url=runner_url):
                self.calls.clear()
                with self.assertRaises(self.mod.TargetRefused):
                    self.mod.run_benchmarks(runner_url, 5, 1)
                self.assertEqual(self.calls, [])
                self.assertEqual(self.recorder.attempts, [])

        self.calls.clear()
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            code = self.mod.run_benchmarks("http://127.0.0.1:9876", 5, 1, allow_primary=True)
        self.assertEqual(code, 0)
        self.assertEqual(self.calls, [("http://127.0.0.1:9876", 5, 1)])
        self.assertIn("benchmarking the PRIMARY runner on port 9876", err.getvalue())


#: Runs the real script as __main__ in a fresh `python -I`, with httpx made
#: unimportable and the same three connection entry points replaced by
#: recorders. The record is printed as the last stderr line, after the script
#: has run, and the script's own exit status is passed through.
CLI_DRIVER = r"""
import json, runpy, socket, sys

attempts = []

def refuse(kind, address):
    attempts.append(f"{kind} {address!r}")
    raise ConnectionRefusedError(f"test_benchmark_ipc: network disabled ({kind})")

def create_connection(address, *args, **kwargs):
    refuse("socket.create_connection", address)

def connect(sock, address):
    refuse("socket.socket.connect", address)

def connect_ex(sock, address):
    refuse("socket.socket.connect_ex", address)

socket.create_connection = create_connection
socket.socket.connect = connect
socket.socket.connect_ex = connect_ex
sys.modules["httpx"] = None
script = sys.argv[1]
sys.argv = [script, *sys.argv[2:]]
code = 0
try:
    runpy.run_path(script, run_name="__main__")
except SystemExit as exit_:
    code = exit_.code
finally:
    sys.stderr.write("\nCONNECTION-ATTEMPTS " + json.dumps(attempts) + "\n")
sys.exit(code)
"""

ATTEMPTS_PREFIX = "CONNECTION-ATTEMPTS "


def run_cli(script: Path, *argv: str) -> tuple[int, str, str, list[str]]:
    proc = subprocess.run(
        [sys.executable, "-I", "-c", CLI_DRIVER, str(script), *argv],
        capture_output=True,
        text=True,
        timeout=60,
    )
    lines = proc.stderr.splitlines()
    records = [line for line in lines if line.startswith(ATTEMPTS_PREFIX)]
    if len(records) != 1:
        raise AssertionError(f"driver printed {len(records)} attempt records:\n{proc.stderr}")
    attempts = json.loads(records[0][len(ATTEMPTS_PREFIX) :])
    stderr = "\n".join(line for line in lines if not line.startswith(ATTEMPTS_PREFIX))
    return proc.returncode, proc.stdout, stderr, attempts


class BenchmarkIpcCliTest(unittest.TestCase):
    def test_driver_records_a_swallowed_request(self):
        """The driver's recorders are a bespoke verifier too, so show them working."""
        with tempfile.TemporaryDirectory() as tmp:
            probe = Path(tmp) / "probe.py"
            probe.write_text(
                "import urllib.request\n"
                "opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))\n"
                "try:\n"
                f"    opener.open({UNREACHED_URL!r}, timeout=5)\n"
                "except Exception:\n"
                "    pass\n",
                encoding="utf-8",
            )
            code, _out, stderr, attempts = run_cli(probe)
        self.assertEqual(code, 0, stderr)
        self.assertNotEqual(attempts, [])
        self.assertIn("127.0.0.1", attempts[0])

    def test_cli_refuses_the_implicit_primary_without_httpx(self):
        """The real entry point, run as __main__, refuses with httpx unimportable."""
        code, out, stderr, attempts = run_cli(SCRIPT)
        self.assertEqual(code, 2, out + stderr)
        self.assertEqual(out, "")
        self.assertEqual(attempts, [])
        self.assertIn("refusing to default to port 9876", stderr)
        self.assertIn("--runner-url http://127.0.0.1:9876 --allow-primary", stderr)

    def test_cli_refuses_a_bare_primary_url_without_httpx(self):
        code, out, stderr, attempts = run_cli(SCRIPT, "--runner-url", "http://localhost:9876")
        self.assertEqual(code, 2, out + stderr)
        self.assertEqual(out, "")
        self.assertEqual(attempts, [])
        self.assertIn("names port 9876, the primary runner", stderr)


if __name__ == "__main__":
    unittest.main()
