#!/usr/bin/env python3
# VENDORED COPY -- the source is qontinui-coord/.github/scripts/route_ci_job.py
# (tests: qontinui-coord/.github/scripts/test_route_ci_job.py). Edit there and
# re-copy byte for byte below this header; never edit this copy alone.
# body-sha256: a02b4050fb570b786502d58851d7a60ded6054afb480750485d3aae7c024c45d
#   (sha256 of everything below this header == `tail -n +2` of the source;
#   src-tauri/tests/ci_route_gates_hosted_only_steps.rs recomputes it)
"""Ask coord where ONE heavy CI job should run, and hand the answer to `runs-on`.

Plan `2026-10-27-coord-routes-ci-jobs-by-runner-capability`, Phase 7 (D5). The
generalisation of `route_rust_ci.py`: that script picks rust-ci's pool from a
GitHub queue signal; this one asks coord's anonymous
`GET /coord/ci/route?repo=&job=&event=` for any job by its check name, and falls
back to the workflow's own static default whenever coord does not answer.

THIS JOB MUST NOT FAIL. A failed `needs:` job SKIPS its dependant, and a skipped
required check reads as a pass. So the script always exits 0 with outputs
written, and the dependant additionally runs on `!cancelled()` with its static
default for an empty output:

    runs-on: ${{ fromJSON(needs.route.outputs.runs_on || '"windows-latest"') }}

A route job on a hosted runner is refused by the spending cap, and its dependant
then gets an EMPTY output -- so it must run where the dependant's static default
runs. (On a public repo the standard hosted runners are free, so a route job
there may run hosted; on a private repo it must run self-hosted.)

THE ANSWER (coord's four lanes, D5):
  hosted       the job fits on its default hosted class (public repo, tenant
               hosted CI on): runs_on is that label.
  self_hosted  the hosted class does not fit (or is not offered) and a
               self-hosted class does: runs_on is a label set that selects only
               hosts that fit.
  fail_fast    every class the job may run on is Incapable: runs_on is the
               static default, and the dependant's first step fails it in
               seconds with the `ci-capacity` reason instead of an OOM 40
               minutes in.
  static       anything coord does not know: the static default.

A repo coord does not know to be PUBLIC (private, visibility unknown, or
owned by no tenant) is not routed: coord answers one opaque `static` with
`runs_on: null` whatever it knows, and this script then uses ROUTE_DEFAULT.
A private repo's workflow must therefore set ROUTE_DEFAULT to its own
self-hosted pool. Where coord decided hosted is not offered on a public repo
(the tenant's hosted CI off), it answers static/fail_fast with
`runs_on: null` too -- it never names a hosted label there.

THIS SCRIPT'S OWN LOGIC (what it does without coord, and what it refuses from
coord). Any HTTP error, timeout, unparseable body, unknown lane or malformed
label set -> `static` with the default. It also refuses a `self_hosted` answer
unless the event is a trusted one (push, workflow_dispatch, schedule,
merge_group) or a pull_request whose head this run's own event payload shows is
NOT a fork (an unreadable fork-ness is refused, and pull_request_target /
issue_comment / workflow_run always are): coord never sends one there, and this
is the second lock on that door. It refuses, as coord's own exclusivity check
does, a self_hosted label set that lacks `qontinui-ci-isolated` (a host's
opt-in that it keeps no operator credential where a job can read it); a set
of GitHub's default labels only is refused first, with its own reason,
though the isolated-label check alone would refuse it too. A missing or
malformed ROUTE_DEFAULT emits an EMPTY runs_on, so the dependant's own
`|| '"<label>"'` default applies. Gate a
hosted-only step on `runner.environment == 'github-hosted'`, never on `lane`:
`lane` is empty when this job did not run, and a step gated on `lane == 'hosted'`
would then skip on the hosted runner the static default chose.

INPUTS (env):
  ROUTE_JOB            REQUIRED: the check-run name exactly as GitHub shows it
                       (a matrix leg is `test (windows-latest)`).
  ROUTE_DEFAULT        the job's static runs-on: a label, or a JSON label array.
                       Pass the dependant's own default explicitly.
  ROUTE_HOSTED_PAGEFILE_MB  the pagefile (MiB) the job's own pagefile step
                       configures on a hosted runner; coord forms the hosted
                       commit limit from it (RAM + pagefile).
  ROUTE_OUTPUT_PREFIX  prefix for every output name (several jobs, one route job).
  ROUTE_COORD_URL      default https://coord.qontinui.io
  ROUTE_DISABLED       `1` answers static without asking.
  ROUTE_REPO / ROUTE_EVENT / ROUTE_FORK  default GITHUB_REPOSITORY,
                       GITHUB_EVENT_NAME, and the fork-ness read from
                       GITHUB_EVENT_PATH.

OUTPUTS (to $GITHUB_OUTPUT, each under ROUTE_OUTPUT_PREFIX): runs_on (valid
JSON -- a quoted label or a label array -- or EMPTY when there is no usable
default), lane, reason (one line), source (coord | fallback). A summary goes
to $GITHUB_STEP_SUMMARY.

VENDORED COPIES carry a header naming this file as their source; edit here and
re-copy. Stdlib only. Tests: .github/scripts/test_route_ci_job.py (hermetic).
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass

DEFAULT_COORD_URL = "https://coord.qontinui.io"
LANES = ("hosted", "self_hosted", "fail_fast", "static")
LABEL_RE = re.compile(r"^[A-Za-z0-9._-]{1,100}$")
PREFIX_RE = re.compile(r"^[A-Za-z0-9_-]{0,60}$")
# coord's own read budget is 6 s; two tries inside the deadline.
REQUEST_TIMEOUT_SECS = 10
TOTAL_DEADLINE_SECS = 25
MAX_BODY_BYTES = 256 * 1024
MAX_LABELS = 20
MAX_HOSTED_PAGEFILE_MB = 262144
# Events whose code is the repo's own: the only ones besides a non-fork
# pull_request this script lets run self-hosted (decision_record
# self-hosted-runners-on-public-repos).
TRUSTED_EVENTS = ("push", "workflow_dispatch", "schedule", "merge_group")
# Mirrors coord's ci_route::GITHUB_DEFAULT_LABELS and ci_route::ISOLATED_LABEL.
GITHUB_DEFAULT_LABELS = ("self-hosted", "linux", "windows", "macos", "x64", "x86", "arm", "arm64")
ISOLATED_LABEL = "qontinui-ci-isolated"


class RouteError(Exception):
    """coord gave no usable answer. Always routes to the static default."""


@dataclass
class Decision:
    runs_on: object  # str label, list of labels, or None (emit empty: the dependant's default)
    lane: str
    reason: str
    source: str


def one_line(text: object) -> str:
    """Collapse CR/LF so a reason cannot break $GITHUB_OUTPUT or a table row."""
    return " ".join(str(text).replace("\r", " ").replace("\n", " ").split())[:900]


def parse_default(raw: str | None) -> object | None:
    """The static runs-on: a plain label or a JSON label array; None when malformed."""
    raw = (raw or "").strip()
    if not raw:
        return None
    if raw.startswith("[") or raw.startswith('"'):
        try:
            return valid_runs_on(json.loads(raw))
        except ValueError:
            return None
    return raw if LABEL_RE.match(raw) else None


def valid_runs_on(value: object) -> object | None:
    """A runs-on this script will hand to fromJSON: a label, or a non-empty label list."""
    if isinstance(value, str):
        return value if LABEL_RE.match(value) else None
    if isinstance(value, list) and 0 < len(value) <= MAX_LABELS:
        if all(isinstance(v, str) and LABEL_RE.match(v) for v in value):
            return list(value)
    return None


def fork_from_event(event_name: str, event_path: str | None) -> bool | None:
    """Is this pull_request's head a fork? None when it cannot be read or does not apply."""
    if event_name not in ("pull_request", "pull_request_target"):
        return None
    if not event_path:
        return None
    try:
        with open(event_path, encoding="utf-8") as fh:
            payload = json.load(fh)
        head = (payload.get("pull_request") or {}).get("head")
        if not isinstance(head, dict):
            return None
        if "repo" in head and head["repo"] is None:
            # A deleted head repo reads `repo: null`: a fork that went away.
            return True
        head_name = (head.get("repo") or {}).get("full_name")
        base = (payload.get("repository") or {}).get("full_name")
        if not head_name or not base:
            return None
        return str(head_name).lower() != str(base).lower()
    except (OSError, ValueError, AttributeError):
        return None


