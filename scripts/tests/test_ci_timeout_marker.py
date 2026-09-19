#!/usr/bin/env python3
"""Regression test for the in-band job-timeout marker and budget tripwire.

WHY THIS FILE EXISTS. GitHub concludes a JOB-level `timeout-minutes` expiry as
`cancelled` -- never `failure`, never `timed_out` -- with zero failing steps.
A `concurrency: cancel-in-progress` supersede concludes exactly the same way.
From the conclusion alone the two are indistinguishable, which is how 30 of
the Frontend Coverage Producer's last 40 runs timed out at its old 45-minute
bound and were read as supersedes for twelve days. The fix is in-band: the job
stamps its own start, a tripwire warns on a GREEN run closing on the budget,
and a marker step on a cancelled run says in the log which cause it was.

A marker like that rots silently in exactly the ways it exists to catch, so
the properties pinned here are the ones a wrong marker gets wrong:

  * The carrier set is DERIVED from `.github/workflows/*.y*ml` and compared to
    a literal roster, so adding a carrier means adding a row, and a carrier
    added (or removed) without the row fails.
  * `JOB_TIMEOUT_MINUTES` is a literal that must equal the job's
    `timeout-minutes` (it cannot be an expression -- see the lockstep note in
    the workflow). Drift makes the timeout arm misfire or never fire.
  * The marker runs on `cancelled() || failure()` (a default `success()` would
    never run on the one state it explains), the tripwire on `success()`.
  * The start stamp is the job's FIRST step and is anchored to the workspace
    root (it runs before checkout).
  * The SHIPPED `run:` bodies -- extracted from the YAML, never copied here --
    route each arm to the right annotation title, never exit non-zero, and
    never turn a missing or garbage measurement into a timeout claim: an
    UNKNOWN is reported as UNKNOWN, never collapsed into "external cancel".

Python 3 + PyYAML only (no pytest), so it runs as a plain script on
ubuntu-latest, like scripts/tests/ci-integrity/test-surface.py.

Run locally:
  python3 scripts/tests/test_ci_timeout_marker.py
"""

from __future__ import annotations

import hashlib
import shutil
import subprocess
import sys
import tempfile
import traceback
from pathlib import Path

import yaml

REPO = Path(__file__).resolve().parents[2]
WORKFLOWS = REPO / ".github" / "workflows"

# --- rosters: adding a carrier = adding a row here ---------------------------
MARKER_WORKFLOWS = ["frontend-coverage-producer.yml"]
TRIPWIRE_WORKFLOWS = ["frontend-coverage-producer.yml"]

STAMP_STEP_NAME = "Record job start time"
MARKER_STEP_NAME = "Explain infrastructure timeout vs external cancellation"
TRIPWIRE_STEP_NAME = "Warn if the job is approaching its budget"

# Literal annotation titles (identical to qontinui-web's carriers).
TITLE_TIMEOUT = "::error title=Job budget exhausted - a TIMEOUT, not an external cancel::"
TITLE_EXTERNAL = "::notice title=Run cancelled externally - NOT a verdict on this diff::"
TITLE_UNKNOWN = "::warning title=Cancelled - a timeout cannot be told from an external cancel::"
TITLE_TRIPWIRE_UNKNOWN = "Job duration UNKNOWN - budget tripwire did not run"
TITLE_TRIPWIRE_WARN_TAIL = "is approaching its job budget"

# sha256 of qontinui-web's shared tripwire `run:` body (its
# backend-coverage-producer.yml carrier, 2026-09-18). The workflow claims the
# runner's copy is byte-identical to web's; this pins that claim. If you change
# the body, change it in qontinui-web too and update this digest in both repos
# -- or drop the claim from the workflow comment.
WEB_TRIPWIRE_BODY_SHA256 = "cc27cc85150ede93e42a20c1de7f600ebf078780a418a58dc85c064a680e19b2"

# Fixed "now" served by the fake `date`, so every elapsed figure is exact. Set
# years away from the real clock on purpose: if a body ever bypassed the stub,
# the exact-minute assertions below would fail instead of passing by accident.
FAKE_NOW = 2_000_000_000

BASH = shutil.which("bash")
REAL_DATE = shutil.which("date")


# --- tiny harness ------------------------------------------------------------
_cases: list = []


def case(fn):
    """Register one named case; main() runs them all and reports PASS/FAIL."""
    _cases.append(fn)
    return fn


def _run_all() -> list[str]:
    failed = []
    for fn in _cases:
        try:
            fn()
            print(f"PASS  {fn.__name__}")
        except Exception as exc:  # noqa: BLE001 -- report every failure, keep going
            detail = "".join(traceback.format_exception_only(type(exc), exc)).strip()
            failed.append(fn.__name__)
            print(f"FAIL  {fn.__name__}\n      {detail}")
    return failed


