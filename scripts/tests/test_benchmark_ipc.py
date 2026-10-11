#!/usr/bin/env python3
"""Regression test: scripts/benchmark-ipc.py never reaches the primary runner implicitly.

WHY THIS FILE EXISTS. benchmark-ipc.py used to default --runner-url to
http://localhost:9876, the PRIMARY runner, which hosts the operator's live
sessions. A bare `python scripts/benchmark-ipc.py` therefore sent several
hundred requests to it. scripts/perf-harness.mjs refuses that port
(PRIMARY_RUNNER_PORT); this script now refuses to reach it unless the operator
names it in --runner-url. Plan 2026-10-11-terminal-page-performance-kpis-across-setups,
Phase 6 ("Close the hazardous neighbour").

What is pinned, with literals spelled out here rather than read back from the
script's own constants:

  * no --runner-url -> exit 2, a message naming port 9876, the primary runner
    and the explicit override, and NO request attempted: run_benchmarks is
    never reached, and the CLI refuses with httpx made unimportable;
  * an explicit --runner-url naming port 9876 is accepted (that is the
    override) with a warning on stderr; any other explicit URL is accepted
    without one;
  * a target that is not an http(s) URL with a host and a valid port -> exit 2
    before any request.

No runner, no network and no httpx are needed: every in-process test replaces
run_benchmarks, and the end-to-end test only exercises the refusal.

Run (standard library only):
  python -m unittest scripts/tests/test_benchmark_ipc.py
  python scripts/tests/test_benchmark_ipc.py
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "benchmark-ipc.py"
MODULE_NAME = "benchmark_ipc_under_test"


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


class BenchmarkIpcTargetTest(unittest.TestCase):
    def setUp(self):
        self.mod = load_script()
        self.addCleanup(sys.modules.pop, MODULE_NAME, None)
        self.calls: list[tuple[str, int, int]] = []

        def fake_run_benchmarks(runner_url, iterations, warmup):
            self.calls.append((runner_url, iterations, warmup))
            return 0

        patcher = mock.patch.object(self.mod, "run_benchmarks", side_effect=fake_run_benchmarks)
        patcher.start()
        self.addCleanup(patcher.stop)

    def run_main(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = self.mod.main(list(argv))
        return code, out.getvalue(), err.getvalue()

    def test_no_runner_url_is_refused_before_any_request(self):
        with mock.patch.dict(sys.modules, {"httpx": None}):
            code, out, err = self.run_main()
        self.assertEqual(code, 2)
        self.assertEqual(self.calls, [], "a refused target must never reach run_benchmarks")
        self.assertEqual(out, "")
        self.assertIn("refusing to default to port 9876", err)
        self.assertIn("primary runner", err)
        self.assertIn("--runner-url http://127.0.0.1:9876", err, "the refusal must name the explicit override")

    def test_explicit_primary_url_is_the_override(self):
        for url in ("http://127.0.0.1:9876", "http://localhost:9876", "https://127.0.0.1:9876"):
            with self.subTest(runner_url=url):
                self.calls.clear()
                code, _out, err = self.run_main("--runner-url", url)
                self.assertEqual(code, 0)
                self.assertEqual(self.calls, [(url, 100, 10)])
                self.assertIn("benchmarking the PRIMARY runner on port 9876", err)

    def test_explicit_non_primary_url_is_accepted_without_a_warning(self):
        for url in ("http://127.0.0.1:9877", "http://127.0.0.1:9899", "http://127.0.0.1"):
            with self.subTest(runner_url=url):
                self.calls.clear()
                code, _out, err = self.run_main("--runner-url", url, "-n", "5", "-w", "1")
                self.assertEqual(code, 0)
                self.assertEqual(self.calls, [(url, 5, 1)])
                self.assertEqual(err, "")

    def test_malformed_target_is_refused_before_any_request(self):
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
                code, out, err = self.run_main("--runner-url", bad)
                self.assertEqual(code, 2)
                self.assertEqual(self.calls, [])
                self.assertEqual(out, "")
                self.assertIn("--runner-url", err)


class BenchmarkIpcCliTest(unittest.TestCase):
    def test_cli_refuses_the_implicit_primary_without_httpx(self):
        """The real entry point, run as __main__, refuses with httpx unimportable."""
        driver = (
            "import runpy, sys\n"
            "sys.modules['httpx'] = None\n"
            "sys.argv = [sys.argv[1]]\n"
            "runpy.run_path(sys.argv[0], run_name='__main__')\n"
        )
        proc = subprocess.run(
            [sys.executable, "-I", "-c", driver, str(SCRIPT)],
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(proc.returncode, 2, proc.stdout + proc.stderr)
        self.assertEqual(proc.stdout, "")
        self.assertIn("refusing to default to port 9876", proc.stderr)
        self.assertIn("--runner-url http://127.0.0.1:9876", proc.stderr)


if __name__ == "__main__":
    unittest.main()
