"""The clean-room scenario: an external operator's first hour, measured.

Plan 2026-09-20-the-published-product-works-without-knowing-a-development-
environment-exists, Phase E. Drives the INSTALLED published runner, on a box the
preflight showed to be foreign, against a generated foreign repo, with NO coord
credential (Fork 3: unpaired), and records one `{step, outcome, reason,
evidence_ref, detail}` per step:

  preflight               the box is foreign (scripts/clean_room/preflight.py)
  install                 the published installer produced the product exe
  first_launch            the exe, started the way an operator starts it
                          (default port, default config, no QONTINUI_*),
                          answers /health responsive
  ui_bridge_ready         a UI Bridge round-trip completes and its control
                          routes answer -- the precondition for every UI step
  open_repo               the foreign repo is added and its project card is
                          rendered on the Projects page
  start_session           the card's "Work on it" action binds a live session
                          to the foreign repo
  session_lists_commands  that session was provisioned with slash commands
                          (the v1.0.10 regression, as a permanent assertion)
  author_plan             a plan can be authored through the UI with no plans
                          directory configured (unknown until a plan-authoring
                          control is named: no substring search is a measurement)
  unpaired_refusal        a tenant-scoped request on an unpaired runner is
                          refused AS a pairing refusal, with a structured
                          envelope: a `next_action` of a known kind, and no
                          fleet noun
  glossary                GET /glossary serves the compiled-in glossary
  stop_reason             the session's stop reason is served after it ends
  close                   the runner exits ON ITS OWN when its main window is
                          closed (a harness kill is recorded as forced, a fail)

DRIVING. Everything is HTTP to the runner's UI Bridge and its own routes, with
two stated exceptions: adding a project goes through the UI Bridge INVOKE door
(`add_saved_project`), because the Projects page's own control opens a native
folder picker that nothing can drive over HTTP; and `session_lists_commands`
reads the provisioned `.claude/commands/` in the session's working directory,
because no route lists a session's commands. Both are observations of what the
product did, not substitutes for it.

NO AGENT CLI. A GitHub-hosted box has no agent CLI and no agent account, so the
session is the plain session the product binds to a project; what is measured is
the product's own work (binding, provisioning, refusing), not a model's.

HONESTY RULES, each asserted by scripts/tests/test_clean_room.py:
  * A step that depends on one that did not pass is unknown(prior_step_not_passed)
    -- or the upstream's own unknown reason when that reason is box-wide
    (box_not_clean, ui_bridge_unreachable) -- never a fail it did not earn.
  * A route this release does not serve is unknown(feature_not_released), never
    a pass. /glossary and /sessions/{id}/stop-reason are such routes until their
    runner phases ship.
  * Every response body and every rendered UI text is observed and scanned for
    fleet nouns; the list is reported whatever the steps concluded.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import re
import sys
import tempfile
import time
import uuid
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Protocol

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    from clean_room import artifact
    from clean_room.fleet_nouns import (
        Vocabulary,
        VocabularyUnavailable,
        load_vocabulary,
    )
    from clean_room.outcome import StepResult, failed, passed, unknown
    from clean_room.transport import (
        Observer,
        Runner,
        TransportError,
        urllib_transport,
    )
else:
    from . import artifact
    from .fleet_nouns import Vocabulary, VocabularyUnavailable, load_vocabulary
    from .outcome import StepResult, failed, passed, unknown
    from .transport import Observer, Runner, TransportError, urllib_transport

STEPS = (
    "preflight",
    "install",
    "first_launch",
    "ui_bridge_ready",
    "open_repo",
    "start_session",
    "session_lists_commands",
    "author_plan",
    "unpaired_refusal",
    "glossary",
    "stop_reason",
    "close",
)

# Which earlier steps each step needs to have PASSED before it is attempted.
DEPENDS: dict[str, tuple[str, ...]] = {
    "preflight": (),
    "install": ("preflight",),
    "first_launch": ("install",),
    "ui_bridge_ready": ("first_launch",),
    "open_repo": ("ui_bridge_ready",),
    "start_session": ("open_repo",),
    "session_lists_commands": ("start_session",),
    "author_plan": ("ui_bridge_ready",),
    "unpaired_refusal": ("first_launch",),
    "glossary": ("first_launch",),
    "stop_reason": ("start_session",),
    "close": ("first_launch",),
}

# An upstream unknown whose reason is about the whole box or the whole UI is
# propagated as-is: every downstream step is unknown for that SAME reason.
PROPAGATED_REASONS = frozenset(
    {
        "box_not_clean",
        "preflight_incomplete",
        "vocabulary_unavailable",
        "ui_bridge_unreachable",
    }
)

# The producer kinds of qontinui-schemas `NextActionKind` (rust/src/refusal.rs,
# `NextActionKind::ALL`), snake_case on the wire. `unrecognised` is reader-side
# only and a producer must not emit it. Pinned against the sibling source by
# scripts/tests/test_clean_room.py (hard at PR time), so a new kind fails the
# selftest until added. The nightly run instead parses refusal.rs at the
# vocabulary's commit (--refusal-rs) and records any drift in the block, so an
# upstream variant never stops the measurement.
NEXT_ACTION_KINDS = frozenset(
    {
        "retry_later",
        "run_command",
        "open_page",
        "sign_in",
        "pair_device",
        "set_setting",
        "wait_for_gate",
        "none_terminal",
        "report_defect",
        "fix_request",
        "resnapshot",
        "scroll_into_view",
        "wait_for_enabled",
        "broaden_selector",
    }
)

DRAIN_CODES = ("drain_unreadable", "device_drained")

# The tenant-pairing refusal names itself: its code (`tenant_not_paired`) or a
# message naming both the tenant and the pairing. Any other refusal on the
# probe (a drain, a bad working directory, a 404, a 500) is NOT the refusal the
# step is about, and is reported as unknown(refusal_not_observed).
_PAIRING_REFUSAL = re.compile(
    r"tenant_not_paired|tenant[^\n]{0,120}pair|pair[^\n]{0,120}tenant", re.IGNORECASE
)


def parse_next_action_kinds(refusal_rs: str) -> frozenset[str]:
    """The producer kinds of `NextActionKind`, read from qontinui-schemas
    rust/src/refusal.rs: every unit variant of the enum, snake_cased, minus the
    reader-side `unrecognised`. Raises ValueError when nothing parses."""
    start = refusal_rs.index("pub enum NextActionKind")
    body = refusal_rs[start : refusal_rs.index("\n}", start)]
    variants = re.findall(r"^\s{4}([A-Z][A-Za-z]+),?\s*$", body, re.MULTILINE)
    kinds = {re.sub(r"(?<!^)([A-Z])", r"_\1", v).lower() for v in variants}
    kinds.discard("unrecognised")
    if not kinds:
        raise ValueError("no NextActionKind variants parsed")
    return frozenset(kinds)


class Process(Protocol):
    def alive(self) -> bool: ...
    def exit_code(self) -> int | None: ...
    def stop(self) -> int | None: ...


@dataclass
class Clock:
    now: Callable[[], float] = time.monotonic
    sleep: Callable[[float], None] = time.sleep


@dataclass
class Config:
    fixture_path: Path
    vocabulary_path: str | None
    evidence_dir: Path
    health_timeout_s: float = 180.0
    ui_bridge_timeout_s: float = 120.0
    session_timeout_s: float = 60.0
    drain_retry_s: float = 60.0
    close_timeout_s: float = 60.0
    poll_s: float = 1.0


@dataclass
class Scenario:
    config: Config
    runner: Runner
    preflight: dict
    install: dict
    start_process: Callable[[str], Process]
    clock: Clock = field(default_factory=Clock)
    next_action_kinds: frozenset[str] = NEXT_ACTION_KINDS
    results: dict[str, StepResult] = field(default_factory=dict)
    vocab: Vocabulary | None = None
    vocab_error: str | None = None
    process: Process | None = None
    project_id: str | None = None
    terminal_id: str | None = None

    # ------------------------------------------------------------------
    def run(self) -> list[StepResult]:
        if self.config.vocabulary_path:
            try:
                self.vocab = load_vocabulary(self.config.vocabulary_path)
            except VocabularyUnavailable as exc:
                self.vocab_error = str(exc)
        else:
            self.vocab_error = "no vocabulary path given"
        try:
            for step in STEPS:
                self.results[step] = self._run_step(step)
        finally:
            if self.process is not None and self.process.alive():
                self.process.stop()
        return [self.results[s] for s in STEPS]

    def _run_step(self, step: str) -> StepResult:
        for dep in DEPENDS[step]:
            r = self.results[dep]
            if r.outcome != "pass":
                if r.outcome == "unknown" and r.reason in PROPAGATED_REASONS:
                    return unknown(
                        step, r.reason, r.evidence_ref, f"{dep} was unknown({r.reason})"
                    )
                return unknown(
                    step,
                    "prior_step_not_passed",
                    r.evidence_ref,
                    f"{dep} was {r.outcome}({r.reason})",
                )
        self.runner.step = step
        try:
            return getattr(self, f"step_{step}")()
        except TransportError as exc:
            return unknown(step, "transport_error", None, str(exc))
        except Exception as exc:  # noqa: BLE001 - a harness bug is reported, never a verdict
            return unknown(step, "harness_error", None, f"{type(exc).__name__}: {exc}")

    # ------------------------------------------------------------------
    # Helpers.
    # ------------------------------------------------------------------
    def _observe_ui(self) -> str | None:
        """Capture the rendered page's visible text for the fleet-noun scan.

        `page/summary` is POST-only on the runner. Its body counts as rendered
        UI text only when it answered 2xx (Runner.request `ui=True`).
        """
        try:
            _, ref = self.runner.request(
                "POST", "/ui-bridge/control/page/summary", {}, timeout=30, ui=True
            )
            return ref
        except TransportError:
            return None

    def _wait(self, timeout_s: float, probe: Callable[[], Any]) -> Any:
        deadline = self.clock.now() + timeout_s
        while True:
            got = probe()
            if got:
                return got
            if self.clock.now() >= deadline:
                return None
            self.clock.sleep(self.config.poll_s)

    @staticmethod
    def _code_of(body: Any) -> str:
        if isinstance(body, dict):
            if isinstance(body.get("code"), str):
                return body["code"]
            ed = body.get("error_detail")
            if isinstance(ed, dict) and isinstance(ed.get("code"), str):
                return ed["code"]
            if isinstance(body.get("error"), str):
                return body["error"]
        return ""

    # ------------------------------------------------------------------
    # Steps.
    # ------------------------------------------------------------------
    def step_preflight(self) -> StepResult:
        v = self.preflight.get("verdict")
        detail = json.dumps(
            self.preflight.get("findings") or self.preflight.get("probe_errors") or []
        )[:2000]
        ref = "preflight.json"
        if v == "not_clean":
            return unknown("preflight", "box_not_clean", ref, detail)
        if v == "vocabulary_unavailable":
            return unknown("preflight", "vocabulary_unavailable", ref, detail)
        if v != "clean":
            return unknown(
                "preflight", "preflight_incomplete", ref, detail or f"verdict {v!r}"
            )
        # The foreign repo is the scenario's subject and the runner's working
        # directory; without it every repo-facing step would measure something
        # else (at worst the harness checkout). Never launch without it.
        if not self.config.fixture_path.is_dir():
            return unknown(
                "preflight",
                "harness_error",
                ref,
                f"the foreign fixture repo is missing ({self.config.fixture_path!s})",
            )
        acknowledged = self.preflight.get("acknowledged") or []
        return passed(
            "preflight",
            "no sibling checkout, dev listener, runner, or QONTINUI_* variable under "
            "the probed roots (preflight.json `probed.roots`)",
            ref,
            f"acknowledged: {json.dumps(acknowledged)}" if acknowledged else "",
        )

    def step_install(self) -> StepResult:
        if self.install.get("unreadable"):
            return unknown(
                "install",
                "harness_error",
                "install.json",
                str(self.install.get("unreadable"))[:2000],
            )
        exe = self.install.get("exe")
        if exe:
            return passed(
                "install",
                "the installed runner exe was located by the shared locator",
                "install.json",
                exe,
            )
        return failed(
            "install",
            "the verified installer ran but the locator found no installed runner exe",
            "install.json",
            str(self.install.get("error") or "")[:2000],
        )

    def step_first_launch(self) -> StepResult:
        # The installer auto-launches the product and the install action stops
        # that instance; if anything still answers on the product port, this
        # launch would lose the single-instance race and the run would measure
        # the survivor. Refuse to measure rather than measure the wrong process.
        try:
            resp, ref = self.runner.get("/health", timeout=5)
        except TransportError:
            pass
        else:
            return unknown(
                "first_launch",
                "box_not_clean",
                ref,
                f"something answered /health ({resp.status}) on the product port "
                "before this run launched the runner",
            )

        self.process = self.start_process(self.install["exe"])
        last: dict[str, Any] = {}

        def probe() -> bool:
            if not self.process.alive():
                return True
            try:
                resp, ref = self.runner.get("/health", timeout=10)
            except TransportError:
                return False
            last["ref"] = ref
            data = resp.envelope_data
            last["ok"] = (
                resp.status == 200
                and isinstance(data, dict)
                and data.get("responsive") is True
            )
            return last["ok"]

        self._wait(self.config.health_timeout_s, probe)
        note = (
            "the installer's own auto-launched instance ran first and was stopped by "
            "the install action, so first-run state may already exist"
        )
        if last.get("ok") and self.process.alive():
            return passed(
                "first_launch",
                "/health answered responsive:true on the default port with default config",
                last["ref"],
                note,
            )
        if not self.process.alive():
            return failed(
                "first_launch",
                f"the runner exited during first launch (exit code {self.process.exit_code()})",
                last.get("ref"),
                note,
            )
        return failed(
            "first_launch",
            f"the runner did not answer /health responsive:true within {int(self.config.health_timeout_s)}s",
            last.get("ref"),
            note,
        )

    def step_ui_bridge_ready(self) -> StepResult:
        last: dict[str, Any] = {}

        def probe() -> bool:
            try:
                resp, ref = self.runner.get("/health", timeout=10)
            except TransportError:
                return False
            last["ref"] = ref
            data = resp.envelope_data
            if isinstance(data, dict) and data.get("uiBridgeIpcObserved") is True:
                return True
            # A passive poll never drives the round-trip; poke a cheap route.
            try:
                self.runner.get("/ui-bridge/control/elements", timeout=15)
            except TransportError:
                pass
            return False

        if not self._wait(self.config.ui_bridge_timeout_s, probe):
            return unknown(
                "ui_bridge_ready",
                "ui_bridge_unreachable",
                last.get("ref"),
                f"no UI Bridge round-trip within {int(self.config.ui_bridge_timeout_s)}s",
            )
        resp, ref = self.runner.get(
            "/ui-bridge/control/elements?refresh=true", timeout=30
        )
        data = resp.envelope_data
        elements = data.get("elements") if isinstance(data, dict) else data
        if resp.status != 200 or not isinstance(elements, list):
            return unknown(
                "ui_bridge_ready",
                "ui_bridge_unreachable",
                ref,
                f"elements route answered {resp.status}",
            )
        self._observe_ui()
        return passed(
            "ui_bridge_ready",
            f"UI Bridge round-trip observed; {len(elements)} element(s) registered",
            ref,
        )

    def step_open_repo(self) -> StepResult:
        fx = self.config.fixture_path
        project = {
            "path": str(fx),
            "name": fx.name,
            "projectType": "unknown",
            "manifest": "go.mod",
        }
        resp, ref = self.runner.post(
            "/ui-bridge/invoke/add_saved_project",
            {"args": {"project": project}},
            timeout=60,
        )
        if resp.status in (503, 504):
            return unknown(
                "open_repo",
                "ui_bridge_unreachable",
                ref,
                f"invoke answered {resp.status}",
            )
        if resp.status != 200:
            return failed(
                "open_repo",
                f"add_saved_project was refused ({resp.status})",
                ref,
                self._code_of(resp.json),
            )

        resp, ref = self.runner.post(
            "/ui-bridge/invoke/list_saved_projects", {"args": {}}, timeout=60
        )
        saved = resp.envelope_data
        if isinstance(saved, dict):
            saved = saved.get("result", saved.get("projects"))
        match = None
        if isinstance(saved, list):
            want = os.path.normcase(os.path.normpath(str(fx)))
            for p in saved:
                if (
                    isinstance(p, dict)
                    and os.path.normcase(os.path.normpath(str(p.get("path", ""))))
                    == want
                ):
                    match = p
        if match is None:
            return failed(
                "open_repo", "the added project is not in the saved-projects list", ref
            )
        self.project_id = str(match.get("id") or "")
        if not self.project_id:
            # An empty id would make the card check below match ANY card.
            return failed("open_repo", "the saved project carries no id", ref)

        self.runner.post(
            "/ui-bridge/control/tab/activate", {"tabId": "projects"}, timeout=30
        )
        card = f"projects.card-{self.project_id}"

        def card_rendered() -> str | None:
            r, cref = self.runner.get("/ui-bridge/control/components", timeout=30)
            return (
                cref
                if re.search(re.escape(card) + r"(?![A-Za-z0-9_-])", r.body)
                else None
            )

        cref = self._wait(30, card_rendered)
        self._observe_ui()
        if not cref:
            return unknown(
                "open_repo",
                "ui_element_not_found",
                ref,
                f"component {card} not registered on the Projects page",
            )
        return passed(
            "open_repo",
            f"the foreign repo is saved and its card {card} is rendered",
            cref,
        )

    def step_start_session(self) -> StepResult:
        card = f"projects.card-{self.project_id}"
        resp, ref = self.runner.post(
            f"/ui-bridge/control/component/{card}/action/work-on-it",
            {"params": {}},
            timeout=60,
        )
        if resp.status == 404:
            return unknown(
                "start_session",
                "ui_element_not_found",
                ref,
                f"{card} has no work-on-it action",
            )
        if resp.status != 200:
            return failed(
                "start_session",
                f"'Work on it' was refused ({resp.status})",
                ref,
                self._code_of(resp.json),
            )
        want = os.path.normcase(os.path.normpath(str(self.config.fixture_path)))
        found: dict[str, Any] = {}

        def bound() -> bool:
            r, tref = self.runner.get("/terminals", timeout=30)
            found["ref"] = tref
            data = r.envelope_data
            terms = data.get("terminals") if isinstance(data, dict) else data
            for t in terms if isinstance(terms, list) else []:
                wd = t.get("workingDir") or t.get("working_dir") or ""
                if os.path.normcase(os.path.normpath(str(wd))) == want and t.get(
                    "isAlive", t.get("is_alive")
                ):
                    self.terminal_id = str(t.get("id"))
                    return True
            return False

        self._wait(self.config.session_timeout_s, bound)
        self._observe_ui()
        if self.terminal_id:
            return passed(
                "start_session",
                f"a live session {self.terminal_id} is bound to the foreign repo",
                found.get("ref"),
            )
        return failed(
            "start_session",
            f"no live session bound to the foreign repo within {int(self.config.session_timeout_s)}s of 'Work on it'",
            found.get("ref"),
        )

    def step_session_lists_commands(self) -> StepResult:
        cmd_dir = self.config.fixture_path / ".claude" / "commands"
        names = sorted(p.name for p in cmd_dir.glob("*.md")) if cmd_dir.is_dir() else []
        ref = self.runner.observer.observe(
            "session_lists_commands", "provisioned-commands", "\n".join(names)
        )
        if names:
            return passed(
                "session_lists_commands",
                f"{len(names)} slash command(s) provisioned into the session",
                ref,
            )
        return failed(
            "session_lists_commands",
            "the session was provisioned with no slash commands (the v1.0.10 regression shape)",
            ref,
            f"{cmd_dir} {'exists but is empty' if cmd_dir.is_dir() else 'does not exist'}",
        )

    def step_author_plan(self) -> StepResult:
        # No selector for a plan-authoring control is known to this harness, so
        # nothing here can MEASURE its absence: a substring search over tab
        # names is not a measurement. The step stays unknown, with the evidence
        # a later driver needs (every tab id, and whether a plans dir is set),
        # until a plan-authoring surface is named and driven.
        resp, _ = self.runner.get("/settings/paths", timeout=30)
        resolved = (
            resp.envelope_data.get("resolved", {})
            if isinstance(resp.envelope_data, dict)
            else {}
        )
        tier = resolved.get("plan_tier_active") if isinstance(resolved, dict) else None
        resp, ref = self.runner.get("/ui-bridge/control/tabs", timeout=30)
        data = resp.envelope_data
        tabs = data.get("tabs") if isinstance(data, dict) else data
        if resp.status != 200 or not isinstance(tabs, list):
            return unknown(
                "author_plan",
                "response_unparseable",
                ref,
                f"tabs route answered {resp.status}",
            )
        ids = [t.get("id") if isinstance(t, dict) else t for t in tabs]
        return unknown(
            "author_plan",
            "ui_element_not_found",
            ref,
            f"no plan-authoring control is known to the harness; plan_tier_active={tier!r}; "
            f"tabs={json.dumps(ids)[:1500]}",
        )

    def step_unpaired_refusal(self) -> StepResult:
        # A session request scoped to a tenant this runner holds no credential
        # for: the product's pairing refusal. A random tenant id, so no real
        # tenant is ever named by this harness.
        body = {
            "workingDir": str(self.config.fixture_path),
            "title": "clean-room unpaired probe",
            "tenantId": str(uuid.uuid4()),
        }
        deadline = self.clock.now() + self.config.drain_retry_s
        while True:
            resp, ref = self.runner.post("/terminals", body, timeout=60)
            code = self._code_of(resp.json)
            if (
                resp.status == 409
                and any(c in code for c in DRAIN_CODES)
                and self.clock.now() < deadline
            ):
                self.clock.sleep(self.config.poll_s * 5)
                continue
            break
        if 200 <= resp.status < 300:
            data = resp.envelope_data
            if isinstance(data, dict) and data.get("id"):
                self.runner.request(
                    "DELETE", f"/terminals/{data['id']}", None, timeout=30
                )
            return unknown(
                "unpaired_refusal",
                "unexpectedly_paired",
                ref,
                "the tenant-scoped session was created",
            )
        if resp.json is None:
            return unknown(
                "unpaired_refusal", "response_unparseable", ref, f"status {resp.status}"
            )
        if resp.status != 400 or not _PAIRING_REFUSAL.search(resp.body):
            return unknown(
                "unpaired_refusal",
                "refusal_not_observed",
                ref,
                f"status {resp.status} code {code!r}",
            )
        if self.vocab is None:
            return unknown(
                "unpaired_refusal",
                "vocabulary_unavailable",
                ref,
                self.vocab_error or "",
            )
        problems = []
        na = find_next_action(resp.json)
        if na is None:
            problems.append("the refusal carries no next_action")
        elif na.get("kind") not in self.next_action_kinds:
            problems.append(
                f"next_action.kind {na.get('kind')!r} is not a NextActionKind"
            )
        leaks = sorted(
            {
                h.class_id
                for h in self.vocab.scan_text(self.runner.observer.redact(resp.body))
            }
        )
        if leaks:
            problems.append(f"the refusal names fleet nouns ({', '.join(leaks)})")
        if problems:
            return failed(
                "unpaired_refusal",
                "; ".join(problems),
                ref,
                f"status {resp.status} code {code!r}",
            )
        return passed(
            "unpaired_refusal",
            f"refused with next_action.kind={na['kind']} and no fleet noun",
            ref,
        )

    def step_glossary(self) -> StepResult:
        resp, ref = self.runner.get("/glossary", timeout=30)
        if resp.status == 404:
            return unknown(
                "glossary",
                "feature_not_released",
                ref,
                "GET /glossary is not served by this release",
            )
        if resp.status != 200 or resp.json is None:
            return failed("glossary", f"GET /glossary answered {resp.status}", ref)
        data = resp.envelope_data if isinstance(resp.envelope_data, dict) else {}
        missing = [
            k for k in ("version", "source", "build_id", "terms") if k not in data
        ]
        if missing:
            return failed(
                "glossary", f"the glossary response lacks {', '.join(missing)}", ref
            )
        if not isinstance(data["terms"], list) or not data["terms"]:
            return failed("glossary", "the glossary serves no terms", ref)
        return passed(
            "glossary", f"{len(data['terms'])} term(s), version {data['version']}", ref
        )

    def step_stop_reason(self) -> StepResult:
        self.runner.request(
            "DELETE", f"/terminals/{self.terminal_id}", None, timeout=30
        )
        resp, ref = self.runner.get(
            f"/sessions/{self.terminal_id}/stop-reason", timeout=30
        )
        code = self._code_of(resp.json)
        if resp.status == 404:
            low = code.lower()
            if "session" in low and ("not_found" in low or "unknown" in low):
                return failed(
                    "stop_reason",
                    "the route exists but does not know the session this run just ended",
                    ref,
                    code,
                )
            return unknown(
                "stop_reason",
                "feature_not_released",
                ref,
                "GET /sessions/{id}/stop-reason is not served by this release",
            )
        if resp.status != 200 or not isinstance(resp.envelope_data, dict):
            return failed("stop_reason", f"stop-reason answered {resp.status}", ref)
        data = resp.envelope_data
        missing = [
            k for k in ("code", "observed_at", "next_action", "source") if k not in data
        ]
        if missing:
            return failed(
                "stop_reason", f"the stop reason lacks {', '.join(missing)}", ref
            )
        return passed("stop_reason", f"stop reason {data['code']!r} served", ref)

    def step_close(self) -> StepResult:
        # The operator's close: the main window's X button, which tears the
        # whole app down. Pass only when the process then exits ON ITS OWN;
        # a harness kill proves nothing about the product.
        try:
            resp, ref = self.runner.post(
                "/ui-bridge/control/page/close-request", {}, timeout=30
            )
            asked = f"close-request answered {resp.status}"
            accepted = 200 <= resp.status < 300
        except TransportError as exc:
            ref, asked, accepted = None, f"close-request got no answer ({exc})", False

        def exited() -> bool:
            return not self.process.alive()

        on_its_own = accepted and bool(self._wait(self.config.close_timeout_s, exited))
        if on_its_own:
            code = self.process.exit_code()
            return passed(
                "close",
                f"the runner exited on its own after the window close (exit code {code!r})",
                ref,
                f"{asked}; forced: false",
            )
        code = self.process.stop()
        still = self.process.alive()
        why = (
            f"the close request was not accepted ({asked})"
            if not accepted
            else f"the runner was still running {int(self.config.close_timeout_s)}s after the window close"
        )
        return failed(
            "close",
            why,
            ref,
            f"forced: true; harness stop exit code {code!r}; alive after stop: {still}",
        )


def find_next_action(body: Any) -> dict | None:
    """The first `next_action` object anywhere in a response body."""
    if isinstance(body, dict):
        na = body.get("next_action")
        if isinstance(na, dict):
            return na
        for v in body.values():
            got = find_next_action(v)
            if got is not None:
                return got
    elif isinstance(body, list):
        for v in body:
            got = find_next_action(v)
            if got is not None:
                return got
    return None


def fleet_noun_report(
    observer: Observer, vocab: Vocabulary | None, vocab_error: str | None
) -> dict:
    """Every fleet-noun hit across the run's served responses and rendered UI.

    Verdict: `red` on any hit. Otherwise `clean` ONLY when at least one 2xx
    rendered-UI text was scanned -- a run that never read the rendered UI has
    not measured the UI half, whatever its HTTP bodies said -- else `unknown`.
    """
    if vocab is None:
        return {
            "verdict": "unknown",
            "reason": vocab_error or "vocabulary unavailable",
            "scanned_texts": 0,
            "ui_texts_scanned": 0,
            "http_texts_scanned": 0,
            "hits": [],
        }
    hits = []
    for obs, text in observer.texts:
        for h in vocab.scan_text(text):
            hits.append(
                {
                    "class_id": h.class_id,
                    "match": h.match,
                    "source": obs.source,
                    "step": obs.step,
                    "kind": obs.kind,
                    "evidence_ref": obs.ref,
                }
            )
    ui = sum(1 for obs, _ in observer.texts if obs.kind == "ui")
    http = len(observer.texts) - ui
    out: dict[str, Any] = {
        "scanned_texts": len(observer.texts),
        "ui_texts_scanned": ui,
        "http_texts_scanned": http,
        "hits": hits,
    }
    if hits:
        out["verdict"] = "red"
    elif ui == 0:
        out["verdict"] = "unknown"
        out["reason"] = "no rendered UI text was scanned (no 2xx page summary)"
    else:
        out["verdict"] = "clean"
    return out


def build_block(
    scenario: Scenario,
    steps: list[StepResult],
    *,
    artifact_version: str,
    platform: str,
    harness_sha: str,
    vocabulary_source: str,
    ran_at: str,
    next_action_kinds_source: str = "pinned",
) -> dict:
    step_json = [s.to_json() for s in steps]
    block = {
        "schema": artifact.BLOCK_SCHEMA,
        "artifact_version": artifact_version,
        "platform": platform,
        "ran_at": ran_at,
        "harness_sha": harness_sha,
        "coord": "unpaired",
        "vocabulary": {
            "source": vocabulary_source,
            "version": scenario.vocab.version if scenario.vocab else None,
        },
        "next_action_kinds": {
            "source": next_action_kinds_source,
            "kinds": sorted(scenario.next_action_kinds),
            "drift_from_pinned": sorted(scenario.next_action_kinds ^ NEXT_ACTION_KINDS),
        },
        "preflight": scenario.preflight,
        "fixture": {"name": scenario.config.fixture_path.name},
        "steps": step_json,
        "summary": artifact.summarize(step_json),
        "fleet_nouns": fleet_noun_report(
            scenario.runner.observer, scenario.vocab, scenario.vocab_error
        ),
    }
    artifact.validate_block(block)
    return block


# ---------------------------------------------------------------------------
# CLI (the Windows job).
# ---------------------------------------------------------------------------
class _LaunchedProcess:
    def __init__(self, exe: str, cwd: Path, log_dir: Path):
        if __package__ in (None, ""):
            from clean_room import launch as _launch
        else:
            from . import launch as _launch
        self._launch = _launch
        self._l = _launch.launch(exe, cwd, log_dir)

    def alive(self) -> bool:
        return self._l.process.poll() is None

    def exit_code(self) -> int | None:
        return self._l.process.poll()

    def stop(self) -> int | None:
        return self._launch.stop(self._l)


def _box_redactions(observer: Observer, fixture: Path, exe: str | None) -> None:
    candidates = [
        (str(fixture), "<fixture>"),
        (str(fixture.parent), "<fixture-parent>"),
        (str(Path.home()), "<home>"),
        (tempfile.gettempdir(), "<temp>"),
    ]
    for var in (
        "LOCALAPPDATA",
        "APPDATA",
        "USERPROFILE",
        "RUNNER_TEMP",
        "RUNNER_TOOL_CACHE",
        "ProgramFiles",
    ):
        if os.environ.get(var):
            candidates.append((os.environ[var], f"<{var}>"))
    if exe:
        candidates.append((str(Path(exe).parent), "<install-dir>"))
    for lit, ph in candidates:
        observer.add_redaction(lit, ph)


def _read_json(p: str, what: str) -> tuple[dict, str | None]:
    """The parsed file, or ({}, why-unreadable). Never a guessed default."""
    try:
        doc = json.loads(Path(p).read_text(encoding="utf-8-sig"))
    except (OSError, ValueError) as exc:
        return {}, f"{what} unreadable ({p}): {exc}"
    if not isinstance(doc, dict):
        return {}, f"{what} is not a JSON object ({p})"
    return doc, None


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="Run the clean-room scenario against the installed runner."
    )
    ap.add_argument("--install", required=True, help="install.json: {exe, error}")
    ap.add_argument("--preflight", required=True)
    ap.add_argument(
        "--fixture", required=True, help="fixture.json written by fixture.py"
    )
    ap.add_argument("--vocabulary")
    ap.add_argument("--vocabulary-source", default="unknown")
    ap.add_argument(
        "--refusal-rs",
        help="qontinui-schemas rust/src/refusal.rs at the vocabulary's commit; its "
        "NextActionKind set replaces the pinned one for this run (drift is recorded)",
    )
    ap.add_argument("--artifact-version", required=True)
    ap.add_argument("--platform", required=True)
    ap.add_argument("--harness-sha", required=True)
    ap.add_argument("--base", default="http://127.0.0.1:9876")
    ap.add_argument("--out", required=True)
    args = ap.parse_args(argv)

    out = Path(args.out)
    report_dir = out.parent
    evidence_dir = report_dir / "evidence"

    install, install_err = _read_json(args.install, "install")
    if install_err:
        install = {"exe": None, "unreadable": install_err}
    install.setdefault("exe", None)
    preflight_doc, pf_err = _read_json(args.preflight, "preflight")
    if pf_err:
        preflight_doc = {
            "verdict": "incomplete",
            "findings": [],
            "probe_errors": [{"probe": "preflight", "error": pf_err}],
        }
    fixture_doc, _ = _read_json(args.fixture, "fixture")
    # An unreadable fixture.json leaves the path EMPTY-and-absent (never ".",
    # which would aim the run at the harness checkout); the preflight step then
    # refuses to launch anything.
    fixture_raw = fixture_doc.get("path") or ""
    fixture_path = (
        Path(fixture_raw) if fixture_raw else report_dir / "no-fixture-path-given"
    )

    kinds, kinds_source = NEXT_ACTION_KINDS, "pinned"
    if args.refusal_rs:
        try:
            kinds = parse_next_action_kinds(
                Path(args.refusal_rs).read_text(encoding="utf-8")
            )
            kinds_source = f"parsed:{args.refusal_rs}"
        except (OSError, ValueError) as exc:
            kinds_source = f"pinned (refusal.rs unreadable: {exc})"

    observer = Observer(evidence_dir=evidence_dir)
    _box_redactions(observer, fixture_path, install.get("exe"))
    runner = Runner(base=args.base, observer=observer, transport=urllib_transport)
    config = Config(
        fixture_path=fixture_path,
        vocabulary_path=args.vocabulary,
        evidence_dir=evidence_dir,
    )
    scenario = Scenario(
        config=config,
        runner=runner,
        preflight=preflight_doc,
        install=install,
        start_process=lambda exe: _LaunchedProcess(
            exe, fixture_path.parent, report_dir / "runner-logs"
        ),
        next_action_kinds=kinds,
    )
    ran_at = _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    steps = scenario.run()
    block = build_block(
        scenario,
        steps,
        artifact_version=args.artifact_version or "unknown",
        platform=args.platform,
        harness_sha=args.harness_sha,
        vocabulary_source=args.vocabulary_source,
        ran_at=ran_at,
        next_action_kinds_source=kinds_source,
    )
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(block, indent=2) + "\n", encoding="utf-8")
    for s in steps:
        print(f"{s.outcome.upper():8} {s.step:24} {s.reason}")
    fn = block["fleet_nouns"]
    print(
        f"fleet nouns: {fn['verdict']} ({len(fn['hits'])} hit(s); "
        f"{fn['ui_texts_scanned']} UI text(s), {fn['http_texts_scanned']} served response(s))"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
