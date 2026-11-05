#!/usr/bin/env python3
"""Decide which runner pool a routed CI job runs on: a self-hosted pool, or the hosted floor.

Plan `2026-10-03-move-private-repo-small-ci-onto-the-self-hosted-pool-with-a-hosted-floor`,
Phase 2. Its first caller is the `route` job of `.github/workflows/lint-frontmatter.yml`,
which hands this script's answer to the `frontmatter` job as
`runs-on: ${{ fromJSON(needs.route.outputs.labels || ...) }}`.

PROVENANCE -- A PARAMETERISED COPY, AND WHY A COPY. This is qontinui-coord's
`.github/scripts/route_rust_ci.py` as of qontinui-coord f32fb04ad, with every
coord-specific constant (the label set, the env var names, the defaults) lifted
into command-line arguments. It is COPIED rather than called as a cross-repo
reusable workflow on purpose: coord's required `rust-ci` must not depend at
runtime on this repo's main, and this repo's lander must not depend on coord's.
RE-SYNC this file when qontinui-coord#2725 (lane in the rust-ci summary) and
qontinui-coord#2727 (`# coord-pools:` declarations and the resolve-with-no-UNKNOWN
test) land -- both edit the original. The decision logic below is the original's
line for line; a divergence that is not a parameter is a re-sync defect.
ONE ccfg-only arm exists, `--hosted-on-rerun` (below), OFF by default so an
unparameterised call keeps coord's semantics exactly -- carry it across a
re-sync rather than dropping it.

THE SIGNAL, and why this one (the original's reasoning, unchanged). The
GITHUB_TOKEN cannot list self-hosted runners (that needs repo administration),
and coord's pool reads are operator-token gated. What the GITHUB_TOKEN CAN read,
with `actions: read`, is this repo's own jobs -- and that answers the question
routing needs: *is the pool picking up the jobs it is given?* So:

  * SATURATED / DOWN: some self-hosted job in this repo has sat `queued` longer
    than the stall threshold, or the self-hosted queue is at least the depth
    threshold. -> hosted.
  * HEALTHY: the read succeeded and shows neither. -> self-hosted.
  * UNKNOWN: any API error, timeout, unparseable body, or a script fault. -> hosted.
    Fail toward progress, never toward waiting.

A dead pool with no queued work reads HEALTHY until the first job routed to it
stalls; that job then becomes the stall evidence that routes every later run
hosted. That is exactly why the CALLER here defaults to `hosted` and runs this
script only when the operator has set the lane variable to `auto`: with no
runner registered yet, `auto` would read HEALTHY and route the first run onto a
pool that does not exist (the plan's Phase 2, "Default lane: hosted").

PARAMETERS (all optional; defaults are coord's, so an unparameterised call
reproduces the original):

  --self-hosted-labels L1,L2   the EXACT label set a pool job carries, and the
                               labels emitted on the self-hosted lane
                               (default self-hosted,qontinui)
  --env-prefix P               every env var below is read as P_<NAME>
                               (default COORD_RUST_CI)
  --title T                    the job the summary names (default rust-ci)
  --count-job-name REGEX       count a pool job toward the queue signals only
                               when its name matches (default: every job with
                               the exact label set, the original's behaviour)
  --default-lane auto|hosted   what an EMPTY P_LANE means (default auto)
  --default-max-queue-depth N  depth threshold when P_MAX_QUEUE_DEPTH is unset
                               (default 4)
  --default-stall-minutes N    stall threshold when P_STALL_MINUTES is unset
                               (default 10)
  --hosted-on-rerun            ccfg-ONLY, absent from the coord original and OFF
                               by default (so the default call keeps coord's
                               semantics): on `auto`, any run attempt after the
                               first routes to the hosted floor without reading
                               the pool. Why: lint-retry.yml USED TO re-run ALL
                               jobs after a self-hosted failure so this router
                               decided again (removed with the `route` job: it
                               now re-runs a failure `--failed`, which REUSES a
                               route decision -- restore the full re-run if
                               this router returns). The queue signal
                               only sees a pool that is NOT PICKING UP work -- a
                               pool that picks up jobs and then fails them (a
                               contaminated host) reads HEALTHY, and the retry
                               would land on it a second time. The attempt is
                               read from P_RUN_ATTEMPT, else GITHUB_RUN_ATTEMPT;
                               an unreadable one routes hosted (UNKNOWN).

ENV (P = --env-prefix): P_LANE (auto | self-hosted | hosted; anything else is
UNKNOWN -> hosted), P_LANE_SOURCE (names where the override came from),
P_RUN_ATTEMPT (read only under --hosted-on-rerun),
P_HOSTED_LABEL (default ubuntu-latest), P_STALL_MINUTES, P_MAX_QUEUE_DEPTH;
plus GH_TOKEN, GITHUB_REPOSITORY, GITHUB_API_URL, GITHUB_RUN_ID.

OUTPUTS (to $GITHUB_OUTPUT): labels (JSON array), lane (self-hosted|hosted),
reason (one line). A summary row goes to $GITHUB_STEP_SUMMARY. The script ALWAYS
exits 0 with outputs written, a bad argument included: a failing route job would
SKIP the routed job, and on this repo a skipped job concludes the workflow
`success` -- which `auto-merge.yml` lands on.

Stdlib only. Tests: scripts/route-ci-lane-test.py (hermetic).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone

DEFAULT_HOSTED_LABEL = "ubuntu-latest"
# Hard bounds on what one route decision may cost. Every routed run pays it,
# and the GITHUB_TOKEN budget is 1,000 requests/hour per repository.
MAX_JOB_LISTS = 10
REQUEST_TIMEOUT_SECS = 10
TOTAL_DEADLINE_SECS = 45
# Job statuses that mean "waiting for a runner". `waiting` is an environment
# approval wait, not a pool wait, so it is excluded.
WAITING_STATUSES = {"queued", "pending", "requested"}
LANES = ("auto", "self-hosted", "hosted")


class SignalError(Exception):
    """The pool signal could not be read. Always routes hosted."""


@dataclass
class Config:
    """Everything the original hard-coded. Defaults reproduce coord's rust-ci router."""

    self_hosted_labels: list[str] = field(default_factory=lambda: ["self-hosted", "qontinui"])
    env_prefix: str = "COORD_RUST_CI"
    title: str = "rust-ci"
    count_job_name: re.Pattern[str] | None = None
    default_lane: str = "auto"
    default_max_queue_depth: int = 4
    default_stall_minutes: float = 10
    hosted_on_rerun: bool = False

    def env(self, name: str) -> str:
        return os.environ.get(f"{self.env_prefix}_{name}", "")


