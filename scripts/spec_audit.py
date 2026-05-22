#!/usr/bin/env python3
"""
Corpus-wide spec-check audit tool.

Iterates every spec registered for an app on a UI Bridge-connected runner,
activates the matching tab (when one exists), captures the live snapshot,
runs `/spec-check`, and writes a deterministic JSON report.

Engineered to be resilient against the UI Bridge SDK disconnects we observed
during the first bash-based audit (after ~60 rapid tab switches the React
SDK stopped responding, leaving the runner stuck in `snapshot-unavailable`).
Strategy: probe SDK health before each request, back off when the SDK times
out, and abort with a partial report rather than producing garbage rows.

Usage:
    python scripts/spec_audit.py \
        --runner http://localhost:9878 \
        --app qontinui-runner \
        --output /tmp/spec-audit.json

The output schema is stable so re-runs diff cleanly with `jq` or `git diff`.
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

import requests
from requests.adapters import HTTPAdapter

# ---------------------------------------------------------------------------
# SDK-disconnect detection
# ---------------------------------------------------------------------------

# The runner returns this exact "note" inside `data` when its native-window
# screenshot fallback fires because the React SDK has stopped responding to
# IPC. Distinct from a transient timeout — the SDK won't recover until the
# webview itself re-handshakes (typically requires a refresh).
SDK_FALLBACK_NOTE = "SDK was not connected"

# Transport-level error message emitted by the Rust side when the IPC
# oneshot times out. Less terminal than the fallback note — sometimes
# recovers on the next request.
SDK_TIMEOUT_MARKER = "UI Bridge request timed out"


@dataclass
class SdkHealth:
    """Result of a single snapshot probe."""

    healthy: bool
    reason: Optional[str] = None
    available_tab_ids: set[str] = field(default_factory=set)


def probe_sdk_health(session: requests.Session, runner: str) -> SdkHealth:
    """Snapshot the runner and decide whether the React SDK is live.

    Returns the set of tab ids the webview is currently aware of so the
    audit can skip tab activation for spec ids that don't correspond to
    a known tab (saves an IPC round-trip + reduces churn).
    """
    try:
        resp = session.get(f"{runner}/ui-bridge/control/snapshot", timeout=15)
    except requests.exceptions.RequestException as e:
        return SdkHealth(False, f"transport: {e}")

    if not resp.ok:
        return SdkHealth(False, f"http {resp.status_code}")

    try:
        body = resp.json()
    except ValueError:
        return SdkHealth(False, f"non-json snapshot body: {resp.text[:120]}")

    data = body.get("data") or {}
    note = (data.get("note") or "")
    reason = (data.get("reason") or "")
    if SDK_FALLBACK_NOTE in note or SDK_TIMEOUT_MARKER in reason:
        return SdkHealth(False, f"sdk_fallback: {note} / {reason}")

    tabs = {t.get("id") for t in (data.get("availableTabs") or []) if t.get("id")}
    return SdkHealth(True, available_tab_ids=tabs)


def wait_for_sdk_recovery(
    session: requests.Session, runner: str, max_wait_secs: float
) -> SdkHealth:
    """Poll the snapshot endpoint until SDK recovers or budget exhausts.

    Uses progressive backoff (0.5s → 2s → 5s ...) so a brief stall is
    cheap to recover from and a real disconnect doesn't hammer the runner.
    """
    deadline = time.monotonic() + max_wait_secs
    delay = 0.5
    last = SdkHealth(False, reason="not yet probed")
    while time.monotonic() < deadline:
        last = probe_sdk_health(session, runner)
        if last.healthy:
            return last
        time.sleep(delay)
        delay = min(delay * 1.8, 5.0)
    return last


# ---------------------------------------------------------------------------
# Tab activation + spec-check
# ---------------------------------------------------------------------------

@dataclass
class SpecResult:
    spec_id: str
    tab_activated: bool
    tab_existed: bool
    match_outcome: str
    overall_match_rate: float
    state_count: int
    full_match_states: int
    miss_reasons: dict[str, int]
    error: Optional[str] = None
    elapsed_ms: int = 0


def activate_tab(session: requests.Session, runner: str, tab_id: str) -> bool:
    try:
        resp = session.post(
            f"{runner}/ui-bridge/control/tab/activate",
            json={"tabId": tab_id},
            timeout=10,
        )
    except requests.exceptions.RequestException:
        return False
    if not resp.ok:
        return False
    try:
        return bool(resp.json().get("success"))
    except ValueError:
        return False


def run_spec_check(
    session: requests.Session, runner: str, app_id: str, spec_id: str
) -> tuple[Optional[dict[str, Any]], Optional[str]]:
    try:
        resp = session.post(
            f"{runner}/spec-check",
            json={"appId": app_id, "pageId": spec_id},
            timeout=30,
        )
    except requests.exceptions.RequestException as e:
        return None, f"transport: {e}"

    # The /spec-check endpoint returns 200 even when it can't capture a
    # snapshot — it embeds the reason in the body. Either way we want to
    # parse the JSON and check.
    try:
        body = resp.json()
    except ValueError:
        return None, f"http {resp.status_code} non-json: {resp.text[:120]}"

    if not resp.ok:
        # Error envelope from wrap_ipc_result — surface the error field.
        return None, body.get("error") or f"http {resp.status_code}"

    if body.get("ok") is False:
        # Snapshot-unavailable / sdk-disconnect path. Caller decides whether
        # to retry or mark as deferred.
        return None, body.get("reason") or "spec-check ok=false"

    return body, None


def audit_one_spec(
    session: requests.Session,
    runner: str,
    app_id: str,
    spec_id: str,
    available_tab_ids: set[str],
) -> SpecResult:
    started = time.monotonic()
    tab_existed = spec_id in available_tab_ids
    tab_activated = False
    if tab_existed:
        tab_activated = activate_tab(session, runner, spec_id)

    body, err = run_spec_check(session, runner, app_id, spec_id)

    elapsed_ms = int((time.monotonic() - started) * 1000)

    if err is not None or body is None:
        return SpecResult(
            spec_id=spec_id,
            tab_activated=tab_activated,
            tab_existed=tab_existed,
            match_outcome="ERROR",
            overall_match_rate=0.0,
            state_count=0,
            full_match_states=0,
            miss_reasons={},
            error=err,
            elapsed_ms=elapsed_ms,
        )

    summary = body.get("summary") or {}
    state_results = body.get("stateResults") or []
    full_states = sum(1 for s in state_results if (s.get("matchRate") or 0) >= 0.999)

    miss_reasons: dict[str, int] = {}
    for state in state_results:
        for assertion in state.get("assertions") or []:
            outcome = assertion.get("outcome") or {}
            if outcome.get("status") == "fail":
                reason = (outcome.get("miss") or {}).get("reason") or "unknown"
                miss_reasons[reason] = miss_reasons.get(reason, 0) + 1

    return SpecResult(
        spec_id=spec_id,
        tab_activated=tab_activated,
        tab_existed=tab_existed,
        match_outcome=summary.get("matchOutcome") or "UNKNOWN",
        overall_match_rate=float(summary.get("overallMatchRate") or 0.0),
        state_count=len(state_results),
        full_match_states=full_states,
        miss_reasons=miss_reasons,
        elapsed_ms=elapsed_ms,
    )


# ---------------------------------------------------------------------------
# Audit driver
# ---------------------------------------------------------------------------

def build_session() -> requests.Session:
    """Plain HTTP session — the runner's API is on localhost so no
    fancy retry mechanics; our SDK recovery is at the application layer
    where we can distinguish transient timeouts from genuine SDK death."""
    s = requests.Session()
    # Modest connection pool — the runner is single-tenant on localhost.
    adapter = HTTPAdapter(pool_connections=4, pool_maxsize=4, max_retries=0)
    s.mount("http://", adapter)
    return s


def list_specs(session: requests.Session, runner: str, app_id: str) -> list[str]:
    resp = session.get(f"{runner}/apps/{app_id}/spec/list", timeout=15)
    resp.raise_for_status()
    body = resp.json()
    if not body.get("ok"):
        raise RuntimeError(f"spec list failed: {body.get('reason') or body}")
    return sorted({s["specId"] for s in body.get("specs") or []})


def run_audit(args: argparse.Namespace) -> dict[str, Any]:
    session = build_session()
    runner = args.runner.rstrip("/")
    app_id = args.app

    print(f"[audit] listing specs for {app_id} on {runner}", file=sys.stderr)
    spec_ids = list_specs(session, runner, app_id)
    total = len(spec_ids)
    print(f"[audit] {total} specs queued", file=sys.stderr)

    print("[audit] probing SDK health …", file=sys.stderr)
    initial = probe_sdk_health(session, runner)
    if not initial.healthy:
        raise SystemExit(f"runner SDK not healthy at start: {initial.reason}")
    available_tab_ids = initial.available_tab_ids
    print(
        f"[audit] SDK live; {len(available_tab_ids)} known tabs",
        file=sys.stderr,
    )

    results: list[SpecResult] = []
    deferred: list[dict[str, Any]] = []
    consecutive_sdk_failures = 0

    for i, spec_id in enumerate(spec_ids, start=1):
        result = audit_one_spec(session, runner, app_id, spec_id, available_tab_ids)

        # Detect SDK-disconnect heuristically:
        # the spec-check endpoint returns `{ok: false, reason: snapshot-unavailable}`
        # when the SDK has gone away. audit_one_spec encodes that as match_outcome
        # ERROR with error="snapshot-unavailable".
        if result.match_outcome == "ERROR" and result.error == "snapshot-unavailable":
            consecutive_sdk_failures += 1
            print(
                f"[audit]  {i}/{total} {spec_id}  SDK disconnect ({consecutive_sdk_failures}/{args.max_consecutive_failures})",
                file=sys.stderr,
            )
            if consecutive_sdk_failures >= args.max_consecutive_failures:
                # Try recovery once; if it fails, bail with partial results.
                print(
                    f"[audit] attempting SDK recovery (up to {args.max_recovery_secs}s)",
                    file=sys.stderr,
                )
                recovered = wait_for_sdk_recovery(
                    session, runner, args.max_recovery_secs
                )
                if not recovered.healthy:
                    print(
                        f"[audit] SDK did not recover ({recovered.reason}); "
                        f"aborting with partial results — {len(spec_ids) - i + 1} specs deferred",
                        file=sys.stderr,
                    )
                    for remaining in spec_ids[i - 1 :]:
                        deferred.append(
                            {"specId": remaining, "reason": "sdk_unrecoverable"}
                        )
                    break
                available_tab_ids = recovered.available_tab_ids
                consecutive_sdk_failures = 0
                # Retry this spec once after recovery
                result = audit_one_spec(
                    session, runner, app_id, spec_id, available_tab_ids
                )
        else:
            consecutive_sdk_failures = 0

        results.append(result)

        if i % 10 == 0:
            print(
                f"[audit]  {i}/{total}  last: {spec_id} → {result.match_outcome} ({result.overall_match_rate:.3f})",
                file=sys.stderr,
            )

        # Pace between specs to give the webview time to settle. Rapid
        # back-to-back tab activations were the trigger for the SDK
        # disconnect in the first audit run.
        time.sleep(args.pace_secs)

    return build_report(runner, app_id, total, results, deferred, spec_ids)


def build_report(
    runner: str,
    app_id: str,
    total: int,
    results: list[SpecResult],
    deferred: list[dict[str, Any]],
    spec_ids: list[str],
) -> dict[str, Any]:
    rates = [r.overall_match_rate for r in results if r.match_outcome != "ERROR"]
    full_count = sum(1 for r in results if r.overall_match_rate >= 0.999)
    high_count = sum(
        1 for r in results if 0.5 <= r.overall_match_rate < 0.999
    )
    low_count = sum(1 for r in results if 0.0 < r.overall_match_rate < 0.5)
    zero_count = sum(
        1 for r in results if r.overall_match_rate == 0.0 and r.match_outcome != "ERROR"
    )
    error_count = sum(1 for r in results if r.match_outcome == "ERROR")

    return {
        "runnerUrl": runner,
        "appId": app_id,
        "auditedAt": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "specCount": total,
        "completedCount": len(results),
        "deferredCount": len(deferred),
        "summary": {
            "fullMatch": full_count,
            "high": high_count,
            "low": low_count,
            "zero": zero_count,
            "error": error_count,
            "meanRate": round(statistics.fmean(rates), 4) if rates else 0.0,
            "medianRate": round(statistics.median(rates), 4) if rates else 0.0,
        },
        "results": [
            {
                "specId": r.spec_id,
                "tabExisted": r.tab_existed,
                "tabActivated": r.tab_activated,
                "matchOutcome": r.match_outcome,
                "overallMatchRate": round(r.overall_match_rate, 6),
                "stateCount": r.state_count,
                "fullMatchStates": r.full_match_states,
                "missReasons": r.miss_reasons,
                "error": r.error,
                "elapsedMs": r.elapsed_ms,
            }
            for r in sorted(results, key=lambda x: x.spec_id)
        ],
        "deferred": sorted(deferred, key=lambda x: x["specId"]),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--runner",
        required=True,
        help="Base URL of the runner (e.g. http://localhost:9878)",
    )
    parser.add_argument(
        "--app", required=True, help="App id registered on the runner"
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=None,
        help="Write JSON report to this path; default stdout",
    )
    parser.add_argument(
        "--pace-secs",
        type=float,
        default=1.0,
        help="Seconds between specs (default 1.0). Lower = faster but more "
        "SDK-disconnect risk; higher = safer.",
    )
    parser.add_argument(
        "--max-consecutive-failures",
        type=int,
        default=3,
        help="After N back-to-back snapshot-unavailable errors, trigger "
        "recovery probe (default 3).",
    )
    parser.add_argument(
        "--max-recovery-secs",
        type=float,
        default=30.0,
        help="Maximum seconds to wait for SDK to recover before aborting "
        "with partial results.",
    )
    args = parser.parse_args()

    report = run_audit(args)

    out = json.dumps(report, indent=2, sort_keys=False)
    if args.output:
        args.output.write_text(out, encoding="utf-8")
        print(
            f"[audit] wrote {args.output} "
            f"({report['completedCount']}/{report['specCount']} completed, "
            f"{report['deferredCount']} deferred)",
            file=sys.stderr,
        )
    else:
        print(out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
