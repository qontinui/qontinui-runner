#!/usr/bin/env python3
"""Offline tests for the clean-room acceptance run (scripts/clean_room/).

Plan 2026-09-20-the-published-product-works-without-knowing-a-development-
environment-exists, Phase E. The real run needs a GitHub-hosted Windows box, a
published installer and a live runner; everything that DECIDES an outcome is a
pure function over injected probes and an injected HTTP transport, so the
acceptance proofs the plan names are exercised here with no network and no
runner:

  * a planted `D:/qontinui-root` in a rendered string reds the dynamic
    fleet-noun list;
  * a runner whose UI Bridge never completes a round-trip yields
    unknown(ui_bridge_unreachable) -- not pass, not fail -- for every UI step;
  * a sibling directory present yields unknown(box_not_clean) for every step;
  * the `unknown` reason set is closed and enumerated.

The vocabulary is qontinui-schemas `fleet-nouns.toml`, read from the sibling
checkout (../qontinui-schemas relative to this repo) or from
$CLEAN_ROOM_VOCABULARY. It is never copied here: an absent vocabulary FAILS
this test with the path probed, it does not skip.

Run:  python3 scripts/tests/test_clean_room.py
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPTS = HERE.parent
REPO = SCRIPTS.parent
sys.path.insert(0, str(SCRIPTS))

from clean_room import artifact, fixture, preflight, scenario
from clean_room.fleet_nouns import VocabularyUnavailable, load_vocabulary
from clean_room.outcome import (
    UNKNOWN_REASONS,
    OutcomeError,
    StepResult,
    unknown,
)
from clean_room.transport import Observer, Runner, TransportError


def _vocabulary_path() -> Path:
    env = os.environ.get("CLEAN_ROOM_VOCABULARY")
    candidates = [Path(env)] if env else []
    candidates.append(REPO.parent / "qontinui-schemas" / "fleet-nouns.toml")
    for c in candidates:
        if c.is_file():
            return c
    raise AssertionError(
        "fleet-nouns.toml not found; probed: "
        + ", ".join(str(c) for c in candidates)
        + ". Set CLEAN_ROOM_VOCABULARY or check out qontinui-schemas beside this repo. "
        "An absent vocabulary is UNKNOWN, never a skipped test."
    )


VOCAB_PATH = _vocabulary_path()
VOCAB = load_vocabulary(VOCAB_PATH)


class VocabularyTests(unittest.TestCase):
    def test_planted_operator_root_hits(self):
        classes = {
            h.class_id
            for h in VOCAB.scan_text('{"root": "D:/qontinui-root/qontinui-runner"}')
        }
        self.assertIn("machine_path", classes)
        self.assertIn("repo_layout", classes)

    def test_product_constants_do_not_hit(self):
        for text in (
            "http://127.0.0.1:9876/health",
            "~/.qontinui/",
            "https://coord.qontinui.io/x",
        ):
            self.assertEqual(VOCAB.scan_text(text), [], text)

    def test_exclude_is_span_scoped(self):
        # `C:\Users\Public` is excluded; the other path on the same line still hits.
        hits = VOCAB.scan_text("C:\\Users\\Public\\x and C:\\Users\\josh\\y")
        self.assertEqual([h.class_id for h in hits], ["machine_path"])

    def test_missing_vocabulary_is_an_error_not_empty(self):
        with self.assertRaises(VocabularyUnavailable):
            load_vocabulary(
                Path(tempfile.gettempdir()) / "definitely-absent-fleet-nouns.toml"
            )

    def test_ports_come_from_the_vocabulary(self):
        ports = preflight.vocabulary_ports(VOCAB)
        for p in (8000, 3001, 5432, 9875):
            self.assertIn(p, ports)
        self.assertNotIn(9876, ports)  # the product's own port is a product constant


class OutcomeTests(unittest.TestCase):
    def test_unknown_reason_set_is_closed(self):
        # The enumerated set. Adding a reason means adding it here, on purpose.
        self.assertEqual(
            set(UNKNOWN_REASONS),
            {
                "box_not_clean",
                "preflight_incomplete",
                "vocabulary_unavailable",
                "ui_bridge_unreachable",
                "prior_step_not_passed",
                "ui_element_not_found",
                "feature_not_released",
                "refusal_not_observed",
                "tenant_scope_not_refused",
                "transport_error",
                "response_unparseable",
                "harness_error",
            },
        )
        for reason, why in UNKNOWN_REASONS.items():
            self.assertTrue(why.strip(), reason)

    def test_unenumerated_unknown_reason_is_refused(self):
        with self.assertRaises(OutcomeError):
            unknown("x", "it_was_cloudy")

    def test_outcome_is_three_valued(self):
        with self.assertRaises(OutcomeError):
            StepResult("x", "skipped", "r", None)
        with self.assertRaises(OutcomeError):
            StepResult("x", "fail", "", None)


class PreflightTests(unittest.TestCase):
    def _assess(self, dirs=None, listening=(), env=None, errors=None, acknowledge=None):
        dirs = dirs or {}
        errors = errors or {}

        def list_dirs(root):
            if root in errors:
                raise errors[root]
            if root not in dirs:
                raise FileNotFoundError(root)
            return dirs[root]

        def probe_port(host, port):
            return (host, port) in listening

        return preflight.assess(
            VOCAB,
            list(dirs) + list(errors),
            env or {},
            list_dirs,
            probe_port,
            acknowledge=acknowledge,
        )

    def test_acknowledged_harness_checkout_is_recorded_not_dirty(self):
        r = self._assess(
            dirs={
                "/d/a": ["qontinui-runner"],
                "/d/a/qontinui-runner": ["qontinui-runner"],
            },
            acknowledge={
                "/d/a/qontinui-runner": "holder",
                "/d/a/qontinui-runner/qontinui-runner": "the harness checkout",
            },
        )
        self.assertEqual(r.verdict, "clean", r.findings)
        self.assertEqual(len(r.to_json()["acknowledged"]), 2)

    def test_acknowledgement_is_by_exact_path_not_by_name(self):
        r = self._assess(
            dirs={"/d/a": ["qontinui-runner", "qontinui-schemas"]},
            acknowledge={"/d/a/qontinui-runner": "the harness checkout"},
        )
        self.assertEqual(r.verdict, "not_clean")
        self.assertEqual(
            [f["path"] for f in r.findings], [str(Path("/d/a/qontinui-schemas"))]
        )

    def test_harness_checkout_cli_probes_its_parents(self):
        with tempfile.TemporaryDirectory() as d:
            holder = Path(d) / "qontinui-runner"
            checkout = holder / "qontinui-runner"
            checkout.mkdir(parents=True)
            out = Path(d) / "pf.json"
            preflight.main(
                [
                    "--vocabulary",
                    str(VOCAB_PATH),
                    "--harness-checkout",
                    str(checkout),
                    "--out",
                    str(out),
                ]
            )
            doc = json.loads(out.read_text())
            self.assertIn(str(holder.resolve().parent), doc["probed"]["roots"])
            self.assertEqual(
                {a["path"] for a in doc["acknowledged"]},
                {str(checkout.resolve()), str(holder.resolve())},
            )
            (Path(d) / "ui-bridge").mkdir()
            preflight.main(
                [
                    "--vocabulary",
                    str(VOCAB_PATH),
                    "--harness-checkout",
                    str(checkout),
                    "--out",
                    str(out),
                ]
            )
            doc = json.loads(out.read_text())
            self.assertIn(
                str((Path(d) / "ui-bridge").resolve()),
                [
                    str(Path(f["path"]).resolve())
                    for f in doc["findings"]
                    if "path" in f
                ],
            )

    def test_clean_box(self):
        r = self._assess(
            dirs={"/w": ["tally-service", "Program Files", "a"]}, env={"PATH": "x"}
        )
        self.assertEqual(r.verdict, "clean", r.findings)

    def test_sibling_directory_is_not_clean(self):
        r = self._assess(dirs={"/w": ["tally-service", "qontinui-schemas"]})
        self.assertEqual(r.verdict, "not_clean")
        self.assertEqual(r.findings[0]["kind"], "sibling_checkout")

    def test_workspace_root_directory_is_not_clean(self):
        r = self._assess(dirs={"D:\\": ["a", "qontinui-root"]})
        self.assertEqual(r.verdict, "not_clean")

    def test_dev_listener_is_not_clean(self):
        r = self._assess(dirs={"/w": []}, listening={("127.0.0.1", 8000)})
        self.assertEqual(r.verdict, "not_clean")
        self.assertEqual(
            r.findings[0], {"kind": "dev_listener", "host": "127.0.0.1", "port": 8000}
        )

    def test_supervisor_listener_is_not_clean(self):
        r = self._assess(dirs={"/w": []}, listening={("::1", 9875)})
        self.assertEqual(r.verdict, "not_clean")

    def test_runner_already_on_product_port_is_not_clean(self):
        r = self._assess(dirs={"/w": []}, listening={("127.0.0.1", 9876)})
        self.assertEqual(r.findings[0]["kind"], "product_port_occupied")

    def test_qontinui_env_is_not_clean_and_value_is_not_recorded(self):
        r = self._assess(dirs={"/w": []}, env={"QONTINUI_ROOT": "secret-ish"})
        self.assertEqual(r.verdict, "not_clean")
        self.assertNotIn("secret-ish", json.dumps(r.to_json()))

    def test_unreadable_root_is_incomplete_not_clean(self):
        r = self._assess(dirs={"/w": []}, errors={"/locked": PermissionError("denied")})
        self.assertEqual(r.verdict, "incomplete")

    def test_port_probe_error_is_incomplete(self):
        def boom(host, port):
            raise TimeoutError("timed out")

        r = preflight.assess(VOCAB, [], {}, lambda root: [], boom)
        self.assertEqual(r.verdict, "incomplete")

    def test_real_probe_refused_port_reads_not_listening(self):
        import socket

        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
        s.close()
        self.assertFalse(preflight.real_probe_port("127.0.0.1", port, timeout=10))
        srv = socket.socket()
        srv.bind(("127.0.0.1", 0))
        srv.listen(1)
        try:
            self.assertTrue(
                preflight.real_probe_port("127.0.0.1", srv.getsockname()[1], timeout=10)
            )
        finally:
            srv.close()


class FixtureTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="clean-room-fixture-"))

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_generates_a_committed_foreign_repo(self):
        repo = fixture.generate(self.tmp, str(VOCAB_PATH))
        self.assertTrue((repo / "go.mod").is_file())
        for marker in fixture.QONTINUI_STACK_MARKERS:
            self.assertFalse((repo / marker).exists(), marker)
        for qdir in (".qontinui", ".claude", "plans"):
            self.assertFalse((repo / qdir).exists(), qdir)
        head = subprocess.run(
            ["git", "-C", str(repo), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
        )
        self.assertRegex(head.stdout.strip(), r"^[0-9a-f]{40}$")
        status = subprocess.run(
            ["git", "-C", str(repo), "status", "--porcelain"],
            capture_output=True,
            text=True,
            check=True,
        )
        self.assertEqual(status.stdout, "")

    def test_fixture_is_deterministic(self):
        a = fixture.generate(self.tmp / "a", str(VOCAB_PATH))
        b = fixture.generate(self.tmp / "b", str(VOCAB_PATH))
        ha = subprocess.run(
            ["git", "-C", str(a), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
            timeout=60,
        ).stdout
        hb = subprocess.run(
            ["git", "-C", str(b), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
            timeout=60,
        ).stdout
        self.assertEqual(ha, hb)

    def test_fixture_name_passes_the_preflight_sibling_check(self):
        self.assertFalse(VOCAB.hits_class("repo_layout", f"../{fixture.FIXTURE_NAME}"))

    def test_refuses_to_reuse_an_existing_directory(self):
        fixture.generate(self.tmp, str(VOCAB_PATH))
        with self.assertRaises(fixture.FixtureError):
            fixture.generate(self.tmp, str(VOCAB_PATH))

    def test_the_test_really_fails(self):
        go = shutil.which("go")
        if go is None:
            # Stated, not silent: windows-latest and ubuntu-latest both ship Go,
            # so the CI runs of this file execute the fixture's test for real.
            self.skipTest(
                "go toolchain not on PATH on this box; failing-test property checked by content below"
            )
        repo = fixture.generate(self.tmp, str(VOCAB_PATH))
        r = subprocess.run(
            [go, "test", "./..."],
            cwd=repo,
            capture_output=True,
            text=True,
            check=False,
            timeout=300,
        )
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("TestSum", r.stdout + r.stderr)

    def test_the_bug_is_in_the_source(self):
        self.assertIn("len(values)-1", fixture.FILES["tally.go"])
        self.assertIn("want 6", fixture.FILES["tally_test.go"])


class ArtifactTests(unittest.TestCase):
    def _block(self, **over):
        b = {
            "schema": artifact.BLOCK_SCHEMA,
            "artifact_version": "v1.0.12",
            "platform": "windows-x64",
            "ran_at": "2026-09-30T07:00:00Z",
            "harness_sha": "a" * 40,
            "preflight": {"verdict": "clean"},
            "steps": [unknown("glossary", "feature_not_released").to_json()],
            "fleet_nouns": {"verdict": "clean", "scanned_texts": 3, "hits": []},
        }
        b["summary"] = artifact.summarize(b["steps"])
        b.update(over)
        return b

    def test_join_adds_block_to_existing_parity_report(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "published-parity.json"
            p.write_text('\ufeff{"parity_defects": 2}', encoding="utf-8")
            doc = artifact.join(p, self._block(), p)
            self.assertEqual(doc["parity_defects"], 2)
            self.assertEqual(
                json.loads(p.read_text())["clean_room"]["artifact_version"], "v1.0.12"
            )

    def test_join_states_a_missing_parity_report(self):
        with tempfile.TemporaryDirectory() as d:
            doc = artifact.join(
                Path(d) / "absent.json", self._block(), Path(d) / "out.json"
            )
            self.assertIsNone(doc["parity_report"])
            self.assertIn("UNKNOWN", doc["parity_report_unavailable"])

    def test_join_with_an_unobtainable_parity_report_says_why(self):
        with tempfile.TemporaryDirectory() as d:
            doc = artifact.join(
                None, self._block(), Path(d) / "out.json", "the download failed"
            )
            self.assertIn("the download failed", doc["parity_report_unavailable"])
            self.assertNotIn("not produced", doc["parity_report_unavailable"])

    def test_stub_block_states_a_missing_clean_room_half(self):
        b = artifact.stub_block(
            "the clean-room job produced no block (job result: failure)",
            scenario.STEPS,
            artifact_version="v1",
            platform="windows-x64",
            harness_sha="h",
            ran_at="t",
        )
        self.assertEqual({s["outcome"] for s in b["steps"]}, {"unknown"})
        self.assertEqual(b["fleet_nouns"]["verdict"], "unknown")
        self.assertFalse(b["summary"]["all_pass"])
        with tempfile.TemporaryDirectory() as d:
            rc = artifact.main(
                [
                    "--stub-reason",
                    "no block",
                    "--parity-unavailable-reason",
                    "no tag",
                    "--out",
                    str(Path(d) / "j.json"),
                ]
            )
            self.assertEqual(rc, 0)
            self.assertEqual(
                json.loads((Path(d) / "j.json").read_text())["clean_room"]["stub"],
                "no block",
            )

    def test_block_requires_provenance(self):
        for k in artifact.REQUIRED_KEYS:
            with self.assertRaises(artifact.ArtifactError, msg=k):
                artifact.validate_block(self._block(**{k: ""}))

    def test_summary_never_all_pass_with_an_unknown(self):
        self.assertFalse(
            artifact.summarize([unknown("x", "harness_error").to_json()])["all_pass"]
        )
        self.assertFalse(artifact.summarize([])["all_pass"])


def _refusal_rs_path() -> Path:
    env = os.environ.get("CLEAN_ROOM_REFUSAL_RS")
    candidates = [Path(env)] if env else []
    candidates.append(REPO.parent / "qontinui-schemas" / "rust" / "src" / "refusal.rs")
    for c in candidates:
        if c.is_file():
            return c
    raise AssertionError(
        "qontinui-schemas rust/src/refusal.rs not found; probed: "
        + ", ".join(map(str, candidates))
    )


class NextActionKindTests(unittest.TestCase):
    @unittest.skipIf(
        os.environ.get("CLEAN_ROOM_NIGHTLY") == "1",
        "the nightly parses refusal.rs at run time and records drift in the block; "
        "the pin is hard in clean-room-selftest.yml at PR time",
    )
    def test_pinned_kinds_match_the_schemas_enum(self):
        parsed = scenario.parse_next_action_kinds(
            _refusal_rs_path().read_text(encoding="utf-8")
        )
        self.assertEqual(parsed, scenario.NEXT_ACTION_KINDS)

    def test_parser_drops_the_reader_side_variant(self):
        src = "pub enum NextActionKind {\n    RetryLater,\n    /// doc\n    PairDevice,\n    #[serde(other)]\n    Unrecognised,\n}\n"
        self.assertEqual(
            scenario.parse_next_action_kinds(src), {"retry_later", "pair_device"}
        )

    def test_parser_refuses_an_empty_enum(self):
        with self.assertRaises(ValueError):
            scenario.parse_next_action_kinds("pub enum NextActionKind {\n}\n")


# ---------------------------------------------------------------------------
# A fake runner: answers the scenario's routes from state, no socket. Routes
# are keyed by (METHOD, path) like the runner's own route table, so a request
# with the wrong method gets the runner's 405 -- the drift that made the first
# revision of this harness read no rendered UI text at all.
# ---------------------------------------------------------------------------
class FakeProcess:
    def __init__(self, exits_at_launch=False):
        self._alive = not exits_at_launch
        self.stopped = False

    def alive(self):
        return self._alive

    def exit_code(self):
        return None if self._alive else 0

    def stop(self):
        self._alive = False
        self.stopped = True
        return -15


class FakeRunner:
    """Today's published runner, unless a knob says otherwise."""

    def __init__(
        self,
        fixture_path,
        *,
        ui_bridge=True,
        provision=True,
        page_text="Projects Terminal Settings",
        refusal=None,
        refusal_status=400,
        glossary=None,
        stop_reason=None,
        tenant_ignored=True,
        stop_reason_status=200,
        delete_status=200,
        project_id="p1",
        exits_on_close=True,
        occupied_before_launch=False,
    ):
        self.fixture = fixture_path
        self.ui_bridge = ui_bridge
        self.provision = provision
        self.page_text = page_text
        self.refusal = refusal
        self.refusal_status = refusal_status
        self.glossary = glossary
        self.stop_reason = stop_reason
        # Today's release (v1.0.11) has no `tenantId` on CreateTerminalRequest,
        # so it ignores the field and creates the session.
        self.tenant_ignored = tenant_ignored
        self.stop_reason_status = stop_reason_status
        self.delete_status = delete_status
        self.project_id = project_id
        self.exits_on_close = exits_on_close
        self.occupied_before_launch = occupied_before_launch
        self.process = None  # set by the test's start_process
        self.projects = []
        self.terminals = []
        self.calls = []

    @staticmethod
    def ok(data):
        return 200, json.dumps({"success": True, "data": data})

    def _routes(self):
        return {
            ("GET", "/health"): lambda p: self.ok(
                {"responsive": True, "uiBridgeIpcObserved": self.ui_bridge}
            ),
            ("GET", "/ui-bridge/control/elements"): lambda p: self.ok(
                {"elements": [{"id": "nav:projects", "label": "Projects"}], "count": 1}
            ),
            ("POST", "/ui-bridge/control/page/summary"): lambda p: self.ok(
                {"title": "Qontinui Runner", "text": self.page_text}
            ),
            ("POST", "/ui-bridge/invoke/add_saved_project"): self._add_project,
            ("POST", "/ui-bridge/invoke/list_saved_projects"): lambda p: self.ok(
                self.projects
            ),
            ("POST", "/ui-bridge/control/tab/activate"): lambda p: self.ok(
                {"activeTab": p["tabId"]}
            ),
            ("GET", "/ui-bridge/control/components"): lambda p: self.ok(
                {
                    "components": [
                        {"id": f"projects.card-{x['id']}"} for x in self.projects
                    ]
                }
            ),
            (
                "POST",
                f"/ui-bridge/control/component/projects.card-{self.project_id}/action/work-on-it",
            ): self._work_on_it,
            ("GET", "/terminals"): lambda p: self.ok({"terminals": self.terminals}),
            ("POST", "/terminals"): self._create_terminal,
            ("GET", "/settings/paths"): lambda p: self.ok(
                {"resolved": {"plan_tier_active": False, "plan_scan_roots": []}}
            ),
            ("GET", "/ui-bridge/control/tabs"): lambda p: self.ok(
                {
                    "tabs": [
                        {"id": "projects"},
                        {"id": "terminal"},
                        {"id": "settings-paths"},
                    ]
                }
            ),
            ("POST", "/ui-bridge/control/page/close-request"): self._close,
        }

    def _add_project(self, payload):
        proj = dict(payload["args"]["project"], id=self.project_id)
        self.projects.append(proj)
        return self.ok(None)

    def _work_on_it(self, payload):
        # The real runner provisions `.claude/commands/` on every terminal
        # spawn (v1.0.11+); `provision=False` models the v1.0.10 regression.
        self.terminals.append(
            {"id": "t1", "workingDir": str(self.fixture), "isAlive": True}
        )
        if self.provision:
            d = self.fixture / ".claude" / "commands"
            d.mkdir(parents=True, exist_ok=True)
            (d / "whereami.md").write_text("x", encoding="utf-8")
        return self.ok({"ok": True})

    def _create_terminal(self, payload):
        if self.refusal is not None:
            return self.refusal_status, json.dumps(self.refusal)
        if self.tenant_ignored:
            return self.ok({"id": "t9"})
        return 400, json.dumps(
            {
                "success": False,
                "error": "terminal:tenant_not_paired: this runner holds no coord credential for tenant x",
            }
        )

    def _close(self, payload):
        if self.exits_on_close and self.process is not None:
            self.process._alive = False
        return self.ok({"success": True})

    def __call__(self, method, url, body, timeout):
        path = url.split("127.0.0.1:9876", 1)[1]
        payload = json.loads(body) if body else None
        self.calls.append((method, path))
        running = self.process is not None and self.process.alive()
        if not running and not (self.occupied_before_launch and self.process is None):
            raise TransportError(f"{method} {url}: connection refused")
        if (
            path.startswith("/ui-bridge/")
            and path != "/ui-bridge/control/page/close-request"
            and not self.ui_bridge
        ):
            return 503, json.dumps(
                {"success": False, "error": "frontend did not respond"}
            )
        base = path.split("?", 1)[0]
        routes = self._routes()
        if (method, base) in routes:
            return routes[(method, base)](payload)
        if any(p == base for (_, p) in routes):
            return 405, ""
        if method == "DELETE" and base.startswith("/terminals/"):
            if self.delete_status != 200:
                return self.delete_status, json.dumps(
                    {"success": False, "error": "terminal busy", "code": "CONFLICT"}
                )
            for t in self.terminals:
                t["isAlive"] = False
            return self.ok({"closed": True})
        if method == "GET" and base == "/glossary" and self.glossary is not None:
            return self.ok(self.glossary)
        if (
            method == "GET"
            and base.endswith("/stop-reason")
            and self.stop_reason is not None
        ):
            if self.stop_reason_status != 200:
                return self.stop_reason_status, json.dumps(self.stop_reason)
            return self.ok(self.stop_reason)
        # The runner's route fallback (`not_found_handler`), verbatim shape.
        return 404, json.dumps(
            {
                "success": False,
                "error": f"No route for {method} {path}",
                "code": "NOT_FOUND",
            }
        )


class ScenarioTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="clean-room-scenario-"))
        self.fixture = self.tmp / "foreign" / "tally-service"
        self.fixture.mkdir(parents=True)
        self.launched = []

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _scenario(
        self,
        fake=None,
        preflight_doc=None,
        install=None,
        exits_at_launch=False,
        home=None,
        fixture=None,
        vocabulary=None,
    ):
        fake = fake or FakeRunner(self.fixture)
        fixture = fixture or self.fixture
        observer = Observer(evidence_dir=self.tmp / "report" / "evidence")
        # The same redaction set the CLI applies: the box's own paths.
        scenario._box_redactions(observer, fixture, (install or {}).get("exe"))
        if home:
            observer.add_redaction(home, "<home>")
        runner = Runner(base="http://127.0.0.1:9876", observer=observer, transport=fake)
        t = [0.0]
        clock = scenario.Clock(
            now=lambda: t[0], sleep=lambda s: t.__setitem__(0, t[0] + s)
        )

        def start(exe):
            p = FakeProcess(exits_at_launch)
            fake.process = p
            self.launched.append(p)
            return p

        return scenario.Scenario(
            config=scenario.Config(
                fixture_path=fixture,
                vocabulary_path=vocabulary or str(VOCAB_PATH),
                evidence_dir=observer.evidence_dir,
            ),
            runner=runner,
            preflight=preflight_doc or {"verdict": "clean", "findings": []},
            install=install or {"exe": "C:/x/Qontinui Runner/qontinui-runner.exe"},
            start_process=start,
            clock=clock,
        )

    def _run(self, fake=None, **kw):
        sc = self._scenario(fake, **kw)
        steps = sc.run()
        block = scenario.build_block(
            sc,
            steps,
            artifact_version="v1.0.12",
            platform="windows-x64",
            harness_sha="b" * 40,
            vocabulary_source="test",
            ran_at="2026-09-30T00:00:00Z",
        )
        return {s.step: s for s in steps}, block

    def test_every_step_is_recorded_in_order(self):
        _, block = self._run()
        self.assertEqual([s["step"] for s in block["steps"]], list(scenario.STEPS))
        for s in block["steps"]:
            self.assertEqual(
                set(s), {"step", "outcome", "reason", "evidence_ref", "detail"}
            )

    def test_todays_release_shape(self):
        steps, block = self._run()
        got = {
            k: (v.outcome, v.reason if v.outcome == "unknown" else "")
            for k, v in steps.items()
        }
        for s in (
            "preflight",
            "install",
            "first_launch",
            "ui_bridge_ready",
            "open_repo",
            "start_session",
            "session_lists_commands",
            "close",
        ):
            self.assertEqual(got[s][0], "pass", (s, steps[s]))
        # No known plan-authoring control: not measured, so not a fail.
        self.assertEqual(got["author_plan"], ("unknown", "ui_element_not_found"))
        self.assertIn('"projects"', steps["author_plan"].detail)
        # Today's release ignores tenantId and creates the session.
        self.assertEqual(
            got["unpaired_refusal"], ("unknown", "tenant_scope_not_refused")
        )
        # Unreleased runner features are NEVER pass.
        self.assertEqual(got["glossary"], ("unknown", "feature_not_released"))
        self.assertEqual(got["stop_reason"], ("unknown", "feature_not_released"))
        self.assertIn("forced: false", steps["close"].detail)
        fn = block["fleet_nouns"]
        self.assertEqual(fn["verdict"], "clean")
        self.assertGreater(fn["ui_texts_scanned"], 0)
        self.assertFalse(block["summary"]["all_pass"])

    def test_page_summary_is_read_with_post(self):
        fake = FakeRunner(self.fixture)
        self._run(fake)
        self.assertIn(("POST", "/ui-bridge/control/page/summary"), fake.calls)
        self.assertNotIn(("GET", "/ui-bridge/control/page/summary"), fake.calls)

    def test_planted_operator_root_reds_the_dynamic_list(self):
        fake = FakeRunner(
            self.fixture, page_text="Workspace: D:/qontinui-root (from settings)"
        )
        _, block = self._run(fake)
        fn = block["fleet_nouns"]
        self.assertEqual(fn["verdict"], "red")
        self.assertIn("machine_path", {h["class_id"] for h in fn["hits"]})
        self.assertTrue(
            all(h["evidence_ref"].startswith("evidence/") for h in fn["hits"])
        )
        self.assertTrue(
            any(h["kind"] == "ui" and "page/summary" in h["source"] for h in fn["hits"])
        )

    def test_the_boxs_own_home_is_redacted_but_a_plant_is_not(self):
        home = "C:\\Users\\runneradmin"
        fake = FakeRunner(
            self.fixture,
            page_text="config at C:\\\\Users\\\\runneradmin\\\\AppData\\\\Roaming",
        )
        _, block = self._run(fake, home=home)
        self.assertEqual(
            block["fleet_nouns"]["verdict"], "clean", block["fleet_nouns"]["hits"]
        )
        fake = FakeRunner(
            self.fixture,
            page_text="config at C:\\\\Users\\\\runneradmin and D:/qontinui-root",
        )
        _, block = self._run(fake, home=home)
        self.assertEqual(block["fleet_nouns"]["verdict"], "red")

    def test_no_rendered_ui_text_means_the_list_is_unknown_not_clean(self):
        _, block = self._run(FakeRunner(self.fixture, ui_bridge=False))
        fn = block["fleet_nouns"]
        self.assertEqual(fn["ui_texts_scanned"], 0)
        self.assertGreater(fn["http_texts_scanned"], 0)
        self.assertEqual(fn["verdict"], "unknown")

    def test_missing_ui_bridge_is_unknown_ui_bridge_unreachable(self):
        steps, _ = self._run(FakeRunner(self.fixture, ui_bridge=False))
        self.assertEqual(
            (steps["ui_bridge_ready"].outcome, steps["ui_bridge_ready"].reason),
            ("unknown", "ui_bridge_unreachable"),
        )
        for s in (
            "open_repo",
            "start_session",
            "session_lists_commands",
            "author_plan",
            "stop_reason",
        ):
            self.assertEqual(
                (steps[s].outcome, steps[s].reason),
                ("unknown", "ui_bridge_unreachable"),
                s,
            )
        self.assertNotIn(
            "pass",
            {
                steps[s].outcome
                for s in ("ui_bridge_ready", "open_repo", "start_session")
            },
        )
        self.assertEqual(steps["first_launch"].outcome, "pass")

    def test_dirty_box_is_unknown_box_not_clean_everywhere(self):
        steps, block = self._run(
            preflight_doc={
                "verdict": "not_clean",
                "findings": [
                    {"kind": "sibling_checkout", "path": "D:/a/qontinui-schemas"}
                ],
            }
        )
        for name, s in steps.items():
            self.assertEqual((s.outcome, s.reason), ("unknown", "box_not_clean"), name)
        self.assertEqual(self.launched, [], "a dirty box must not be measured at all")
        self.assertEqual(
            block["summary"]["unknown_reasons"], {"box_not_clean": len(scenario.STEPS)}
        )

    def test_sibling_directory_end_to_end_through_preflight(self):
        root = self.tmp / "probe-root"
        (root / "qontinui-schemas").mkdir(parents=True)
        pf = preflight.assess(
            VOCAB, [str(root)], {}, preflight.real_list_dirs, lambda h, p: False
        )
        steps, _ = self._run(preflight_doc=pf.to_json())
        self.assertTrue(all(s.reason == "box_not_clean" for s in steps.values()))

    def test_a_runner_already_on_the_port_is_box_not_clean(self):
        steps, _ = self._run(FakeRunner(self.fixture, occupied_before_launch=True))
        self.assertEqual(
            (steps["first_launch"].outcome, steps["first_launch"].reason),
            ("unknown", "box_not_clean"),
        )
        self.assertEqual(self.launched, [])
        self.assertEqual(steps["glossary"].reason, "box_not_clean")

    def test_missing_fixture_launches_nothing(self):
        steps, _ = self._run(fixture=self.tmp / "no-such-fixture")
        self.assertEqual(
            (steps["preflight"].outcome, steps["preflight"].reason),
            ("unknown", "harness_error"),
        )
        self.assertEqual(self.launched, [])
        self.assertTrue(all(s.outcome == "unknown" for s in steps.values()))

    def test_runner_that_dies_at_launch_fails_first_launch_only(self):
        steps, _ = self._run(exits_at_launch=True)
        self.assertEqual(steps["first_launch"].outcome, "fail")
        self.assertEqual(
            (steps["ui_bridge_ready"].outcome, steps["ui_bridge_ready"].reason),
            ("unknown", "prior_step_not_passed"),
        )
        self.assertEqual(steps["glossary"].reason, "prior_step_not_passed")

    def test_missing_install_fails_install(self):
        steps, _ = self._run(
            install={"exe": None, "error": "Could not locate the INSTALLED runner exe"}
        )
        self.assertEqual(steps["install"].outcome, "fail")
        self.assertEqual(steps["first_launch"].reason, "prior_step_not_passed")

    def test_unreadable_install_record_is_a_harness_error_not_a_product_fail(self):
        steps, _ = self._run(
            install={"exe": None, "unreadable": "install unreadable: no such file"}
        )
        self.assertEqual(
            (steps["install"].outcome, steps["install"].reason),
            ("unknown", "harness_error"),
        )

    def test_saved_project_without_an_id_fails_open_repo(self):
        steps, _ = self._run(FakeRunner(self.fixture, project_id=""))
        self.assertEqual(steps["open_repo"].outcome, "fail")
        self.assertIn("no id", steps["open_repo"].reason)

    def test_no_provisioned_commands_fails(self):
        steps, _ = self._run(FakeRunner(self.fixture, provision=False))
        self.assertEqual(steps["session_lists_commands"].outcome, "fail")

    def test_provisioned_commands_pass(self):
        steps, _ = self._run(FakeRunner(self.fixture, provision=True))
        self.assertEqual(steps["session_lists_commands"].outcome, "pass")

    def test_a_bare_pairing_refusal_without_an_envelope_fails(self):
        steps, _ = self._run(FakeRunner(self.fixture, tenant_ignored=False))
        self.assertEqual(steps["unpaired_refusal"].outcome, "fail")
        self.assertIn("no next_action", steps["unpaired_refusal"].reason)

    def test_a_message_naming_tenant_and_pairing_is_not_the_pairing_code(self):
        refusal = {
            "success": False,
            "code": "tenant_credential_store_unreadable",
            "error": "this runner holds a coord credential for tenant x but is unpaired",
            "next_action": {"kind": "report_defect"},
        }
        steps, _ = self._run(FakeRunner(self.fixture, refusal=refusal))
        self.assertEqual(steps["unpaired_refusal"].reason, "refusal_not_observed")

    def test_a_handler_404_from_stop_reason_fails_after_polling(self):
        fake = FakeRunner(
            self.fixture,
            stop_reason={"success": False, "error": "no session", "code": "SESSION_NOT_FOUND"},
            stop_reason_status=404,
        )
        steps, _ = self._run(fake)
        self.assertEqual(steps["stop_reason"].outcome, "fail")
        polls = [c for c in fake.calls if c[1].endswith("/stop-reason")]
        self.assertGreater(len(polls), 1)

    def test_a_failed_session_end_leaves_stop_reason_unknown(self):
        steps, _ = self._run(
            FakeRunner(self.fixture, stop_reason={"code": "x"}, delete_status=409)
        )
        self.assertEqual(
            (steps["stop_reason"].outcome, steps["stop_reason"].reason),
            ("unknown", "prior_step_not_passed"),
        )

    def test_structured_refusal_and_released_doors_pass(self):
        refusal = {
            "success": False,
            "code": "tenant_not_paired",
            "refusal": {
                "code": "unknown",
                "next_action": {"kind": "pair_device", "target": "Settings > Account"},
                "glossary_terms": ["tenant"],
                "observed_at": "2026-09-30T00:00:00Z",
                "source": "runner",
            },
        }
        glossary = {
            "version": 3,
            "source": "embedded",
            "build_id": "x",
            "terms": [{"id": "gate"}],
        }
        stop = {
            "code": "closed_by_user",
            "glossary_terms": [],
            "next_action": {"kind": "none_terminal"},
            "observed_at": "2026-09-30T00:00:00Z",
            "source": "runner",
        }
        steps, _ = self._run(
            FakeRunner(
                self.fixture, refusal=refusal, glossary=glossary, stop_reason=stop
            )
        )
        self.assertEqual(
            steps["unpaired_refusal"].outcome, "pass", steps["unpaired_refusal"]
        )
        self.assertEqual(steps["glossary"].outcome, "pass")
        self.assertEqual(steps["stop_reason"].outcome, "pass")

    def test_refusal_naming_a_fleet_noun_fails(self):
        refusal = {
            "success": False,
            "code": "tenant_not_paired",
            "refusal": {
                "next_action": {
                    "kind": "run_command",
                    "target": "cd ../qontinui-schemas",
                }
            },
        }
        steps, _ = self._run(FakeRunner(self.fixture, refusal=refusal))
        self.assertEqual(steps["unpaired_refusal"].outcome, "fail")
        self.assertIn("fleet nouns", steps["unpaired_refusal"].reason)

    def test_refusal_with_unknown_kind_fails(self):
        refusal = {
            "success": False,
            "code": "tenant_not_paired",
            "next_action": {"kind": "call_the_maintainer"},
        }
        steps, _ = self._run(FakeRunner(self.fixture, refusal=refusal))
        self.assertIn("not a NextActionKind", steps["unpaired_refusal"].reason)

    def test_a_drain_past_the_deadline_is_not_the_pairing_refusal(self):
        # Even a drain refusal that carries a perfect envelope is not the
        # refusal this step is about.
        refusal = {
            "success": False,
            "code": "drain_unreadable",
            "next_action": {"kind": "retry_later"},
        }
        steps, _ = self._run(
            FakeRunner(self.fixture, refusal=refusal, refusal_status=409)
        )
        self.assertEqual(
            (steps["unpaired_refusal"].outcome, steps["unpaired_refusal"].reason),
            ("unknown", "refusal_not_observed"),
        )

    def test_a_bad_request_is_not_the_pairing_refusal(self):
        refusal = {
            "success": False,
            "error": "workingDir is not an existing directory",
            "next_action": {"kind": "fix_request"},
        }
        steps, _ = self._run(FakeRunner(self.fixture, refusal=refusal))
        self.assertEqual(steps["unpaired_refusal"].reason, "refusal_not_observed")

    def test_a_created_tenant_session_is_not_called_paired(self):
        steps, _ = self._run(FakeRunner(self.fixture, tenant_ignored=True))
        self.assertEqual(
            (steps["unpaired_refusal"].outcome, steps["unpaired_refusal"].reason),
            ("unknown", "tenant_scope_not_refused"),
        )

    def test_glossary_without_terms_fails_not_passes(self):
        steps, _ = self._run(
            FakeRunner(
                self.fixture,
                glossary={
                    "version": 1,
                    "source": "embedded",
                    "build_id": "x",
                    "terms": [],
                },
            )
        )
        self.assertEqual(steps["glossary"].outcome, "fail")

    def test_a_runner_that_ignores_the_window_close_fails_close_forced(self):
        steps, _ = self._run(FakeRunner(self.fixture, exits_on_close=False))
        self.assertEqual(steps["close"].outcome, "fail")
        self.assertIn("forced: true", steps["close"].detail)
        self.assertTrue(self.launched[0].stopped)

    def test_transport_loss_mid_run_is_unknown(self):
        fake = FakeRunner(self.fixture)

        def flaky(method, url, body, timeout):
            if url.endswith("/glossary"):
                raise TransportError("connection reset")
            return fake(method, url, body, timeout)

        sc = self._scenario(fake)
        sc.runner.transport = flaky
        steps = {s.step: s for s in sc.run()}
        self.assertEqual(
            (steps["glossary"].outcome, steps["glossary"].reason),
            ("unknown", "transport_error"),
        )

    def test_absent_vocabulary_makes_the_dynamic_list_unknown(self):
        sc = self._scenario(
            FakeRunner(self.fixture, tenant_ignored=False),
            vocabulary=str(self.tmp / "nope.toml"),
        )
        steps = sc.run()
        block = scenario.build_block(
            sc,
            steps,
            artifact_version="v",
            platform="p",
            harness_sha="h",
            vocabulary_source="s",
            ran_at="t",
        )
        self.assertEqual(block["fleet_nouns"]["verdict"], "unknown")
        self.assertEqual(
            {s.step: s.reason for s in steps}["unpaired_refusal"],
            "vocabulary_unavailable",
        )

    def test_kind_drift_is_recorded(self):
        sc = self._scenario()
        sc.next_action_kinds = scenario.NEXT_ACTION_KINDS | {"brand_new_kind"}
        steps = sc.run()
        block = scenario.build_block(
            sc,
            steps,
            artifact_version="v",
            platform="p",
            harness_sha="h",
            vocabulary_source="s",
            ran_at="t",
            next_action_kinds_source="parsed:x",
        )
        self.assertEqual(
            block["next_action_kinds"]["drift_from_pinned"], ["brand_new_kind"]
        )