@dataclass
class Observation:
    runs_inspected: int = 0
    runs_skipped_budget: int = 0
    queued_self_hosted: int = 0
    stalled: list[tuple[str, float]] = field(default_factory=list)  # (job label, minutes)
    last_pickup: datetime | None = None
    last_pickup_runner: str = ""


@dataclass
class Decision:
    lane: str
    labels: list[str]
    reason: str


def parse_ts(value: str | None) -> datetime | None:
    if not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def is_self_hosted(job: dict, cfg: Config) -> bool:
    """True only for a job whose runs-on is EXACTLY the pool's label set
    (and, when --count-job-name is given, whose name matches it).

    A superset (a host-pinned `[self-hosted, qontinui-ccfg, merytshost]` job) waits
    on ONE host, not on the pool, and counting its queue would flip every routed
    run hosted over a single busy machine. Case-insensitive, order-free.
    """
    labels = {str(label).lower() for label in (job.get("labels") or [])}
    if labels != {label.lower() for label in cfg.self_hosted_labels}:
        return False
    if cfg.count_job_name is not None and not cfg.count_job_name.search(str(job.get("name") or "")):
        return False
    return True


LABEL_RE = re.compile(r"^[A-Za-z0-9._-]+$")


def valid_label(value: str | None) -> str:
    """The hosted label, or the default when it is empty or not a plain runner label."""
    value = (value or "").strip()
    return value if LABEL_RE.match(value) else DEFAULT_HOSTED_LABEL


def one_line(text: str) -> str:
    """Collapse CR/LF so a reason cannot break $GITHUB_OUTPUT or a table row."""
    return " ".join(str(text).replace("\r", " ").replace("\n", " ").split())