# --- YAML helpers ------------------------------------------------------------
def _load(path: Path) -> dict:
    return yaml.safe_load(path.read_text(encoding="utf-8")) or {}


def _all_workflow_paths() -> list[Path]:
    return sorted(WORKFLOWS.glob("*.y*ml"))


def _carriers_of(step_name: str) -> list[str]:
    found = []
    for path in _all_workflow_paths():
        for job in (_load(path).get("jobs") or {}).values():
            if not isinstance(job, dict):
                continue
            if any(s.get("name") == step_name for s in job.get("steps") or []):
                found.append(path.name)
                break
    return sorted(found)


def _steps_named(workflows: list[str], step_name: str):
    """Yield (workflow, job_id, job, step) for EVERY matching step."""
    for wf in workflows:
        doc = _load(WORKFLOWS / wf)
        for job_id, job in (doc.get("jobs") or {}).items():
            for step in job.get("steps") or []:
                if step.get("name") == step_name:
                    yield wf, job_id, job, step


def _condition(step: dict) -> str:
    raw = step.get("if")
    return "success()" if raw is None else " ".join(str(raw).split())


def _env(step: dict) -> dict[str, str]:
    return {k: str(v) for k, v in (step.get("env") or {}).items()}


def _cancel_in_progress(wf: str, job: dict):
    conc = job.get("concurrency")
    if conc is None:
        conc = _load(WORKFLOWS / wf).get("concurrency")
    if not isinstance(conc, dict):
        return None
    return conc.get("cancel-in-progress")


def _body(step_name: str, wf: str) -> str:
    steps = list(_steps_named([wf], step_name))
    assert steps, f"{wf} has no step named {step_name!r}"
    return steps[0][3]["run"]


# --- body execution ----------------------------------------------------------
def _stub_dir(gh_output: str | None) -> Path:
    """A PATH dir with a fake `date` (fixed now) and a fake `gh`."""
    d = Path(tempfile.mkdtemp(prefix="timeout-marker-"))
    (d / "date").write_text(
        "#!/usr/bin/env bash\n"
        '# `date -u +%s` -> the fixed clock; anything else (e.g. -d) -> real date.\n'
        'if [ "$#" -eq 2 ] && [ "$1" = "-u" ] && [ "$2" = "+%s" ]; then\n'
        f"  echo {FAKE_NOW}\n"
        "  exit 0\n"
        "fi\n"
        f'exec {REAL_DATE} "$@"\n',
        encoding="utf-8",
    )
    if gh_output is None:
        gh = "#!/usr/bin/env bash\necho 'HTTP 403' >&2\nexit 1\n"
    else:
        gh = f"#!/usr/bin/env bash\necho '{gh_output}'\n"
    (d / "gh").write_text(gh, encoding="utf-8")
    for f in ("date", "gh"):
        (d / f).chmod(0o755)
    return d


def _exec(script: str, env: dict[str, str], gh_output: str | None = None) -> str:
    """Run a shipped body under the flags GitHub gives it in production.

    With no `shell:` key GitHub runs `run:` as `bash -e {0}` (only an explicit
    `shell: bash` gives `bash --noprofile --norc -eo pipefail {0}`). The bodies
    `set -uo pipefail` themselves, so `-eo pipefail` here is exactly the
    effective production set either way. `-e` matters: a harness without it
    runs the bodies under weaker semantics and cannot see an abort (a
    top-level `$(( 08 ))` exits 1 under -e and 0 without it).
    """
    stub = _stub_dir(gh_output)
    try:
        full_env = {
            "PATH": f"{stub}:/usr/bin:/bin",
            "GITHUB_REPOSITORY": "qontinui/qontinui-runner",
            "GITHUB_RUN_ID": "1234567890",
            "GH_TOKEN": "stub",
            **env,
        }
        proc = subprocess.run(
            [BASH, "--noprofile", "--norc", "-eo", "pipefail", "-s"],
            input=script,
            env=full_env,
            capture_output=True,
            text=True,
            timeout=30,
        )
    finally:
        shutil.rmtree(stub, ignore_errors=True)
    assert proc.returncode == 0, (
        f"body must never exit non-zero (it only reports); got {proc.returncode}\n"
        f"stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
    )
    # Exit 0 alone is too weak. bash aborts only the enclosing COMPOUND command
    # on an arithmetic error, even under -e, and then carries on -- so an
    # unguarded `$(( 08 ))` inside an `if` exits 0 while silently skipping the
    # measurement. Any shell error on stderr means a guard is missing.
    assert proc.stderr == "", f"body wrote shell errors to stderr:\n{proc.stderr}"
    return proc.stdout


