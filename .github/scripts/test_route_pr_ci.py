#!/usr/bin/env python3
"""Hermetic tests for route_pr_ci.py, plus tree pins for the public-pool safety rules.

Run: python3 -B .github/scripts/test_route_pr_ci.py   (stdlib + PyYAML, no network)

The tree pins read .github/workflows/*.yml and fail when a change would let untrusted
or secret-bearing work reach a self-hosted host:
  * a workflow triggered by pull_request_target / workflow_run / issue_comment has a job
    whose runs-on can name `self-hosted`, `public-pool` or `qontinui`;
  * a release/secret-bearing workflow (D5) consumes the route outputs;
  * a routed job's step that needs sudo or hosted-only actions is not lane-guarded;
  * a public-pool runs-on omits its OS label, or names the private `qontinui` label.
"""

from __future__ import annotations

import os
import re
import sys
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import route_pr_ci as r  # noqa: E402

try:
    import yaml  # type: ignore
except ImportError:  # pragma: no cover
    yaml = None

WORKFLOWS = HERE.parent / "workflows"
NOW = datetime(2026, 10, 3, 20, 0, tzinfo=timezone.utc)


def call(os_key="linux", **kw):
    args = dict(
        event="pull_request",
        lane_raw="auto",
        lane_source="test",
        same_repo=True,
        bot_author=False,
        policy=None,
        policy_error="GET -> HTTP 403",
        pool=r.PoolObservation(),
        pool_error=None,
        stall_minutes=10,
        max_queue_depth=3,
    )
    args.update(kw)
    return r.decide(os_key, **args)


class DecideTests(unittest.TestCase):
    def test_unset_lane_is_hosted(self):
        for raw in (None, "", "  "):
            d = call(lane_raw=raw)
            self.assertEqual(d.lane, "hosted", raw)
            self.assertEqual(d.labels, ["ubuntu-22.04"])

    def test_invalid_lane_is_hosted_unknown(self):
        d = call(lane_raw="yes-please")
        self.assertEqual(d.lane, "hosted")
        self.assertIn("UNKNOWN", d.reason)

    def test_push_events_never_route(self):
        for event in ("push", "schedule", "merge_group", ""):
            self.assertEqual(call(event=event, lane_raw="self-hosted").lane, "hosted", event)

    def test_same_repo_auto_healthy_routes_self_hosted(self):
        d = call()
        self.assertEqual(d.lane, "self-hosted")
        self.assertEqual(d.labels, ["self-hosted", "Linux", "public-pool"])

    def test_windows_pool_needs_onboarding_label(self):
        d = call("windows", lane_raw="self-hosted")
        self.assertEqual(d.labels, ["self-hosted", "Windows", "public-pool", "qontinui-windows"])
        self.assertEqual(call("windows", lane_raw=None).labels, ["windows-latest"])

    def test_fork_pr_with_unknown_policy_is_hosted_even_when_forced(self):
        for lane in ("auto", "self-hosted"):
            d = call(same_repo=False, lane_raw=lane)
            self.assertEqual(d.lane, "hosted", lane)
            self.assertIn("fork PR", d.reason)

    def test_bot_authored_pr_is_hosted_even_when_forced(self):
        for lane in ("auto", "self-hosted"):
            self.assertEqual(call(lane_raw=lane, bot_author=True).lane, "hosted", lane)

    def test_fork_pr_with_proven_policy_may_route(self):
        d = call(same_repo=False, policy="all_external_contributors", policy_error=None)
        self.assertEqual(d.lane, "self-hosted")

    def test_weaker_policy_fails_closed_for_everyone(self):
        for policy in ("first_time_contributors", "first_time_contributors_new_to_github", "none"):
            for same in (True, False):
                d = call(same_repo=same, lane_raw="self-hosted", policy=policy, policy_error=None)
                self.assertEqual(d.lane, "hosted", (policy, same))
                self.assertIn("fail closed", d.reason)

    def test_auto_pool_unreadable_is_hosted(self):
        d = call(pool=None, pool_error="HTTP 500")
        self.assertEqual(d.lane, "hosted")
        self.assertIn("UNKNOWN", d.reason)

    def test_forced_self_hosted_skips_pool_health(self):
        d = call(lane_raw="self-hosted", pool=None, pool_error="HTTP 500")
        self.assertEqual(d.lane, "self-hosted")

    def test_stall_and_saturation(self):
        stalled = r.PoolObservation(queued=1, stalled=[("test (run 1)", 12.0)])
        self.assertEqual(call(pool=stalled).lane, "hosted")
        deep = r.PoolObservation(queued=3)
        self.assertEqual(call(pool=deep).lane, "hosted")
        shallow = r.PoolObservation(queued=2)
        self.assertEqual(call(pool=shallow).lane, "self-hosted")


