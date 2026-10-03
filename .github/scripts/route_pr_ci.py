#!/usr/bin/env python3
"""Decide, per OS, whether this run's routable jobs go to the PUBLIC self-hosted pool or stay hosted.

Plan `2026-10-03-runner-pr-branch-windows-ci-has-no-off-hosted-path` (qontinui-dev-notes),
Phases 1-2 (D1, D4, D5 and the approval-gate amendment). Modelled on qontinui-coord's
`.github/scripts/route_rust_ci.py`, copied and parameterised rather than shared: a
cross-repo reusable workflow would make this public repo's CI depend on a private one.

WHAT IT ROUTES. Only `pull_request` runs (and a `workflow_dispatch` that forces a lane,
which is how the pool is trialled). Pushes -- main, develop and the merge-candidate
refs -- always stay hosted: the merge-candidate leg belongs to the ci_node lane (plan
2026-09-27-arm-ci-node-dispatch-for-runner-windows-leg), not to this pool.

THE THREE SAFETY RULES, in the order they are applied. Each one can only send a job
HOSTED; nothing here can override them toward self-hosted.

  1. KILL SWITCH, DEFAULT HOSTED. Repo Actions variables RUNNER_LINUX_LANE and
     RUNNER_WINDOWS_LANE take `hosted | auto | self-hosted`. UNSET MEANS HOSTED -- unlike
     coord's `auto` default -- because an `auto` with nothing queued reads HEALTHY on a
     pool that does not exist. A variable is flipped off `hosted` only after that OS's
     public-pool runner has passed onboarding (two consecutive forced runs green).
     Anything that is not one of the three words is UNKNOWN and routes hosted.

  2. FORK CODE NEVER REACHES A FLEET HOST UNLESS THE APPROVAL GATE IS PROVEN. The
     primary control for running a public repo's PR CI on our hardware is the repo's
     fork-PR approval policy (`all_external_contributors`: every outside contributor's
     run waits for the operator). decision_record/self-hosted-runners-on-public-repos
     condition 1. A run whose head repo is not this repo routes self-hosted ONLY when this
     script READ that policy and it says `all_external_contributors`. The read needs
     repository-administration permission, which the GITHUB_TOKEN can never hold, so in
     practice fork PRs stay hosted -- fail closed. Same-repo PRs need push access to exist
     at all, so the approval gate is not what protects them; but if the policy IS
     readable and reads anything weaker, EVERY run routes hosted (a weakened setting
     fails closed for everyone, it never silently widens exposure).

  3. A STALLED OR SATURATED POOL FALLS BACK. In `auto`, a matching self-hosted job in
     this repo queued >= RUNNER_PUBLIC_POOL_STALL_MINUTES (default 10), or >=
     RUNNER_PUBLIC_POOL_MAX_QUEUE_DEPTH (default 3) of them queued, routes hosted. Any
     read error, timeout or script fault is UNKNOWN and routes hosted. `self-hosted`
     skips this check (it is the trial/forcing lever) but never rules 1 and 2.

WHAT IT NEVER ROUTES (D5), by construction rather than by this script: release.yml,
build-python-executor.yml, published-parity.yml and reproducibility-gate.yml do not
consume these outputs, and pull_request_target / workflow_run workflows must never
target `public-pool` or `qontinui` (ci-integrity's own trigger is pull_request_target and
it runs on ubuntu-latest). test_route_pr_ci.py pins all of that against the tree.

OUTPUTS ($GITHUB_OUTPUT), per OS in {linux, windows}: <os>_lane (self-hosted|hosted),
<os>_labels (JSON array), <os>_reason (one line). Plus approval_policy (the value read,
or UNKNOWN). The script ALWAYS exits 0 with every output written: a failing route job
would skip its dependants, and a skipped required check reads as a pass.

Stdlib only. Tests: .github/scripts/test_route_pr_ci.py (hermetic).
"""

from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime, timezone