def _marker_env(wf: str) -> dict[str, str]:
    env = _env(next(_steps_named([wf], MARKER_STEP_NAME))[3])
    # Expressions are not evaluated here; supply what GitHub would.
    env["GH_TOKEN"] = "stub"
    return env


def _run_marker(
    wf: str,
    *,
    job_status: str = "cancelled",
    elapsed_s: int | None = None,
    stamp: str | None = None,
    gh_output: str | None = None,
) -> str:
    env = _marker_env(wf)
    env["JOB_STATUS"] = job_status
    if stamp is not None:
        env["JOB_START_EPOCH"] = stamp
    elif elapsed_s is not None:
        env["JOB_START_EPOCH"] = str(FAKE_NOW - elapsed_s)
    return _exec(_body(MARKER_STEP_NAME, wf), env, gh_output)


def _run_tripwire(wf: str, *, elapsed_s: int | None = None, stamp: str | None = None) -> str:
    env = _env(next(_steps_named([wf], TRIPWIRE_STEP_NAME))[3])
    if stamp is not None:
        env["JOB_START_EPOCH"] = stamp
    elif elapsed_s is not None:
        env["JOB_START_EPOCH"] = str(FAKE_NOW - elapsed_s)
    return _exec(_body(TRIPWIRE_STEP_NAME, wf), env)


def _iso(epoch: int) -> str:
    out = subprocess.run(
        [REAL_DATE, "-u", "-d", f"@{epoch}", "+%Y-%m-%dT%H:%M:%SZ"],
        capture_output=True, text=True, check=True,
    )
    return out.stdout.strip()


# --- structural pins ---------------------------------------------------------
@case
def marker_carriers_are_exactly_the_roster():
    got = _carriers_of(MARKER_STEP_NAME)
    assert got == sorted(MARKER_WORKFLOWS), (
        f"marker carriers {got} != roster {sorted(MARKER_WORKFLOWS)}; "
        "add/remove the row in MARKER_WORKFLOWS together with the carrier"
    )


@case
def tripwire_carriers_are_exactly_the_roster():
    got = _carriers_of(TRIPWIRE_STEP_NAME)
    assert got == sorted(TRIPWIRE_WORKFLOWS), (
        f"tripwire carriers {got} != roster {sorted(TRIPWIRE_WORKFLOWS)}"
    )


@case
def every_carrier_job_stamps_its_start_as_step_zero():
    jobs = set()
    for wf, job_id, job, _ in list(_steps_named(MARKER_WORKFLOWS, MARKER_STEP_NAME)) + list(
        _steps_named(TRIPWIRE_WORKFLOWS, TRIPWIRE_STEP_NAME)
    ):
        jobs.add((wf, job_id))
        first = job["steps"][0]
        assert first.get("name") == STAMP_STEP_NAME, (
            f"{wf}#{job_id}: step 0 is {first.get('name') or first.get('uses')!r}, "
            f"expected {STAMP_STEP_NAME!r}"
        )
        assert "JOB_START_EPOCH" in first.get("run", ""), f"{wf}#{job_id}: stamp writes no JOB_START_EPOCH"
        assert first.get("working-directory") == "${{ github.workspace }}", (
            f"{wf}#{job_id}: stamp must set working-directory: ${{{{ github.workspace }}}} "
            f"(it runs before checkout); got {first.get('working-directory')!r}"
        )
    assert jobs, "no carrier jobs found at all"


@case
def marker_runs_on_cancelled_or_failure():
    for wf, job_id, _, step in _steps_named(MARKER_WORKFLOWS, MARKER_STEP_NAME):
        assert _condition(step) == "cancelled() || failure()", (
            f"{wf}#{job_id}: marker if: is {_condition(step)!r}"
        )


@case
def tripwire_runs_on_success():
    for wf, job_id, _, step in _steps_named(TRIPWIRE_WORKFLOWS, TRIPWIRE_STEP_NAME):
        assert _condition(step) == "success()", f"{wf}#{job_id}: tripwire if: is {_condition(step)!r}"