class ObserveTests(unittest.TestCase):
    def job(self, labels, minutes_ago, status="queued"):
        return {
            "name": "t",
            "run_id": 1,
            "status": status,
            "labels": labels,
            "created_at": (NOW - timedelta(minutes=minutes_ago)).isoformat().replace("+00:00", "Z"),
        }

    def test_only_exact_pool_label_sets_count(self):
        obs = {k: r.PoolObservation() for k in r.POOL_LABELS}
        jobs = [
            self.job(["self-hosted", "Linux", "public-pool"], 11),
            self.job(["self-hosted", "linux", "PUBLIC-POOL"], 2),  # case-insensitive
            self.job(["self-hosted", "Linux", "public-pool", "dell-2020"], 30),  # superset: one host, not the pool
            self.job(["self-hosted", "qontinui"], 30),  # private pool
            self.job(["ubuntu-22.04"], 30),
            self.job(["self-hosted", "Linux", "public-pool"], 50, status="in_progress"),
        ]
        r.observe_jobs(obs, jobs, NOW, 10)
        self.assertEqual(obs["linux"].queued, 2)
        self.assertEqual(len(obs["linux"].stalled), 1)
        self.assertEqual(obs["windows"].queued, 0)


class MainNeverFailsTests(unittest.TestCase):
    def test_main_writes_every_output_without_network(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            env = {
                "EVENT_NAME": "pull_request",
                "GITHUB_REPOSITORY": "qontinui/qontinui-runner",
                "HEAD_REPO": "qontinui/qontinui-runner",
                "GITHUB_OUTPUT": str(out),
                "RUNNER_LINUX_LANE": "",
                "RUNNER_WINDOWS_LANE": "",
            }
            saved = dict(os.environ)
            try:
                os.environ.clear()
                os.environ.update(env)
                self.assertEqual(r.main(), 0)
            finally:
                os.environ.clear()
                os.environ.update(saved)
            text = out.read_text()
            for key in ("linux_lane=hosted", "linux_labels=[\"ubuntu-22.04\"]", "windows_lane=hosted", "approval_policy="):
                self.assertIn(key, text)


@unittest.skipIf(yaml is None, "PyYAML not installed")
class TreePins(unittest.TestCase):
    UNTRUSTED_TRIGGERS = {"pull_request_target", "workflow_run", "issue_comment"}
    # D5: secret-bearing or host-destructive workflows that must never consume the route.
    NEVER_ROUTED = {"release.yml", "build-python-executor.yml", "published-parity.yml", "reproducibility-gate.yml"}
    FORBIDDEN_RUNS_ON = ("self-hosted", "public-pool", "qontinui")

    def load(self, path):
        with open(path, encoding="utf-8") as fh:
            return yaml.safe_load(fh)

    def workflows(self):
        return sorted(WORKFLOWS.glob("*.yml"))

    def triggers(self, doc):
        on = doc.get(True, doc.get("on"))
        if isinstance(on, dict):
            return set(on)
        if isinstance(on, list):
            return set(on)
        return {on}

    def test_untrusted_triggers_never_reach_a_self_hosted_label(self):
        for path in self.workflows():
            doc = self.load(path)
            if not (self.triggers(doc) & self.UNTRUSTED_TRIGGERS):
                continue
            for job_key, job in (doc.get("jobs") or {}).items():
                text = str(job.get("runs-on", ""))
                for word in self.FORBIDDEN_RUNS_ON:
                    self.assertNotIn(word, text, f"{path.name}#{job_key}: {sorted(self.triggers(doc))} job may not run on {word!r}")
                self.assertNotIn("needs.route", text, f"{path.name}#{job_key} consumes the route")

    def test_secret_bearing_workflows_do_not_consume_the_route(self):
        # Not just "no public-pool": a bare [self-hosted, Linux] would also match the
        # pool runner, so these workflows may name no self-hosted label at all.
        for name in self.NEVER_ROUTED:
            doc = self.load(WORKFLOWS / name)
            text = (WORKFLOWS / name).read_text(encoding="utf-8")
            self.assertNotIn("route_pr_ci", text, name)
            self.assertNotIn("needs.route", text, name)
            for job_key, job in (doc.get("jobs") or {}).items():
                ro = str(job.get("runs-on", "")).lower()
                for word in ("self-hosted", "public-pool", "qontinui"):
                    self.assertNotIn(word, ro, f"{name}#{job_key} runs-on names {word!r}")

    def test_public_pool_labels_name_an_os_and_never_qontinui(self):
        for labels in r.POOL_LABELS.values():
            low = {label.lower() for label in labels}
            self.assertIn("public-pool", low)
            self.assertTrue(low & {"linux", "windows"})
            self.assertNotIn("qontinui", low)

    def routed_jobs(self):
        doc = self.load(WORKFLOWS / "ci.yml")
        return {k: j for k, j in doc["jobs"].items() if "needs.route" in str(j.get("runs-on", ""))}

    def test_routed_jobs_fail_open_to_hosted(self):
        jobs = self.routed_jobs()
        self.assertTrue(jobs, "no ci.yml job consumes the route")
        for key, job in jobs.items():
            self.assertEqual(job.get("needs"), "route", key)
            self.assertEqual(str(job.get("if", "")).replace(" ", ""), "${{!cancelled()}}", key)

    GUARD = "env.SELF_HOSTED_LANE != 'true'"
    HOST_MUTATING_RUN = re.compile(r"\b(sudo|apt-get|apt|swapoff|swapon|mkswap|fallocate)\b")
    HOSTED_ONLY_ACTIONS = ("Swatinem/rust-cache", "jlumbroso/free-disk-space", "al-cheb/configure-pagefile-action")

    @staticmethod
    def squash(text):
        return " ".join(str(text).split())

    @classmethod
    def guarded(cls, cond):
        # The guard must be a TOP-LEVEL `&&` conjunct of an `if:` with no `||`, so
        # `x || env.SELF_HOSTED_LANE != 'true'` (true on the pool) does not count.
        c = cls.squash(cond)
        if "||" in c:
            return False
        return cls.GUARD in [part.strip() for part in c.split("&&")]

    @classmethod
    def blanked(cls, expr):
        # `env.SELF_HOSTED_LANE != 'true' && secrets.X || ''` -- exactly this shape.
        return re.fullmatch(r"env\.SELF_HOSTED_LANE != 'true' && secrets\.[A-Z0-9_]+ \|\| ''", cls.squash(expr)) is not None

    def test_routed_jobs_guard_host_mutating_steps(self):
        # The guard must point the right way: `!= 'true'` (skip on the pool), never
        # `== 'true'`, which would run the step ONLY on the pool.
        for key, job in self.routed_jobs().items():
            for step in job.get("steps", []):
                uses = str(step.get("uses", ""))
                run = str(step.get("run", ""))
                if (any(a in uses for a in self.HOSTED_ONLY_ACTIONS) or "dtolnay/rust-toolchain" in uses
                        or self.HOST_MUTATING_RUN.search(run)):
                    self.assertTrue(self.guarded(step.get("if", "")),
                                    f"{key}: step {step.get('name') or uses} is not skipped on the pool")

    def test_routed_jobs_deliver_no_secret_to_the_pool(self):
        # Every `secrets.` occurrence anywhere in a routed job (job env, step env,
        # with:, run:) must sit in an expression that blanks it on the pool, or in a
        # step skipped on the pool. `secrets: inherit` is refused outright.
        expr = re.compile(r"\$\{\{(.*?)\}\}", re.S)
        for key, job in self.routed_jobs().items():
            self.assertNotIn("secrets", job, f"{key}: reusable-workflow secrets on a routed job")
            for env_key, value in (job.get("env") or {}).items():
                self.assertNotIn("secrets.", str(value), f"{key}: job-level env {env_key} carries a secret")
            for step in job.get("steps", []):
                if self.guarded(step.get("if", "")):
                    continue
                text = yaml.safe_dump({k: v for k, v in step.items() if k != "if"})
                for m in expr.finditer(text):
                    e = self.squash(m.group(1))
                    if "secrets." in e:
                        self.assertTrue(self.blanked(e),
                                        f"{key}: step {step.get('name')} delivers a secret to the pool: {e}")
                self.assertNotIn("secrets.", expr.sub("", text), f"{key}: step {step.get('name')} names a secret outside an expression")

    def test_workflow_level_env_carries_only_the_run_token(self):
        doc = self.load(WORKFLOWS / "ci.yml")
        for k, v in (doc.get("env") or {}).items():
            if "secrets." in str(v):
                self.assertEqual(self.squash(v), "${{ secrets.GITHUB_TOKEN }}", f"workflow env {k}")

    def test_no_unrouted_job_can_reach_a_self_hosted_runner(self):
        # Any job, in any workflow, whose runs-on names self-hosted/public-pool must
        # be one of the pinned routed ci.yml jobs (toolcache-probe is the one known
        # dispatch-only exception, pending deletion in qontinui-runner#1966).
        allowed = {("ci.yml", k) for k in self.routed_jobs()} | {("toolcache-probe.yml", "probe")}
        for path in self.workflows():
            doc = self.load(path)
            for key, job in (doc.get("jobs") or {}).items():
                ro = str(job.get("runs-on", "")).lower()
                if "self-hosted" in ro or "public-pool" in ro:
                    self.assertIn((path.name, key), allowed, f"{path.name}#{key} can reach a self-hosted runner")

    def test_route_job_if_is_pinned(self):
        doc = self.load(WORKFLOWS / "ci.yml")
        self.assertEqual(
            self.squash(doc["jobs"]["route"]["if"]),
            "${{ (github.event_name == 'pull_request' && vars.RUNNER_LINUX_LANE != '' && vars.RUNNER_LINUX_LANE != 'hosted') "
            "|| (github.event_name == 'workflow_dispatch' && (inputs.force_linux_lane == 'self-hosted' || "
            "(inputs.force_linux_lane != 'hosted' && vars.RUNNER_LINUX_LANE != '' && vars.RUNNER_LINUX_LANE != 'hosted'))) }}")

    def test_routed_runs_on_are_pinned(self):
        pool = "'[\"self-hosted\",\"Linux\",\"public-pool\"]'"
        want = {
            "test": "${{ fromJSON(matrix.platform == 'ubuntu-22.04' && needs.route.outputs.linux_lane == 'self-hosted' && " + pool + " || toJSON(matrix.platform)) }}",
            "holder-crates": "${{ fromJSON(matrix.platform == 'ubuntu-latest' && needs.route.outputs.linux_lane == 'self-hosted' && " + pool + " || toJSON(matrix.platform)) }}",
            "frontend-tests": "${{ fromJSON(needs.route.outputs.linux_lane == 'self-hosted' && " + pool + " || '\"ubuntu-latest\"') }}",
        }
        jobs = self.routed_jobs()
        self.assertEqual(set(jobs), set(want))
        for key, expected in want.items():
            self.assertEqual(self.squash(jobs[key]["runs-on"]), expected, key)
        lane = {
            "test": "${{ matrix.platform == 'ubuntu-22.04' && needs.route.outputs.linux_lane == 'self-hosted' }}",
            "holder-crates": "${{ matrix.platform == 'ubuntu-latest' && needs.route.outputs.linux_lane == 'self-hosted' }}",
            "frontend-tests": "${{ needs.route.outputs.linux_lane == 'self-hosted' }}",
        }
        for key, expected in lane.items():
            self.assertEqual(self.squash(jobs[key]["env"]["SELF_HOSTED_LANE"]), expected, key)

    def test_every_routed_job_prepares_first(self):
        for key, job in self.routed_jobs().items():
            steps = job.get("steps", [])
            self.assertTrue(str(steps[0].get("uses", "")).startswith("actions/checkout@"), key)
            self.assertIn("public-pool-prepare", str(steps[1].get("uses", "")), f"{key}: step 2 must be public-pool-prepare")
            self.assertEqual(steps[1].get("if"), "env.SELF_HOSTED_LANE == 'true'", key)
            self.assertEqual(sum("public-pool-prepare" in str(st.get("uses", "")) for st in steps), 1, key)

    def test_prepare_step_clears_exactly_the_checked_out_siblings(self):
        for key, job in self.routed_jobs().items():
            steps = job.get("steps", [])
            sibs = set()
            for step in steps:
                if "checkout-sibling" in str(step.get("uses", "")):
                    repo = (step.get("with") or {}).get("repo", "qontinui/qontinui-schemas")
                    sibs.add(repo.split("/")[-1])
            prepares = [st for st in steps if "public-pool-prepare" in str(st.get("uses", ""))]
            if not sibs:
                continue
            self.assertEqual(len(prepares), 1, key)
            self.assertEqual(prepares[0]["if"], "env.SELF_HOSTED_LANE == 'true'", key)
            self.assertEqual(set(prepares[0]["with"]["siblings"].split()), sibs, key)
            first_sibling = min(i for i, st in enumerate(steps) if "checkout-sibling" in str(st.get("uses", "")))
            self.assertLess(steps.index(prepares[0]), first_sibling, key)

    def test_route_job_is_hosted_and_cannot_fail_its_dependants(self):
        doc = self.load(WORKFLOWS / "ci.yml")
        route = doc["jobs"]["route"]
        self.assertEqual(route["runs-on"], "ubuntu-latest")
        self.assertEqual(route["permissions"].get("actions"), "read")
        self.assertIn("|| true", str(route["steps"][-1].get("run", "")))


@unittest.skipIf(yaml is None, "PyYAML not installed")
class PrepareActionShell(unittest.TestCase):
    """Runs public-pool-prepare's actual shell body against temp dirs.

    The YAML-shape pins cannot see a shell defect (a glob under `set -f` once made
    the size cap dead code), so this executes the script itself.
    """

    ACTION = HERE.parent / "actions" / "public-pool-prepare" / "action.yml"

    def run_prepare(self, tmp, **env_over):
        import subprocess

        with open(self.ACTION, encoding="utf-8") as fh:
            script = yaml.safe_load(fh)["runs"]["steps"][0]["run"]
        tmp = Path(tmp)
        for d in ("work/qontinui-runner", "home", "rt"):
            (tmp / d).mkdir(parents=True, exist_ok=True)
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": str(tmp / "home"),
            "RUNNER_OS": "Linux",
            "RUNNER_TEMP": str(tmp / "rt"),
            "GITHUB_RUN_ID": "7",
            "GITHUB_WORKSPACE": str(tmp / "work/qontinui-runner"),
            "GITHUB_ENV": str(tmp / "env"),
            "GITHUB_PATH": str(tmp / "path"),
            "SIBLINGS": "",
            "CHECK_BUILD_DEPS": "false",
            "CHECK_RUST": "false",
            "MAX_TARGET_GB": "200",
            "TARGET_KEY": "",
        }
        env.update(env_over)
        return subprocess.run(["bash", "-c", script], env=env, capture_output=True, text=True)

    def test_no_glob_while_set_f(self):
        text = self.ACTION.read_text(encoding="utf-8")
        self.assertNotRegex(text, r'"\}?/\*/?', "a glob would be inert under set -f")

    def test_siblings_removed_symlinks_not_followed(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            (t / "work/ui-bridge/x").mkdir(parents=True)
            outside = t / "outside"
            outside.mkdir()
            (outside / "f").write_text("keep")
            outside.chmod(0o555)
            (t / "work/qontinui-schemas").symlink_to(outside)
            r = self.run_prepare(tmp, SIBLINGS="ui-bridge qontinui-schemas")
            mode_after = outside.stat().st_mode & 0o777
            outside.chmod(0o755)
            self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
            self.assertFalse((t / "work/ui-bridge").exists())
            self.assertFalse((t / "work/qontinui-schemas").is_symlink())
            self.assertTrue((outside / "f").exists())
            self.assertEqual(oct(mode_after), oct(0o555), "symlink target was chmod-ed through the link")
            self.assertTrue((t / "work/qontinui-runner").exists())

    def test_bad_sibling_name_refused(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            self.assertNotEqual(self.run_prepare(tmp, SIBLINGS="../x").returncode, 0)
            self.assertNotEqual(self.run_prepare(tmp, SIBLINGS="*").returncode, 0)

    def test_env_exports_and_no_target_without_key(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            r = self.run_prepare(tmp)
            self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
            env = (Path(tmp) / "env").read_text()
            self.assertNotIn("CARGO_TARGET_DIR=", env)
            for key in ("HOME=", "CARGO_HOME=", "RUSTUP_HOME=", "XDG_CONFIG_HOME=", "XDG_CACHE_HOME="):
                self.assertIn(key, env)
            self.assertIn(f"HOME={tmp}/rt/home", env)

    def test_cap_prunes_oldest_first_and_stale_by_age(self):
        import tempfile
        import time

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "home/.cache/qontinui-runner-ci/targets"
            for name, age_days in (("pr-old", 1), ("pr-new", 0), ("pr-stale", 5)):
                d = root / name
                d.mkdir(parents=True)
                (d / "blob").write_bytes(b"x" * 1024)
                t = time.time() - age_days * 86400
                os.utime(d, (t, t))
            r = self.run_prepare(tmp, TARGET_KEY="pr-9-test", MAX_TARGET_GB="0")
            self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
            left = sorted(p.name for p in root.iterdir())
            self.assertNotIn("pr-stale", left)   # age prune
            self.assertNotIn("pr-old", left)     # cap prune, oldest first
            self.assertIn("pr-9-test", left)
            env = (Path(tmp) / "env").read_text()
            self.assertIn(f"CARGO_TARGET_DIR={root}/pr-9-test", env)

    def test_bad_target_key_refused(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            self.assertNotEqual(self.run_prepare(tmp, TARGET_KEY="../escape").returncode, 0)

    def test_writable_rustup_home_refused(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "home/.rustup").mkdir(parents=True)
            r = self.run_prepare(tmp, CHECK_RUST="true")
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("RUSTUP_HOME is not operator-owned", r.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)