# Registration labels of the public pool (D3). Every public-pool job names its OS label,
# because both OS's hosts carry `public-pool`. `qontinui` is the PRIVATE pool's label
# and must never appear here.
POOL_LABELS = {
    "linux": ["self-hosted", "Linux", "public-pool"],
    # Routing label `qontinui-windows` is added to a Windows host only after onboarding
    # (D3), so a job asking for it cannot land on a host that has not passed the trial.
    "windows": ["self-hosted", "Windows", "public-pool", "qontinui-windows"],
}
HOSTED_LABELS = {"linux": ["ubuntu-22.04"], "windows": ["windows-latest"]}
LANE_VARS = {"linux": "RUNNER_LINUX_LANE", "windows": "RUNNER_WINDOWS_LANE"}
REQUIRED_APPROVAL_POLICY = "all_external_contributors"
DEFAULT_STALL_MINUTES = 10.0
DEFAULT_MAX_QUEUE_DEPTH = 3
MAX_JOB_LISTS = 10
REQUEST_TIMEOUT_SECS = 10
TOTAL_DEADLINE_SECS = 45
WAITING_STATUSES = {"queued", "pending", "requested"}
ROUTABLE_EVENTS = {"pull_request", "workflow_dispatch"}


class SignalError(Exception):
    """A GitHub read failed. Always resolves toward hosted."""


@dataclass
class PoolObservation:
    queued: int = 0
    stalled: list[tuple[str, float]] = field(default_factory=list)
    runs_inspected: int = 0


@dataclass
class Decision:
    lane: str
    labels: list[str]
    reason: str


def one_line(text: str) -> str:
    return " ".join(str(text).replace("\r", " ").replace("\n", " ").split())


def parse_ts(value: str | None) -> datetime | None:
    if not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def label_set(labels) -> set[str]:
    return {str(label).lower() for label in (labels or [])}


def is_pool_job(job: dict, os_key: str) -> bool:
    """True only for a job whose runs-on is EXACTLY this OS's pool label set."""
    return label_set(job.get("labels")) == label_set(POOL_LABELS[os_key])


def observe_jobs(obs: dict[str, PoolObservation], jobs: list[dict], now: datetime, stall_minutes: float) -> None:
    for job in jobs:
        status = job.get("status") or ""
        if status not in WAITING_STATUSES:
            continue
        for os_key, pool_obs in obs.items():
            if not is_pool_job(job, os_key):
                continue
            pool_obs.queued += 1
            created = parse_ts(job.get("created_at")) or parse_ts(job.get("started_at"))
            if created is not None:
                waited = (now - created).total_seconds() / 60
                if waited >= stall_minutes:
                    pool_obs.stalled.append((f"{job.get('name', '?')} (run {job.get('run_id', '?')})", waited))


def normalise_lane(raw: str | None) -> str:
    """`hosted` when unset/blank (the default), the word when valid, else `invalid`."""
    value = (raw or "").strip().lower()
    if value == "":
        return "hosted"
    if value in ("hosted", "auto", "self-hosted"):
        return value
    return "invalid"