@unittest.skipIf(
    os.name == "nt",
    "the stand-in exe is a POSIX script; the Windows job runs the real installed exe",
)
class EndToEndCliTests(unittest.TestCase):
    """scenario.main() for real: launch a stand-in exe, talk HTTP, write the block, join it."""

    def _free_port(self):
        import socket

        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
        s.close()
        return port

    def _cli(self, page_text):
        tmp = Path(tempfile.mkdtemp(prefix="clean-room-e2e-"))
        self.addCleanup(shutil.rmtree, tmp, True)
        repo = fixture.generate(tmp / "foreign", str(VOCAB_PATH))
        port = self._free_port()
        exe = HERE / "fixtures" / "clean_room_fake_runner.py"
        (tmp / "install.json").write_text(json.dumps({"exe": str(exe), "error": None}))
        (tmp / "preflight.json").write_text(
            json.dumps({"verdict": "clean", "findings": []})
        )
        (tmp / "fixture.json").write_text(
            json.dumps({"path": str(repo), "name": repo.name})
        )
        env = {
            "CLEAN_ROOM_FAKE_PORT": str(port),
            "CLEAN_ROOM_FAKE_FIXTURE": str(repo),
            "CLEAN_ROOM_FAKE_PAGE_TEXT": page_text,
            "CLEAN_ROOM_FAKE_VOCABULARY": str(VOCAB_PATH),
        }
        saved = {k: os.environ.get(k) for k in env}
        os.environ.update(env)
        try:
            rc = scenario.main(
                [
                    "--install",
                    str(tmp / "install.json"),
                    "--preflight",
                    str(tmp / "preflight.json"),
                    "--fixture",
                    str(tmp / "fixture.json"),
                    "--vocabulary",
                    str(VOCAB_PATH),
                    "--vocabulary-source",
                    "test",
                    "--refusal-rs",
                    str(_refusal_rs_path()),
                    "--artifact-version",
                    "v0.0.0-test",
                    "--platform",
                    "linux-test",
                    "--harness-sha",
                    "c" * 40,
                    "--base",
                    f"http://127.0.0.1:{port}",
                    "--out",
                    str(tmp / "report" / "clean-room.json"),
                ]
            )
        finally:
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
        self.assertEqual(rc, 0)
        block = json.loads((tmp / "report" / "clean-room.json").read_text())
        joined = artifact.join(tmp / "absent-parity.json", block, tmp / "joined.json")
        return block, joined, tmp

    def test_cli_end_to_end(self):
        block, joined, tmp = self._cli("Projects Terminal")
        by = {s["step"]: s for s in block["steps"]}
        self.assertEqual(by["first_launch"]["outcome"], "pass", by["first_launch"])
        self.assertEqual(by["start_session"]["outcome"], "pass", by["start_session"])
        self.assertEqual(
            (by["glossary"]["outcome"], by["glossary"]["reason"]),
            ("unknown", "feature_not_released"),
        )
        self.assertEqual(by["close"]["outcome"], "pass", by["close"])
        self.assertEqual(
            block["fleet_nouns"]["verdict"], "clean", block["fleet_nouns"]["hits"]
        )
        self.assertTrue(block["next_action_kinds"]["source"].startswith("parsed:"))
        self.assertEqual(joined["clean_room"]["harness_sha"], "c" * 40)
        self.assertTrue((tmp / "report" / "evidence").is_dir())

    def test_cli_planted_root_is_red(self):
        block, _, _ = self._cli("root: D:/qontinui-root")
        self.assertEqual(block["fleet_nouns"]["verdict"], "red")

    def test_cli_unreadable_fixture_record_launches_nothing(self):
        tmp = Path(tempfile.mkdtemp(prefix="clean-room-e2e-"))
        self.addCleanup(shutil.rmtree, tmp, True)
        (tmp / "install.json").write_text(
            json.dumps({"exe": "/nonexistent", "error": None})
        )
        (tmp / "preflight.json").write_text(
            json.dumps({"verdict": "clean", "findings": []})
        )
        rc = scenario.main(
            [
                "--install",
                str(tmp / "install.json"),
                "--preflight",
                str(tmp / "preflight.json"),
                "--fixture",
                str(tmp / "absent-fixture.json"),
                "--vocabulary",
                str(VOCAB_PATH),
                "--artifact-version",
                "v",
                "--platform",
                "p",
                "--harness-sha",
                "h",
                "--base",
                f"http://127.0.0.1:{self._free_port()}",
                "--out",
                str(tmp / "r" / "clean-room.json"),
            ]
        )
        self.assertEqual(rc, 0)
        block = json.loads((tmp / "r" / "clean-room.json").read_text())
        self.assertEqual(block["steps"][0]["reason"], "harness_error")
        self.assertFalse(any(s["outcome"] == "pass" for s in block["steps"]))


if __name__ == "__main__":
    unittest.main(verbosity=2)