def parse_pagefile(raw: str | None) -> int | None:
    """ROUTE_HOSTED_PAGEFILE_MB: an integer MiB in range, else None."""
    try:
        v = int((raw or "").strip())
    except ValueError:
        return None
    return v if 0 <= v <= MAX_HOSTED_PAGEFILE_MB else None


def self_hosted_allowed(event: str, fork: bool | None) -> str | None:
    """None when this event may take a self-hosted label; else why not. PURE."""
    e = (event or "").strip().lower()
    if e in TRUSTED_EVENTS:
        return None
    if e == "pull_request":
        if fork is False:
            return None
        if fork is True:
            return "this pull_request's head is a fork"
        return "this pull_request's fork-ness could not be read"
    return f"event {one_line(e)[:40]!r} never takes a self-hosted label here"


def query_url(
    base: str,
    repo: str,
    job: str,
    event: str,
    fork: bool | None,
    default: object | None,
    hosted_pagefile_mb: int | None = None,
) -> str:
    params = {"repo": repo, "job": job, "event": event}
    if fork is not None:
        params["fork"] = "true" if fork else "false"
    if isinstance(default, str):
        params["default"] = default
    if hosted_pagefile_mb is not None:
        params["hosted_pagefile_mb"] = str(hosted_pagefile_mb)
    return f"{base.rstrip('/')}/coord/ci/route?{urllib.parse.urlencode(params)}"