def decide(
    os_key: str,
    *,
    event: str,
    lane_raw: str | None,
    lane_source: str,
    same_repo: bool,
    policy: str | None,
    policy_error: str | None,
    pool: PoolObservation | None,
    pool_error: str | None,
    stall_minutes: float,
    max_queue_depth: int,
) -> Decision:
    hosted = Decision("hosted", list(HOSTED_LABELS[os_key]), "")
    self_hosted = Decision("self-hosted", list(POOL_LABELS[os_key]), "")
    var = LANE_VARS[os_key]

    if event not in ROUTABLE_EVENTS:
        hosted.reason = f"event {event or '?'} is never routed (pushes, merge candidates and main stay hosted)"
        return hosted

    lane = normalise_lane(lane_raw)
    if lane == "invalid":
        hosted.reason = f"UNKNOWN: {var}={one_line(lane_raw or '')!r} is not hosted|auto|self-hosted ({lane_source}); hosted"
        return hosted
    if lane == "hosted":
        hosted.reason = f"{var} is hosted ({lane_source}; unset means hosted)"
        return hosted

    # Rule 2: the approval gate. A READ that is weaker fails closed for everyone.
    if policy is not None and policy != REQUIRED_APPROVAL_POLICY:
        hosted.reason = (
            f"fork-PR approval policy reads {policy!r}, weaker than {REQUIRED_APPROVAL_POLICY!r}: "
            "public-pool routing refused for every run (fail closed)"
        )
        return hosted
    if not same_repo:
        if policy == REQUIRED_APPROVAL_POLICY:
            pass  # proven gate: an approved fork run may use the pool
        else:
            hosted.reason = (
                "fork PR and the approval policy is UNKNOWN "
                f"({policy_error or 'not read'}): fork code never reaches a fleet host unproven; hosted"
            )
            return hosted

    if lane == "self-hosted":
        self_hosted.reason = f"{var} forces self-hosted ({lane_source})"
        return self_hosted

    # Rule 3: auto -> pool health.
    if pool_error is not None or pool is None:
        hosted.reason = f"UNKNOWN: pool signal unreadable ({pool_error or 'no observation'}); hosted floor"
        return hosted
    if pool.stalled:
        name, minutes = max(pool.stalled, key=lambda s: s[1])
        hosted.reason = (
            f"pool STALLED: {len(pool.stalled)} {os_key} public-pool job(s) queued >= {stall_minutes:g} min "
            f"(longest {name}, {minutes:.0f} min)"
        )
        return hosted
    if pool.queued >= max_queue_depth:
        hosted.reason = f"pool SATURATED: {pool.queued} {os_key} public-pool jobs queued (threshold {max_queue_depth})"
        return hosted
    self_hosted.reason = (
        f"pool healthy: {pool.queued} {os_key} public-pool job(s) queued, none for >= {stall_minutes:g} min "
        f"across {pool.runs_inspected} in-flight run(s)"
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


def read_policy(api: Api) -> str:
    body = api.get("/actions/permissions/fork-pr-contributor-approval")
    policy = body.get("approval_policy")
    if not isinstance(policy, str) or not policy:
        raise SignalError("fork-pr-contributor-approval: no approval_policy field")
    return policy


def gather_pool(api: Api, now: datetime, own_run_id: str, stall_minutes: float) -> dict[str, PoolObservation]:
    obs = {os_key: PoolObservation() for os_key in POOL_LABELS}
    runs: dict[int, dict] = {}
    for status in ("queued", "in_progress"):
        body = api.get(f"/actions/runs?status={status}&per_page=100")
        if not isinstance(body.get("workflow_runs"), list):
            raise SignalError(f"runs?status={status}: no workflow_runs array")
        for run in body["workflow_runs"]:
            runs[int(run["id"])] = run
    ordered = sorted(
        (rid for rid in runs if str(rid) != str(own_run_id)),
        key=lambda rid: parse_ts(runs[rid].get("created_at")) or now,
    )
    inspected = 0
    for run_id in ordered[:MAX_JOB_LISTS]:
        body = api.get(f"/actions/runs/{run_id}/jobs?filter=latest&per_page=100")
        if not isinstance(body.get("jobs"), list):
            raise SignalError(f"runs/{run_id}/jobs: no jobs array")
        observe_jobs(obs, body["jobs"], now, stall_minutes)
        inspected += 1
    for pool_obs in obs.values():
        pool_obs.runs_inspected = inspected
    return obs


def env_number(name: str, default: float) -> float:
    try:
        value = float(os.environ.get(name, "") or default)
        return value if value > 0 else default
    except ValueError:
        return default


def emit(decisions: dict[str, Decision], policy_display: str, calls: int) -> None:
    out_lines = []
    for os_key, decision in decisions.items():
        decision.reason = one_line(decision.reason)
        labels_json = json.dumps(decision.labels, separators=(",", ":"))
        out_lines += [f"{os_key}_lane={decision.lane}", f"{os_key}_labels={labels_json}", f"{os_key}_reason={decision.reason}"]
        print(f"{os_key} lane: {decision.lane} runs-on={labels_json} -- {decision.reason}")
        print(f"::notice title=PR CI {os_key} lane: {decision.lane}::{decision.reason}")
    out_lines.append(f"approval_policy={one_line(policy_display)}")
    out = os.environ.get("GITHUB_OUTPUT")
    if out:
        with open(out, "a", encoding="utf-8") as fh:
            fh.write("\n".join(out_lines) + "\n")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        lines = ["### PR CI routing (public pool or hosted floor)", "", "| OS | lane | runs-on | reason |", "|---|---|---|---|"]
        for os_key, d in decisions.items():
            lines.append(f"| {os_key} | **{d.lane}** | `{json.dumps(d.labels)}` | {d.reason} |")
        lines += ["", f"fork-PR approval policy: `{policy_display}` · API calls: {calls}", ""]
        with open(summary, "a", encoding="utf-8") as fh:
            fh.write("\n".join(lines) + "\n")


def hosted_floor(reason: str) -> dict[str, Decision]:
    return {os_key: Decision("hosted", list(HOSTED_LABELS[os_key]), reason) for os_key in POOL_LABELS}


def main() -> int:
    event = os.environ.get("EVENT_NAME", "")
    repo = os.environ.get("GITHUB_REPOSITORY", "")
    head_repo = os.environ.get("HEAD_REPO", "")
    # A workflow_dispatch runs on this repo's own ref; a pull_request carries its head repo.
    same_repo = event == "workflow_dispatch" or (bool(head_repo) and head_repo.lower() == repo.lower())
    stall_minutes = env_number("RUNNER_PUBLIC_POOL_STALL_MINUTES", DEFAULT_STALL_MINUTES)
    max_depth = int(env_number("RUNNER_PUBLIC_POOL_MAX_QUEUE_DEPTH", DEFAULT_MAX_QUEUE_DEPTH))
    lanes = {os_key: os.environ.get(LANE_VARS[os_key]) for os_key in POOL_LABELS}
    sources = {os_key: os.environ.get(f"{LANE_VARS[os_key]}_SOURCE") or f"repo variable {LANE_VARS[os_key]}" for os_key in POOL_LABELS}

    token = os.environ.get("GH_TOKEN", "")
    api = None
    if token and repo:
        api = Api(os.environ.get("GITHUB_API_URL", "https://api.github.com"), repo, token, time.monotonic() + TOTAL_DEADLINE_SECS)

    wants_pool = event in ROUTABLE_EVENTS and any(normalise_lane(v) in ("auto", "self-hosted") for v in lanes.values())
    policy = policy_error = None
    pool = pool_error = None
    if wants_pool:
        if api is None:
            policy_error = pool_error = "GH_TOKEN or GITHUB_REPOSITORY unset"
        else:
            try:
                policy = read_policy(api)
            except SignalError as exc:
                policy_error = str(exc)
            except Exception as exc:  # noqa: BLE001 -- any fault is UNKNOWN
                policy_error = f"{type(exc).__name__}: {exc}"
            if any(normalise_lane(v) == "auto" for v in lanes.values()):
                try:
                    pool = gather_pool(api, datetime.now(timezone.utc), os.environ.get("GITHUB_RUN_ID", ""), stall_minutes)
                except SignalError as exc:
                    pool_error = str(exc)
                except Exception as exc:  # noqa: BLE001
                    pool_error = f"{type(exc).__name__}: {exc}"

    decisions = {
        os_key: decide(
            os_key,
            event=event,
            lane_raw=lanes[os_key],
            lane_source=sources[os_key],
            same_repo=same_repo,
            policy=policy,
            policy_error=policy_error,
            pool=pool[os_key] if pool else None,
            pool_error=pool_error,
            stall_minutes=stall_minutes,
            max_queue_depth=max_depth,
        )
        for os_key in POOL_LABELS
    }
    policy_display = policy if policy is not None else f"UNKNOWN ({policy_error or 'not read'})"
    emit(decisions, policy_display, api.calls if api else 0)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as exc:  # noqa: BLE001
        # Never exit non-zero: a failed route job skips its dependants, and a skipped
        # required check reads as a pass. Emit the hosted floor.
        emit(hosted_floor(f"UNKNOWN: route script fault ({type(exc).__name__}); hosted floor"), "UNKNOWN (script fault)", 0)
        sys.exit(0)