@case
def job_timeout_minutes_matches_the_job_everywhere():
    seen = 0
    for name, wfs in ((MARKER_STEP_NAME, MARKER_WORKFLOWS), (TRIPWIRE_STEP_NAME, TRIPWIRE_WORKFLOWS)):
        for wf, job_id, job, step in _steps_named(wfs, name):
            seen += 1
            declared = _env(step).get("JOB_TIMEOUT_MINUTES")
            actual = job.get("timeout-minutes")
            assert isinstance(actual, int), f"{wf}#{job_id}: timeout-minutes {actual!r} is not a literal int"
            assert declared is not None and declared.isdigit(), (
                f"{wf}#{job_id} step {name!r}: JOB_TIMEOUT_MINUTES {declared!r} is not a literal integer"
            )
            assert int(declared) == actual, (
                f"{wf}#{job_id} step {name!r}: JOB_TIMEOUT_MINUTES={declared} but "
                f"timeout-minutes={actual} -- keep them in lockstep"
            )
    assert seen, "no steps checked"


@case
def soft_budget_is_below_the_job_budget():
    for wf, job_id, _, step in _steps_named(TRIPWIRE_WORKFLOWS, TRIPWIRE_STEP_NAME):
        env = _env(step)
        soft, hard = env.get("SOFT_BUDGET_MINUTES", ""), env.get("JOB_TIMEOUT_MINUTES", "")
        assert soft.isdigit() and hard.isdigit(), f"{wf}#{job_id}: non-integer budgets {soft!r}/{hard!r}"
        assert int(soft) < int(hard), f"{wf}#{job_id}: SOFT_BUDGET {soft} must be < JOB_TIMEOUT {hard}"
        assert env.get("JOB_LABEL", "").strip(), f"{wf}#{job_id}: JOB_LABEL missing"
        assert env.get("BUDGET_STAKES", "").strip(), f"{wf}#{job_id}: BUDGET_STAKES missing"


@case
def external_cancel_note_matches_concurrency():
    for wf, job_id, job, step in _steps_named(MARKER_WORKFLOWS, MARKER_STEP_NAME):
        note = _env(step).get("EXTERNAL_CANCEL_NOTE", "")
        assert note.strip(), f"{wf}#{job_id}: EXTERNAL_CANCEL_NOTE missing"
        cip = _cancel_in_progress(wf, job)
        mentions = "cancel-in-progress" in note
        assert mentions == (cip is True), (
            f"{wf}#{job_id}: EXTERNAL_CANCEL_NOTE mentions cancel-in-progress={mentions} "
            f"but the governing concurrency cancel-in-progress={cip!r}"
        )


# --- executed marker arms ----------------------------------------------------
@case
def marker_a_cancelled_at_the_budget_is_a_timeout():
    for wf in MARKER_WORKFLOWS:
        budget = int(_marker_env(wf)["JOB_TIMEOUT_MINUTES"])
        out = _run_marker(wf, elapsed_s=budget * 60)
        assert TITLE_TIMEOUT in out, out
        assert f"ran {budget} min (measured from this job's own start)" in out, out
        assert TITLE_EXTERNAL not in out and TITLE_UNKNOWN not in out, out


@case
def marker_b_651s_supersede_is_external_not_timeout():
    for wf in MARKER_WORKFLOWS:
        out = _run_marker(wf, elapsed_s=651)
        assert TITLE_EXTERNAL in out, out
        assert "cancelled after 10 min" in out, out
        assert TITLE_TIMEOUT not in out, out
        assert "cancel-in-progress" in out, "external arm must carry the concurrency note"


@case
def marker_c_no_stamp_and_gh_fails_is_unknown():
    for wf in MARKER_WORKFLOWS:
        out = _run_marker(wf, gh_output=None)
        assert TITLE_UNKNOWN in out, out
        assert TITLE_TIMEOUT not in out and TITLE_EXTERNAL not in out, out


@case
def marker_d_inside_the_two_minute_slack_is_a_timeout():
    for wf in MARKER_WORKFLOWS:
        budget = int(_marker_env(wf)["JOB_TIMEOUT_MINUTES"])
        out = _run_marker(wf, elapsed_s=(budget - 1) * 60)
        assert TITLE_TIMEOUT in out, out
        # ...and just below the floor is NOT a timeout.
        out = _run_marker(wf, elapsed_s=(budget - 3) * 60)
        assert TITLE_TIMEOUT not in out and TITLE_EXTERNAL in out, out


@case
def marker_e_garbage_stamp_never_crashes_or_claims_timeout():
    for wf in MARKER_WORKFLOWS:
        for bad in ("08", "abc", "-5", "", "99999999999999999999", str(FAKE_NOW + 3600)):
            out = _run_marker(wf, stamp=bad, gh_output=None)  # _exec asserts exit 0
            assert TITLE_TIMEOUT not in out, f"stamp {bad!r} produced a timeout claim:\n{out}"
            assert TITLE_EXTERNAL not in out, f"stamp {bad!r} produced an external-cancel claim:\n{out}"
            assert TITLE_UNKNOWN in out, f"stamp {bad!r} should read UNKNOWN:\n{out}"