def ask(url: str, deadline: float) -> dict:
    last = "no attempt"
    for _ in range(2):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        req = urllib.request.Request(url, headers={"Accept": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=min(REQUEST_TIMEOUT_SECS, remaining)) as resp:
                body = resp.read(MAX_BODY_BYTES + 1)
                if len(body) > MAX_BODY_BYTES:
                    raise RouteError("response body over the size bound")
                parsed = json.loads(body)
                if not isinstance(parsed, dict):
                    raise RouteError("response is not a JSON object")
                return parsed
        except urllib.error.HTTPError as exc:
            # A 4xx is the request's own fault: retrying cannot help.
            if 400 <= exc.code < 500:
                raise RouteError(f"HTTP {exc.code}") from exc
            last = f"HTTP {exc.code}"
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            last = f"{type(exc).__name__}: {exc}"
        except ValueError as exc:
            raise RouteError(f"unparseable body ({exc})") from exc
    raise RouteError(f"coord unreachable ({last})")


def decide(
    answer: dict | None,
    error: str | None,
    default: object | None,
    fork: bool | None,
    event: str,
) -> Decision:
    """coord's answer, checked; the static default on anything else. PURE.

    With no usable default the runs_on is None (an EMPTY output), so the
    dependant's own `||` default applies -- never a label this script made up.
    """
    fallback_runs_on = default

    def static(why: str) -> Decision:
        if default is None:
            why = f"{why} (ROUTE_DEFAULT missing or malformed: the job's own default applies)"
        return Decision(fallback_runs_on, "static", one_line(why), "fallback")

    if error is not None or answer is None:
        return static(f"UNKNOWN: {error or 'no answer'}; the static default")
    lane = answer.get("lane")
    if lane not in LANES:
        return static(f"UNKNOWN: coord answered lane {one_line(lane)!r}; the static default")
    reason = one_line(answer.get("reason") or "")
    raw = answer.get("runs_on")
    if lane in ("static", "fail_fast") and raw is None:
        return Decision(fallback_runs_on, lane, reason, "coord")
    runs_on = valid_runs_on(raw)
    if runs_on is None:
        return static(f"UNKNOWN: coord's runs_on {one_line(json.dumps(raw))[:200]} is not a label set; the static default")
    if lane == "self_hosted":
        refused = self_hosted_allowed(event, fork)
        if refused is not None:
            return static(f"refused coord's self_hosted answer: {refused}")
        labels = [label.lower() for label in ([runs_on] if isinstance(runs_on, str) else runs_on)]
        if "self-hosted" not in labels:
            return static("refused coord's self_hosted answer: its label set does not name self-hosted")
        # Implied by the ISOLATED_LABEL check below (not a default label);
        # first only so the reason names the larger mistake.
        if all(label in GITHUB_DEFAULT_LABELS for label in labels):
            return static("refused coord's self_hosted answer: its label set is GitHub's defaults only")
        if ISOLATED_LABEL not in labels:
            return static(f"refused coord's self_hosted answer: its label set does not name {ISOLATED_LABEL}")
    if lane in ("static", "fail_fast"):
        # coord echoes the caller's own default; keep the caller's spelling.
        runs_on = fallback_runs_on
    return Decision(runs_on, lane, reason, "coord")


def cell(text: object) -> str:
    """A markdown table cell: a `|` would split the row."""
    return str(text).replace("|", "\\|")


def emit(d: Decision, prefix: str, job: str) -> None:
    runs_on_json = "" if d.runs_on is None else json.dumps(d.runs_on, separators=(",", ":"))
    out = os.environ.get("GITHUB_OUTPUT")
    if out:
        with open(out, "a", encoding="utf-8") as fh:
            fh.write(
                f"{prefix}runs_on={runs_on_json}\n{prefix}lane={d.lane}\n"
                f"{prefix}reason={d.reason}\n{prefix}source={d.source}\n"
            )
    print(f"{job}: lane={d.lane} runs-on={runs_on_json} source={d.source} -- {d.reason}")
    level = "warning" if d.lane == "fail_fast" else "notice"
    print(f"::{level} title={one_line(job)[:80]} route: {d.lane}::{d.reason}")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        lines = [
            f"### `{cell(job)}` routing",
            "",
            "| | |",
            "|---|---|",
            f"| lane | **{d.lane}** ({d.source}) |",
            f"| runs-on | `{cell(runs_on_json or '(the job default)')}` |",
            f"| reason | {cell(d.reason)} |",
            "",
        ]
        with open(summary, "a", encoding="utf-8") as fh:
            fh.write("\n".join(lines) + "\n")


def main() -> int:
    prefix = os.environ.get("ROUTE_OUTPUT_PREFIX", "")
    if not PREFIX_RE.match(prefix):
        prefix = ""
    job = one_line(os.environ.get("ROUTE_JOB", ""))
    default = parse_default(os.environ.get("ROUTE_DEFAULT"))
    event = os.environ.get("ROUTE_EVENT") or os.environ.get("GITHUB_EVENT_NAME", "")
    fork_env = (os.environ.get("ROUTE_FORK") or "").strip().lower()
    fork = (
        True
        if fork_env == "true"
        else False
        if fork_env == "false"
        else fork_from_event(event, os.environ.get("GITHUB_EVENT_PATH"))
    )
    answer: dict | None = None
    error: str | None = None
    if os.environ.get("ROUTE_DISABLED") == "1":
        error = "ROUTE_DISABLED=1"
    elif not job:
        error = "ROUTE_JOB unset"
    else:
        repo = os.environ.get("ROUTE_REPO") or os.environ.get("GITHUB_REPOSITORY", "")
        if not repo:
            error = "ROUTE_REPO / GITHUB_REPOSITORY unset"
        else:
            url = query_url(
                os.environ.get("ROUTE_COORD_URL") or DEFAULT_COORD_URL,
                repo,
                job,
                event,
                fork,
                default,
                parse_pagefile(os.environ.get("ROUTE_HOSTED_PAGEFILE_MB")),
            )
            try:
                answer = ask(url, time.monotonic() + TOTAL_DEADLINE_SECS)
            except RouteError as exc:
                error = str(exc)
            except Exception as exc:  # noqa: BLE001 -- any fault is UNKNOWN, and UNKNOWN is static
                error = f"{type(exc).__name__}: {exc}"
    emit(decide(answer, error, default, fork, event), prefix, job or "<unnamed job>")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as exc:  # noqa: BLE001
        # Last resort: never exit non-zero (a failed route job skips its
        # dependant, and a skipped required check reads as a pass). An empty
        # output makes the dependant's `||` default its runs-on.
        print(f"route script fault ({type(exc).__name__}); the dependant uses its static default")
        sys.exit(0)