def observe_jobs(obs: Observation, jobs: list[dict], now: datetime, stall_minutes: float, cfg: Config) -> None:
    """Fold one run's jobs into the observation."""
    for job in jobs:
        if not is_self_hosted(job, cfg):
            continue
        status = job.get("status") or ""
        if status in WAITING_STATUSES:
            obs.queued_self_hosted += 1
            created = parse_ts(job.get("created_at")) or parse_ts(job.get("started_at"))
            if created is not None:
                waited = (now - created).total_seconds() / 60
                if waited >= stall_minutes:
                    obs.stalled.append((f"{job.get('name', '?')} (run {job.get('run_id', '?')})", waited))
        elif status in ("in_progress", "completed") and job.get("runner_name"):
            started = parse_ts(job.get("started_at"))
            if started is not None and (obs.last_pickup is None or started > obs.last_pickup):
                obs.last_pickup = started
                obs.last_pickup_runner = str(job.get("runner_name"))


def resolve_override(raw: str | None, cfg: Config) -> str:
    """The effective lane request. EMPTY means --default-lane; anything else is kept
    (lower-cased) so `decide` can name an unrecognised value as UNKNOWN."""
    value = (raw or "").strip().lower()
    return value or cfg.default_lane


def rerun_attempt(raw: str | None) -> int | None:
    """The run attempt as a positive int, or None when it cannot be read."""
    try:
        value = int(str(raw or "").strip())
    except ValueError:
        return None
    return value if value >= 1 else None


def decide(
    obs: Observation | None,
    error: str | None,
    override: str,
    hosted_label: str,
    stall_minutes: float,
    max_queue_depth: int,
    cfg: Config | None = None,
    run_attempt: str | None = None,
) -> Decision:
    cfg = cfg or Config()
    var = f"{cfg.env_prefix}_LANE"
    hosted = Decision("hosted", [hosted_label], "")
    self_hosted = Decision("self-hosted", list(cfg.self_hosted_labels), "")
    unset = not (override or "").strip()
    override = resolve_override(override, cfg)
    if unset:
        source = f"{var} unset, so the caller's default lane"
    else:
        source = one_line(cfg.env("LANE_SOURCE") or f"repo variable {var}")
    if override == "self-hosted":
        self_hosted.reason = f"override ({source}): self-hosted"
        return self_hosted
    if override == "hosted":
        hosted.reason = f"override ({source}): hosted"
        return hosted
    if override != "auto":
        hosted.reason = f"UNKNOWN: {var}={override!r} is not auto|self-hosted|hosted; routing to the hosted floor"
        return hosted
    if cfg.hosted_on_rerun:
        attempt = rerun_attempt(run_attempt)
        if attempt is None:
            hosted.reason = f"UNKNOWN: run attempt {run_attempt!r} unreadable under --hosted-on-rerun; routing to the hosted floor"
            return hosted
        if attempt > 1:
            hosted.reason = (
                f"re-run (attempt {attempt}): a re-run goes to the hosted floor, because a pool that picks up "
                "jobs and then fails them reads healthy to the queue signal"
            )
            return hosted
    if error is not None or obs is None:
        hosted.reason = f"UNKNOWN: pool signal unreadable ({error or 'no observation'}); routing to the hosted floor"
        return hosted
    if obs.stalled:
        worst_name, worst_min = max(obs.stalled, key=lambda s: s[1])
        hosted.reason = (
            f"pool STALLED: {len(obs.stalled)} self-hosted job(s) queued >= {stall_minutes:g} min "
            f"(longest: {worst_name}, {worst_min:.0f} min)"
        )
        return hosted
    if obs.queued_self_hosted >= max_queue_depth:
        hosted.reason = (
            f"pool SATURATED: {obs.queued_self_hosted} self-hosted jobs queued "
            f"(threshold {max_queue_depth})"
        )
        return hosted
    self_hosted.reason = (
        f"pool healthy: {obs.queued_self_hosted} self-hosted job(s) queued, none for "
        f">= {stall_minutes:g} min, across {obs.runs_inspected} in-flight run(s)"
    )
    return self_hosted