@case
def marker_run_basis_fallback_rules_and_discloses_it():
    for wf in MARKER_WORKFLOWS:
        budget = int(_marker_env(wf)["JOB_TIMEOUT_MINUTES"])
        out = _run_marker(wf, gh_output=_iso(FAKE_NOW - budget * 60))
        assert TITLE_TIMEOUT in out and "RUN's start" in out, out
        out = _run_marker(wf, gh_output=_iso(FAKE_NOW - 651))
        assert TITLE_EXTERNAL in out and "RUN's start" in out, out
        out = _run_marker(wf, gh_output="not-a-timestamp")
        assert TITLE_UNKNOWN in out, out


@case
def marker_future_stamp_falls_back_to_the_run_basis():
    """A FUTURE job stamp must not suppress the run-basis lookup."""
    for wf in MARKER_WORKFLOWS:
        future = str(FAKE_NOW + 3600)
        out = _run_marker(wf, stamp=future, gh_output=_iso(FAKE_NOW - 91 * 60))
        assert TITLE_TIMEOUT in out and "RUN's start" in out, out
        assert TITLE_UNKNOWN not in out, out
        out = _run_marker(wf, stamp=future, gh_output=_iso(FAKE_NOW - 651))
        assert TITLE_EXTERNAL in out and "RUN's start" in out, out


@case
def tripwire_body_is_byte_identical_to_web():
    for wf, job_id, _, step in _steps_named(TRIPWIRE_WORKFLOWS, TRIPWIRE_STEP_NAME):
        digest = hashlib.sha256(step["run"].encode("utf-8")).hexdigest()
        assert digest == WEB_TRIPWIRE_BODY_SHA256, (
            f"{wf}#{job_id}: tripwire body sha256 {digest} != web's "
            f"{WEB_TRIPWIRE_BODY_SHA256}; per-site text belongs in env, not the body"
        )


@case
def marker_on_a_failure_says_nothing_misleading():
    for wf in MARKER_WORKFLOWS:
        budget = int(_marker_env(wf)["JOB_TIMEOUT_MINUTES"])
        out = _run_marker(wf, job_status="failure", elapsed_s=budget * 60)
        assert "::" not in out, f"a failed (not cancelled) job must get no annotation:\n{out}"


# --- executed tripwire arms --------------------------------------------------
@case
def tripwire_warns_past_the_soft_budget():
    for wf in TRIPWIRE_WORKFLOWS:
        out = _run_tripwire(wf, elapsed_s=75 * 60)
        assert TITLE_TRIPWIRE_WARN_TAIL in out and "::warning title=" in out, out
        soft = int(_env(next(_steps_named([wf], TRIPWIRE_STEP_NAME))[3])["SOFT_BUDGET_MINUTES"])
        assert TITLE_TRIPWIRE_WARN_TAIL in _run_tripwire(wf, elapsed_s=soft * 60)
        assert TITLE_TRIPWIRE_WARN_TAIL not in _run_tripwire(wf, elapsed_s=(soft - 1) * 60)


@case
def tripwire_is_quiet_on_a_fast_job():
    for wf in TRIPWIRE_WORKFLOWS:
        out = _run_tripwire(wf, elapsed_s=30 * 60)
        assert "::" not in out, out
        assert "Job wall-clock: 30 min" in out, out


@case
def tripwire_missing_or_garbage_stamp_is_unknown():
    for wf in TRIPWIRE_WORKFLOWS:
        for kwargs in ({}, {"stamp": "08"}, {"stamp": "abc"}, {"stamp": str(FAKE_NOW + 3600)}):
            out = _run_tripwire(wf, **kwargs)
            assert TITLE_TRIPWIRE_UNKNOWN in out, f"{kwargs}: {out}"
            assert TITLE_TRIPWIRE_WARN_TAIL not in out, f"{kwargs}: {out}"


def main() -> int:
    if not BASH or not REAL_DATE:
        print("FAIL  environment: bash and date are required")
        return 1
    probe = subprocess.run([REAL_DATE, "-u", "-d", "2026-01-01T00:00:00Z", "+%s"],
                           capture_output=True, text=True)
    if probe.returncode != 0:
        print("FAIL  environment: GNU `date -u -d` is required (the marker uses it)")
        return 1
    failed = _run_all()
    print(f"\n{len(_cases) - len(failed)}/{len(_cases)} passed")
    if failed:
        print("FAILED: " + ", ".join(failed))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
