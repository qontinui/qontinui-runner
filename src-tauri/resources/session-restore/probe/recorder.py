#!/usr/bin/env python3
"""Phase 1 probe recorder — records the SHAPE of what the Claude Code CLI sends.

Plan: `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
Phase 1. This is a hand-run probe, NOT a runner component: nothing in the
runner references this directory, nothing bundles it, and it must never be
registered by the runner's own `--settings` carrier.

One recorder serves every hook event and the statusLine. It reads the JSON the
CLI writes to stdin and appends ONE line to the scratch file named by
`$PROBE_OUT`:

    {"mode", "ts", "pid", "event", "keys", "enums"}

* `keys` is a recursive TYPE skeleton — objects map key -> skeleton, arrays are
  the list of distinct element skeletons, scalars are the type name
  (`string` / `number` / `boolean` / `null`). **No value is ever written.**
* `enums` carries ONLY an allow-listed set of protocol identifiers (event names,
  `notification_type`, `error_type`, `error`, `reason`, `source`,
  `permission_mode`, `tool_name`), and only when the value is a short identifier — never free
  text. These are CLI protocol constants, not user content, and several of the
  five Phase 1 questions are unanswerable without them. They stay in the
  scratch file; the committed fixtures carry the skeleton only.

Modes (argv[1]):

* `hook` — record and exit 0, printing nothing (hook stdout is consumed by the
  CLI).
* `statusline` — record a `start` line, optionally sleep `$PROBE_SL_SLEEP`
  seconds (to provoke the CLI's cancel-in-flight behaviour), print
  `$PROBE_SL_TEXT` if set (else print NOTHING), then record an `end` line. A
  SIGTERM / SIGHUP / SIGINT is recorded as a `signal` line, so a cancelled run
  is distinguishable from one killed outright (SIGKILL leaves `start` with no
  `end` and no `signal`). With `$PROBE_SL_TERM_GRACE` set, the handler keeps
  running that many seconds, appending a `survived` line every 50 ms, so the
  last one bounds how long the CLI lets a cancelled run live.
* `async-sleeper` — record `start`, sleep `$PROBE_ASYNC_SLEEP` seconds (0 when
  unset), record `end`. Registered with `async: true` to test whether the CLI
  honours it: if the turn's next event lands before this `end`, it did.
* `noop-stamp` — record only a timestamp (Q5 per-event cost bracket).

Fail-open by construction: any error is swallowed and the process exits 0.
"""

import json
import os
import re
import signal
import sys
import time

ENUM_KEYS = {
    "hook_event_name",
    "notification_type",
    "error_type",
    "reason",
    "source",
    "permission_mode",
    "tool_name",
    "matcher",
    "error",
}
IDENT = re.compile(r"^[A-Za-z0-9_.:\-]{1,64}$")


def skeleton(value):
    """Type skeleton of `value`; never the value itself."""
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "boolean"
    if isinstance(value, (int, float)):
        return "number"
    if isinstance(value, str):
        return "string"
    if isinstance(value, list):
        out = []
        for element in value:
            skel = skeleton(element)
            if skel not in out:
                out.append(skel)
        return out
    if isinstance(value, dict):
        return {k: skeleton(v) for k, v in sorted(value.items())}
    return "unknown"


def enums(value):
    """Allow-listed protocol identifiers found at the top level only."""
    found = {}
    if isinstance(value, dict):
        for key in ENUM_KEYS:
            candidate = value.get(key)
            if isinstance(candidate, str) and IDENT.match(candidate):
                found[key] = candidate
    return found


def append(record):
    path = os.environ.get("PROBE_OUT")
    if not path:
        return
    record.setdefault("ts", time.time())
    record.setdefault("pid", os.getpid())
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, sort_keys=True) + "\n")


def read_payload():
    raw = sys.stdin.buffer.read(1024 * 1024)
    try:
        return json.loads(raw.decode("utf-8")), len(raw)
    except (ValueError, UnicodeDecodeError):
        return None, len(raw)


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "hook"
    if mode == "noop-stamp":
        append({"mode": mode})
        return
    payload, size = read_payload()
    event = payload.get("hook_event_name") if isinstance(payload, dict) else None
    base = {
        "mode": mode,
        "event": event if isinstance(event, str) and IDENT.match(event) else None,
        "keys": skeleton(payload),
        "enums": enums(payload),
        "stdin_bytes": size,
    }
    if mode == "statusline":
        run_id = "%d-%f" % (os.getpid(), time.time())

        def on_signal(signum, _frame):
            append({"mode": mode, "phase": "signal", "signal": signum, "run": run_id})
            # $PROBE_SL_TERM_GRACE: keep running after the signal, to learn
            # whether the CLI follows its SIGTERM with a SIGKILL (a
            # `survived` line means work after the signal — a detached POST —
            # still gets out).
            grace = float(os.environ.get("PROBE_SL_TERM_GRACE", "0") or 0)
            began = time.time()
            while time.time() - began < grace:
                time.sleep(0.05)
                append({"mode": mode, "phase": "survived", "after_s": round(time.time() - began, 3), "run": run_id})
            sys.exit(0)

        for signum in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
            signal.signal(signum, on_signal)
        base.update({"phase": "start", "run": run_id})
        append(base)
        time.sleep(float(os.environ.get("PROBE_SL_SLEEP", "0") or 0))
        text = os.environ.get("PROBE_SL_TEXT")
        if text:
            sys.stdout.write(text + "\n")
            sys.stdout.flush()
        append({"mode": mode, "phase": "end", "run": run_id})
        return
    if mode == "async-sleeper":
        base["phase"] = "start"
        append(base)
        time.sleep(float(os.environ.get("PROBE_ASYNC_SLEEP", "0") or 0))
        append({"mode": mode, "phase": "end", "event": base["event"]})
        return
    append(base)


if __name__ == "__main__":
    try:
        main()
    except Exception:  # noqa: BLE001 — a probe must never break the session
        pass
    sys.exit(0)