class Api:
    def __init__(self, base: str, repo: str, token: str, deadline: float) -> None:
        self.base = base.rstrip("/")
        self.repo = repo
        self.token = token
        self.deadline = deadline
        self.calls = 0

    def get(self, path: str) -> dict:
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise SignalError("route deadline exceeded")
        req = urllib.request.Request(
            f"{self.base}/repos/{self.repo}{path}",
            headers={
                "Authorization": f"Bearer {self.token}",
                "Accept": "application/vnd.github+json",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        self.calls += 1
        try:
            with urllib.request.urlopen(req, timeout=min(REQUEST_TIMEOUT_SECS, remaining)) as resp:
                return json.load(resp)
        except urllib.error.HTTPError as exc:
            raise SignalError(f"GET {path} -> HTTP {exc.code}") from exc
        except (urllib.error.URLError, TimeoutError, OSError, ValueError) as exc:
            raise SignalError(f"GET {path} -> {type(exc).__name__}: {exc}") from exc


def gather(api: Api, now: datetime, own_run_id: str, stall_minutes: float, cfg: Config) -> Observation:
    obs = Observation()
    runs: dict[int, dict] = {}
    for status in ("queued", "in_progress"):
        body = api.get(f"/actions/runs?status={status}&per_page=100")
        if not isinstance(body.get("workflow_runs"), list):
            raise SignalError(f"runs?status={status}: no workflow_runs array")
        for run in body["workflow_runs"]:
            runs[int(run["id"])] = run
    cutoff = now - timedelta(minutes=stall_minutes)
    candidates = []
    for run_id, run in runs.items():
        if str(run_id) == str(own_run_id):
            continue
        created = parse_ts(run.get("created_at"))
        # A run younger than the stall threshold cannot hold a stalled job. It
        # CAN hold queued ones (depth), but the oldest runs are the ones that
        # decide a stall, so they get the bounded budget first.
        candidates.append((created or now, run_id, created is not None and created <= cutoff))
    candidates.sort(key=lambda c: (not c[2], c[0]))
    for _, run_id, _old in candidates:
        if obs.runs_inspected >= MAX_JOB_LISTS:
            obs.runs_skipped_budget += 1
            continue
        body = api.get(f"/actions/runs/{run_id}/jobs?filter=latest&per_page=100")
        if not isinstance(body.get("jobs"), list):
            raise SignalError(f"runs/{run_id}/jobs: no jobs array")
        observe_jobs(obs, body["jobs"], now, stall_minutes, cfg)
        obs.runs_inspected += 1
    return obs


def env_number(raw: str, default: float) -> float:
    try:
        value = float(raw or default)
        return value if value > 0 else default
    except ValueError:
        return default


def emit(decision: Decision, obs: Observation | None, calls: int, cfg: Config | None = None) -> None:
    cfg = cfg or Config()
    decision.reason = one_line(decision.reason)
    labels_json = json.dumps(decision.labels, separators=(",", ":"))
    out = os.environ.get("GITHUB_OUTPUT")
    if out:
        with open(out, "a", encoding="utf-8") as fh:
            fh.write(f"labels={labels_json}\nlane={decision.lane}\nreason={decision.reason}\n")
    print(f"{cfg.title} lane: {decision.lane} runs-on={labels_json} -- {decision.reason}")
    print(f"::notice title={cfg.title} lane: {decision.lane}::{decision.reason}")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        lines = [
            f"### {cfg.title} routing",
            "",
            "| | |",
            "|---|---|",
            f"| lane | **{decision.lane}** |",
            f"| runs-on | `{labels_json}` |",
            f"| reason | {decision.reason} |",
        ]
        if obs is not None:
            pickup = (
                f"{obs.last_pickup.isoformat()} on `{obs.last_pickup_runner}`"
                if obs.last_pickup
                else "none among the inspected runs (not decisive -- see route-ci-lane.py)"
            )
            lines += [
                f"| in-flight runs inspected | {obs.runs_inspected} (+{obs.runs_skipped_budget} over budget) |",
                f"| self-hosted jobs queued | {obs.queued_self_hosted} |",
                f"| latest self-hosted pickup | {pickup} |",
            ]
        lines += [f"| API calls | {calls} |", ""]
        with open(summary, "a", encoding="utf-8") as fh:
            fh.write("\n".join(lines) + "\n")


class _Parser(argparse.ArgumentParser):
    """argparse exits 2 on a bad argument; this router must never exit non-zero."""

    def error(self, message: str) -> None:  # type: ignore[override]
        raise ValueError(message)


def parse_config(argv: list[str]) -> Config:
    p = _Parser(add_help=False)
    p.add_argument("--self-hosted-labels", default="self-hosted,qontinui")
    p.add_argument("--env-prefix", default="COORD_RUST_CI")
    p.add_argument("--title", default="rust-ci")
    p.add_argument("--count-job-name", default="")
    p.add_argument("--default-lane", default="auto", choices=("auto", "hosted"))
    p.add_argument("--default-max-queue-depth", type=int, default=4)
    p.add_argument("--default-stall-minutes", type=float, default=10)
    p.add_argument("--hosted-on-rerun", action="store_true")
    a = p.parse_args(argv)
    labels = [s.strip() for s in a.self_hosted_labels.split(",") if s.strip()]
    if not labels or not all(LABEL_RE.match(s) for s in labels):
        raise ValueError(f"--self-hosted-labels {a.self_hosted_labels!r} is not a list of plain runner labels")
    if not re.match(r"^[A-Z][A-Z0-9_]*$", a.env_prefix):
        raise ValueError(f"--env-prefix {a.env_prefix!r} is not an upper-case env var prefix")
    if a.default_max_queue_depth < 1 or a.default_stall_minutes <= 0:
        raise ValueError("--default-max-queue-depth and --default-stall-minutes must be positive")
    return Config(
        self_hosted_labels=labels,
        env_prefix=a.env_prefix,
        title=one_line(a.title) or "routed job",
        count_job_name=re.compile(a.count_job_name) if a.count_job_name else None,
        default_lane=a.default_lane,
        default_max_queue_depth=a.default_max_queue_depth,
        default_stall_minutes=a.default_stall_minutes,
        hosted_on_rerun=a.hosted_on_rerun,
    )


def main(argv: list[str]) -> int:
    cfg = parse_config(argv)
    hosted_label = valid_label(cfg.env("HOSTED_LABEL"))
    raw_lane = cfg.env("LANE")
    attempt_raw = (cfg.env("RUN_ATTEMPT") or os.environ.get("GITHUB_RUN_ATTEMPT", "")) if cfg.hosted_on_rerun else None
    rerun = cfg.hosted_on_rerun and rerun_attempt(attempt_raw) != 1
    stall_minutes = env_number(cfg.env("STALL_MINUTES"), cfg.default_stall_minutes)
    max_depth = int(env_number(cfg.env("MAX_QUEUE_DEPTH"), cfg.default_max_queue_depth))
    obs: Observation | None = None
    error: str | None = None
    api: Api | None = None
    # A re-run (or an unreadable attempt) under --hosted-on-rerun is decided
    # without the pool read, so it spends no API budget on an answer it ignores.
    if resolve_override(raw_lane, cfg) == "auto" and not rerun:
        token = os.environ.get("GH_TOKEN", "")
        repo = os.environ.get("GITHUB_REPOSITORY", "")
        if not token or not repo:
            error = "GH_TOKEN or GITHUB_REPOSITORY unset"
        else:
            api = Api(
                os.environ.get("GITHUB_API_URL", "https://api.github.com"),
                repo,
                token,
                time.monotonic() + TOTAL_DEADLINE_SECS,
            )
            try:
                obs = gather(api, datetime.now(timezone.utc), os.environ.get("GITHUB_RUN_ID", ""), stall_minutes, cfg)
            except SignalError as exc:
                error = str(exc)
            except Exception as exc:  # noqa: BLE001 -- any fault is UNKNOWN, and UNKNOWN routes hosted
                error = f"{type(exc).__name__}: {exc}"
    decision = decide(obs, error, raw_lane, hosted_label, stall_minutes, max_depth, cfg, attempt_raw)
    emit(decision, obs, api.calls if api else 0, cfg)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Exception as exc:  # noqa: BLE001
        # Last resort: never exit non-zero (a failed route job skips the routed
        # job, and a skipped job reads as a pass). Emit the floor. The prefix is
        # re-read from argv by hand, because a bad argument is one of the faults
        # that lands here and argparse cannot be trusted to have produced it.
        prefix = "COORD_RUST_CI"
        args = sys.argv[1:]
        for i, arg in enumerate(args):
            if arg == "--env-prefix" and i + 1 < len(args):
                prefix = args[i + 1]
            elif arg.startswith("--env-prefix="):
                prefix = arg.split("=", 1)[1]
        label = valid_label(os.environ.get(f"{prefix}_HOSTED_LABEL"))
        emit(Decision("hosted", [label], f"UNKNOWN: route script fault ({type(exc).__name__}: {exc}); hosted floor"), None, 0)
        sys.exit(0)
