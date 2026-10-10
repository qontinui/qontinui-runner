#!/usr/bin/env python3
"""Smoke-test a frozen qontinui-executor over the runner's stdin/stdout protocol.

Used by .github/workflows/build-python-executor.yml after PyInstaller has
produced the executable. A build that freezes but cannot answer the protocol is
not a working executor, so this sends two commands and asserts the answers.

Protocol facts relied on (python-bridge/qontinui_executor.py):
  - One JSON object per stdin line. The main loop dispatches a line ONLY when it
    carries "type": "command"; a bare {"command": "ping"} is silently ignored.
    The probe therefore uses the runner's own framing {type, id, command, params}.
  - stdout carries a {"type": "ready"} line at startup, then for each command a
    {"type": "response", "id": ..., "success": ..., "data": ..., "error": ...}
    line. `ping` additionally prints {"type": "pong", ...} before its response.
  - EOF on stdin ends the `for line in sys.stdin` loop, so closing stdin is the
    shutdown signal. There is no explicit shutdown command.

Assertions: a pong line, the ping response with success == true, and a
models_list response carrying a `success` key (its value depends on which models
are downloaded on the CI host, so only its presence is asserted).

Usage: smoke-frozen-executor.py <path-to-executable>
Env:   SMOKE_TIMEOUT_SECS (default 180) bounds the whole run.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys

PING_ID = "smoke-ping"
MODELS_ID = "smoke-models"


def _command(cmd_id: str, command: str) -> str:
    return json.dumps({"type": "command", "id": cmd_id, "command": command, "params": {}})


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: smoke-frozen-executor.py <path-to-executable>", file=sys.stderr)
        return 2

    exe = os.path.abspath(sys.argv[1])
    if not os.path.isfile(exe):
        print(f"::error::frozen executor not found at {exe}")
        return 1

    timeout = int(os.environ.get("SMOKE_TIMEOUT_SECS", "180"))
    stdin = _command(PING_ID, "ping") + "\n" + _command(MODELS_ID, "models_list") + "\n"

    timed_out = False
    try:
        proc = subprocess.run(
            [exe], input=stdin.encode("utf-8"), capture_output=True, timeout=timeout
        )
        out, err, rc = proc.stdout, proc.stderr, proc.returncode
    except subprocess.TimeoutExpired as exc:
        # The process did not exit on stdin EOF within the bound. Judge what it
        # DID answer; a hang after answering is reported, not hidden.
        timed_out = True
        out, err, rc = exc.stdout or b"", exc.stderr or b"", None

    text = out.decode("utf-8", errors="replace")
    print("---- executor stdout ----")
    print(text)
    print("---- executor stderr (last 4000 chars) ----")
    print(err.decode("utf-8", errors="replace")[-4000:])

    responses: dict[str, dict] = {}
    pong = False
    ready = False
    for line in text.splitlines():
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if not isinstance(msg, dict):
            continue
        if msg.get("type") == "ready":
            ready = True
        if msg.get("type") == "pong":
            pong = True
        if msg.get("type") == "response" and "success" in msg:
            responses[str(msg.get("id"))] = msg

    failures = []
    if not ready:
        failures.append('no {"type": "ready"} line at startup')
    if not pong:
        failures.append('no {"type": "pong"} line for ping')
    if responses.get(PING_ID, {}).get("success") is not True:
        failures.append("ping response missing or success != true")
    if MODELS_ID not in responses:
        failures.append("no models_list response line with a `success` key")
    if failures:
        if timed_out:
            failures.append(f"(the executor was killed after {timeout}s)")
        for failure in failures:
            print(f"::error::frozen executor smoke failed: {failure}")
        return 1

    if timed_out:
        print(
            f"::warning::executor answered both commands but did not exit within "
            f"{timeout}s of stdin EOF"
        )
    elif rc != 0:
        print(f"::warning::executor answered both commands but exited {rc}")

    print(f"models_list success={responses[MODELS_ID]['success']}")
    print("frozen executor smoke OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
