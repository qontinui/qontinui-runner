//! `coord doctor` — one-command runner self-check for coord access + gate
//! registration (plan 2026-06-13 Phase 4).
//!
//! Runs NINE ordered checks: eight BLOCKING ones that stop at the first red
//! and report that link + its fix, plus one ADVISORY check that always runs
//! (a warning that never changes the verdict — see [`CheckResult::advisory`]).
//! Green on all nine ⇒ "this runner can set gates." The output is
//! copy-pasteable and **identical across machines**, so MSI / spaceship / a
//! fresh box all self-diagnose the same way.
//!
//! # Why this lives in the lib crate
//!
//! Two consumers need it: the headless standalone bin (`src/bin/coord_doctor.rs`)
//! and the in-app Tauri command (`crate::coord_doctor_cmd` in the runner
//! binary). A `src/bin/*` crate cannot import the runner binary's module tree,
//! so the reusable core (types + the first-red-stops DRIVER + the report
//! FORMATTER + `diagnose`) lives here in `qontinui_runner_lib` where both can
//! reach it.
//!
//! The checks reuse the same on-disk + secure-storage state the live predicates
//! read (`auth`, `pair`, `secure_storage`, `profiles` are all lib modules that
//! compile into the runner binary too), so the bin and the Tauri command
//! produce the SAME report. The one fact only the running runtime knows — the
//! ACTUALLY-BOUND API port (check 6) — is injected via [`DoctorInputs`]: the
//! Tauri command passes the live `coord_mcp::resolve_bound_api_port()`, the
//! standalone bin passes `None` (and check 6 honestly reports "bound port
//! unknown — run from inside the running runner").

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;

// ===========================================================================
// Result types (pure data — serialize straight to the Tauri command / JSON)
// ===========================================================================

/// One check's outcome. `name` identifies the link; `ok` is the verdict;
/// `detail` is a one-line human explanation; `fix` is the actionable next step
/// (only meaningful when `!ok`, but always carried so the report is uniform).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CheckResult {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    pub fix: String,
    /// A failure here is a WARNING, not a blocker: it does not stop the chain
    /// and does not flip [`DoctorReport::overall_ok`].
    ///
    /// This report answers exactly one question — "can this runner set gates?"
    /// — so a red must mean "no". Hygiene findings (an inherited Claude
    /// session marker, say) are worth surfacing but do not stop gate
    /// registration, and reporting one as `BLOCKED` would make the report lie
    /// about its own subject. Defaults to `false`; `serde` omits it so the
    /// existing JSON shape is unchanged for blocking checks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub advisory: bool,
}

/// The whole self-check: the ordered checks actually RUN (the driver stops
/// after the first red, so later checks are absent), plus the overall verdict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DoctorReport {
    pub checks: Vec<CheckResult>,
    pub overall_ok: bool,
}

impl DoctorReport {
    /// Render the copy-pasteable, machine-identical text report. The ordering
    /// and the trailing verdict line are the contract — keep them stable so
    /// the output diffs cleanly across machines.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("coord doctor — runner gate-access self-check\n");
        out.push_str("============================================\n");
        for (i, c) in self.checks.iter().enumerate() {
            let mark = match (c.ok, c.advisory) {
                (true, _) => "PASS",
                (false, true) => "WARN",
                (false, false) => "FAIL",
            };
            out.push_str(&format!(
                "{}. [{}] {} — {}\n",
                i + 1,
                mark,
                c.name,
                c.detail
            ));
            if !c.ok {
                out.push_str(&format!("      fix: {}\n", c.fix));
            }
        }
        out.push_str("--------------------------------------------\n");
        if self.overall_ok {
            out.push_str("OK: this runner can set gates.\n");
            // Advisory reds do not change the verdict, but a silent warning is
            // a warning nobody acts on — name them under the OK line.
            let warned: Vec<&str> = self
                .checks
                .iter()
                .filter(|c| !c.ok && c.advisory)
                .map(|c| c.name.as_str())
                .collect();
            if !warned.is_empty() {
                out.push_str(&format!(
                    "   (warnings, not blocking: {})\n",
                    warned.join(", ")
                ));
            }
        } else {
            // The first BLOCKING red is the one to fix; name it again so a
            // glance at the last line is enough. Advisory reds are skipped
            // here — they never blocked anything.
            let first_red = self.checks.iter().find(|c| !c.ok && !c.advisory);
            match first_red {
                Some(c) => out.push_str(&format!("BLOCKED at: {} — {}\n", c.name, c.fix)),
                None => out.push_str("BLOCKED (no check ran).\n"),
            }
        }
        out
    }
}

// ===========================================================================
// Pure first-red-stops driver (injectable check fns → unit-testable)
// ===========================================================================

/// What running one check produced: the verdict, the human detail, and —
/// optionally — a fix that REFINES the check's static one.
///
/// # Why a check may override its own fix
///
/// The blocked report's last line is `BLOCKED at: <name> — <fix>`, so `fix` is
/// the string an operator actually acts on. For most checks one static
/// remediation covers every way they can fail, and sourcing it from
/// [`CHECK_SPECS`] is what stops the generated onboarding doc drifting from
/// the live report.
///
/// The tier check is the exception, and it is the exception for the same
/// NO-DOWNGRADE reason the tri-state [`crate::profiles::TierRead`] exists:
/// "settings.json is unreadable" and "the tier is set to local" are different
/// faults with genuinely different fixes, and telling an operator with a
/// corrupt settings.json to "set the runner tier" sends them to the wrong
/// place. A check that can distinguish its failures must be able to say so.
///
/// `None` (the common case — `From<(bool, String)>` produces it) means "use
/// the static spec fix".
pub struct CheckOutcome {
    pub ok: bool,
    pub detail: String,
    pub fix: Option<String>,
}

impl From<(bool, String)> for CheckOutcome {
    fn from((ok, detail): (bool, String)) -> Self {
        Self {
            ok,
            detail,
            fix: None,
        }
    }
}

/// A single check: a static name + fix, plus a thunk that runs the predicate
/// and returns a [`CheckOutcome`] (a bare `(ok, detail)` tuple converts into
/// one). The thunk is `FnOnce` so a check can own resources; the driver only
/// ever calls it once.
pub struct Check<'a> {
    pub name: &'static str,
    pub fix: &'static str,
    pub run: Box<dyn FnOnce() -> CheckOutcome + 'a>,
    /// See [`CheckResult::advisory`]. Constructed via [`Check::advisory`].
    pub advisory: bool,
    /// See [`CheckSpec::always_run`]: run this check even after an earlier
    /// BLOCKING check went red. Independent of `advisory`.
    pub always_run: bool,
}

impl<'a> Check<'a> {
    /// A BLOCKING check: a failure stops the chain and fails the report.
    pub fn new<R: Into<CheckOutcome>>(
        name: &'static str,
        fix: &'static str,
        run: impl FnOnce() -> R + 'a,
    ) -> Self {
        Self {
            name,
            fix,
            run: Box::new(move || run().into()),
            advisory: false,
            always_run: false,
        }
    }

    /// An ADVISORY check: a failure is reported as a warning, but the chain
    /// continues and the overall verdict is unaffected. Use for findings that
    /// are worth an operator's attention yet do not stop this runner from
    /// registering gates.
    pub fn advisory<R: Into<CheckOutcome>>(
        name: &'static str,
        fix: &'static str,
        run: impl FnOnce() -> R + 'a,
    ) -> Self {
        Self {
            name,
            fix,
            run: Box::new(move || run().into()),
            advisory: true,
            always_run: false,
        }
    }

    /// Build a check from its [`CheckSpec`], taking `name`, `fix` AND
    /// `advisory` from the table.
    ///
    /// Preferred over hand-picking [`Check::new`] vs [`Check::advisory`]: the
    /// spec table is what the onboarding doc renders from, so choosing the
    /// constructor independently lets the doc claim a check blocks while the
    /// live chain treats it as advisory (or vice versa). Here they are one
    /// value.
    pub fn from_spec<R: Into<CheckOutcome>>(
        spec: &'static CheckSpec,
        run: impl FnOnce() -> R + 'a,
    ) -> Self {
        Self {
            name: spec.name,
            fix: spec.fix,
            run: Box::new(move || run().into()),
            advisory: spec.advisory,
            // Taken from the table for the same reason `advisory` is: the doc
            // renders from the spec, so a hand-picked value here could let the
            // doc and the live chain disagree about which checks are reachable.
            always_run: spec.always_run,
        }
    }
}

/// Run `checks` in order. The first red in a BLOCKING check stops the rest of
/// the blocking chain (each one presupposes the previous, so running on is
/// noise). **Advisory checks always run**, even after a blocking red.
///
/// That asymmetry is deliberate. An advisory check is independent of the
/// credential chain — an inherited env marker has nothing to do with whether
/// you are signed in — so gating it behind "everything else is green" would
/// hide it on exactly the misconfigured machines most likely to have it. A
/// detector that silently does not run is the failure class this whole plan
/// exists to fix (`2026-07-28-runner-transcript-persistence-env-leak` §5: the
/// leak "ran indefinitely because nothing watched for it").
///
/// Returns a [`DoctorReport`] whose `checks` are the blocking prefix that ran
/// PLUS every advisory check. Pure over the injected closures — the unit tests
/// drive it with fake checks to assert this without any live runner.
pub fn run_checks(checks: Vec<Check<'_>>) -> DoctorReport {
    let mut results = Vec::with_capacity(checks.len());
    let mut overall_ok = true;
    let mut blocked = false;
    for check in checks {
        // A blocking red suppresses the remaining BLOCKING checks — except
        // the ones that declare themselves independent of everything before
        // them. `always_run` is NOT a second spelling of `advisory`: such a
        // check still contributes to `overall_ok`, so a red in it still
        // blocks. What it buys is that the check EXECUTES, which is the whole
        // of the `coord_reachable` defect (plan
        // `2026-08-31-coord-mcp-credential-selection-by-binding-provenance`
        // Phase 5a).
        if blocked && !check.advisory && !check.always_run {
            continue;
        }
        let name = check.name;
        let spec_fix = check.fix;
        let advisory = check.advisory;
        let outcome = (check.run)();
        let ok = outcome.ok;
        results.push(CheckResult {
            name: name.to_string(),
            ok,
            detail: outcome.detail,
            // A check that can distinguish its own failure modes may REFINE
            // the static fix (see [`CheckOutcome::fix`]); everything else
            // reports the spec's, which is what keeps the generated
            // onboarding doc and the live report in step.
            fix: outcome.fix.unwrap_or_else(|| spec_fix.to_string()),
            advisory,
        });
        if !ok && !advisory {
            overall_ok = false;
            blocked = true;
        }
    }
    DoctorReport {
        checks: results,
        overall_ok,
    }
}

// ===========================================================================
// Single source of truth — the static spec for each of the 10 checks.
//
// `name` and `fix` are sourced FROM here by `diagnose()` (so the live report
// can't drift from this table), and the onboarding doc is GENERATED from here
// (so the doc can't drift from the report). `title` + `verifies` are the
// human-readable, doc-only fields. A test asserts the live `diagnose()` chain
// and `CHECK_SPECS` agree on order + names, so adding/removing/reordering a
// check without updating this table fails the build.
// ===========================================================================

/// The static, doc-and-report-shared spec for one doctor check. `name`/`fix`
/// are the SAME strings the live `Check` carries (sourced here by `diagnose`);
/// `title` + `verifies` are the human-readable description rendered into the
/// onboarding doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckSpec {
    /// Stable machine name (matches the live `CheckResult::name`).
    pub name: &'static str,
    /// Short human title for the doc section heading.
    pub title: &'static str,
    /// One-line "what this verifies" description for the doc.
    pub verifies: &'static str,
    /// Actionable fix (matches the live `CheckResult::fix`).
    pub fix: &'static str,
    /// Whether this check is ADVISORY (see [`CheckResult::advisory`]).
    ///
    /// `diagnose()` picks `Check::advisory` vs `Check::new` FROM this flag, so
    /// the table and the live chain cannot disagree about which checks block.
    pub advisory: bool,
    /// Whether this check runs even after an earlier BLOCKING check went red.
    ///
    /// Orthogonal to `advisory`, and the two answer different questions:
    /// `advisory` is "may a red here withhold gate access?", `always_run` is
    /// "is this check's input independent of everything before it?". A check
    /// that dials a remote service is independent of every LOCAL check, so
    /// suppressing it on a local red discards the one signal that would
    /// separate "your config is stale" from "coord is down".
    ///
    /// An `always_run` check that is NOT advisory still blocks when it is red —
    /// that combination is the point.
    pub always_run: bool,
}

/// The 10 checks in `diagnose()` order. THE single source of truth for check
/// names + fixes (the live report sources them here) and for the onboarding
/// doc (which is generated from here). Index 5 (check 6, device-JWT-live) is
/// also the source the `DEVICE_JWT_LIVE_CHECK_NAME`/`_FIX` constants derive
/// from — see the consts below and the `device_jwt_live_spec_matches_constants` test.
pub const CHECK_SPECS: &[CheckSpec] = &[
    CheckSpec {
        name: "claude_account",
        title: "Claude account signed in",
        verifies: "a Claude account with live credentials is present \
                   (a valid ~/.claude/.credentials.json)",
        fix: "run /login",
        advisory: false,
        always_run: false,
    },
    CheckSpec {
        name: "tier",
        title: "Runner tier is Qontinui account",
        verifies: "the runner tier RESOLVES to qontinui_account — either \
                   settings.json::tier says so, or the shared inference \
                   (`profiles::infer_tier`: a device pairing, or a legacy \
                   web_integration.runner_token) supplies it on a document that \
                   records no explicit operator choice. When a box that IS \
                   credentialed still resolves non-account, the report says so \
                   — \"credentialed but not authorized\" is a different failure \
                   from \"no credential\" and has a different fix",
        fix: "set runner tier to Qontinui account — app: Settings \u{2192} Account; \
              headless: `qontinui_profile device pair --pair-code <code>` promotes \
              it, or launch with QONTINUI_SERVER_MODE=1; if this box is already \
              paired and still reads non-account, a tier choice is pinning it \
              \u{2014} run `qontinui_profile tier --clear-choice`, or, on a \
              `local_provider` document (which clearing alone does not re-open), \
              `qontinui_profile tier --set qontinui_account`",
        advisory: false,
        always_run: false,
    },
    CheckSpec {
        name: "credential_store_readable",
        title: "Credential store readable",
        verifies: "the credential store (OS keychain / on-disk slot) can be \
                   READ — placed ahead of every bearer-consuming check so an \
                   unreadable store reports itself instead of being \
                   misdiagnosed as 'not signed in' or 'no tenant'",
        fix: "credential store unreadable — check file permissions / OS keychain access",
        advisory: false,
        always_run: false,
    },
    CheckSpec {
        name: "paired_signed_in",
        title: "Paired and signed in",
        verifies: "paired_user.json is present and a bearer is stored in the \
                   access-token slot",
        fix: "sign in / re-pair",
        advisory: false,
        always_run: false,
    },
    CheckSpec {
        name: "tenant_resolvable",
        title: "Tenant resolvable",
        verifies: "a tenant_id resolves from the OAuth/runner-bearer claim, the \
                   outgoing device-JWT, or machine.json::active_tenant_id",
        fix: "machine.json missing active_tenant_id",
        advisory: false,
        always_run: false,
    },
    CheckSpec {
        name: "device_jwt_live",
        title: "Coord device JWT live",
        verifies: "a live coord device JWT is present in the access-token slot \
                   and is not near expiry",
        fix: "kick refresher / re-pair",
        advisory: false,
        always_run: false,
    },
    CheckSpec {
        name: "mcp_json_valid",
        title: ".mcp.json valid",
        verifies: "the session .mcp.json coord-mcp port equals the bound API \
                   port, its nonce is a registered proxy key, and the bearer is \
                   a coord device JWT",
        fix: "stale config — reprovision",
        advisory: false,
        always_run: false,
    },
    CheckSpec {
        name: "coord_reachable",
        title: "Coord reachable",
        verifies: "a one-shot tools/list JSON-RPC round-trips 200 against the \
                   configured coord /mcp endpoint, using the SAME bearer the \
                   coord-mcp proxy would select",
        fix: "coord unreachable",
        advisory: false,
        // ALWAYS RUN — plan
        // `2026-08-31-coord-mcp-credential-selection-by-binding-provenance`
        // Phase 5a, absorbing
        // `2026-08-24-coord-doctor-blocks-on-a-proxy-artifact-and-never-probes-the-direct-door`.
        //
        // `run_checks` skips every remaining check once a BLOCKING one goes
        // red. This is check 8, behind seven blocking checks — so ANY red in
        // 1-7 suppressed it, and it is the ONLY check that probes the direct
        // coord `/mcp` door. It had therefore effectively never executed on a
        // box with a problem, which is the only kind of box anyone runs the
        // doctor on. Most pointedly, `mcp_json_valid` (7) is a check on a
        // PROXY ARTIFACT, and this probe does not use the proxy at all.
        //
        // ADVISORY WOULD BE THE WRONG FIX and was tried first. It makes the
        // check run, but it also removes it from the blocking set — so a
        // runner that genuinely cannot reach coord would start reporting that
        // it can register gates. `always_run` keeps both properties: the check
        // executes whatever preceded it, AND a red still blocks.
        always_run: true,
    },
    CheckSpec {
        name: "no_inherited_session_markers",
        title: "No inherited Claude session markers",
        verifies: "this runner process did NOT inherit Claude Code's \
                   process-topology markers (CLAUDECODE, \
                   CLAUDE_CODE_CHILD_SESSION) from whatever launched it — a \
                   marked runner is mislabelled as a nested session",
        fix: "restart the runner from a shell without the markers (via \
              dev-start.ps1 / the supervisor); spawns are stripped either way",
        advisory: true,
        always_run: false,
    },
    CheckSpec {
        name: "mcp_json_not_dcr_escalating",
        title: ".mcp.json carries the non-escalating header shape",
        verifies: "the coord-mcp proxy .mcp.json carries the nonce in a static \
                   `Authorization: Bearer` header, not only in the legacy \
                   `X-Coord-Mcp-Proxy-Key` one — a legacy-only file \
                   authenticates fine today and still makes the next MCP \
                   client launched against it escalate a stale-key 401 into \
                   OAuth discovery, Dynamic Client Registration, this runner's \
                   own 404, and a durable client-side poison entry",
        fix: "spawn a terminal in that workdir (every session spawn rewrites \
              the file through the current emitter), or restart the runner so \
              the boot self-heal upgrades it in place with the same nonce",
        advisory: true,
        always_run: false,
    },
];

/// Look up a [`CheckSpec`] by name at construction time. Panics if the name is
/// absent — `diagnose()` only ever passes literals that ARE in `CHECK_SPECS`,
/// and a missing entry is a programming error that should fail loudly (the
/// `diagnose_order_matches_specs` test would also catch it).
fn spec(name: &'static str) -> &'static CheckSpec {
    CHECK_SPECS
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no CHECK_SPECS entry for check {name:?}"))
}

// ===========================================================================
// Onboarding doc generator — emits the new-runner provisioning checklist as
// markdown straight from CHECK_SPECS, so the doc and the live `coord doctor`
// report can never drift. Checked in at `docs/runner-onboarding.md` and
// enforced fresh by `.github/workflows/onboarding-doc-fresh.yml`.
// ===========================================================================

/// Render the new-runner onboarding / provisioning checklist as markdown,
/// generated entirely from [`CHECK_SPECS`]. The output is byte-stable so a CI
/// gate can diff the checked-in `docs/runner-onboarding.md` against a fresh
/// regen. Emit it via `coord_doctor --onboarding-doc`.
pub fn render_onboarding_doc() -> String {
    let mut out = String::new();
    out.push_str("# Runner onboarding — coord access checklist\n\n");
    out.push_str(
        "This is the checklist a fresh runner must satisfy before it can reach \
         coord and set gates. Each item below is one of the ordered checks run \
         by `coord doctor` (plan 2026-06-13 Phase 4/5). A runner is not \
         provisioning-complete until **all** of them pass.\n\n",
    );
    out.push_str(
        "<!-- GENERATED FILE — do not edit by hand. Regenerate via \
         `coord_doctor --onboarding-doc` (or \
         `cargo run --bin coord_doctor -- --onboarding-doc > docs/runner-onboarding.md`). \
         The source of truth is `CHECK_SPECS` in \
         `src-tauri/src/coord_doctor.rs`. -->\n\n",
    );
    out.push_str("## Provisioning checklist\n\n");
    for (i, s) in CHECK_SPECS.iter().enumerate() {
        // Three states now, not two — `always_run` is orthogonal to `advisory`
        // (plan `2026-08-31-coord-mcp-credential-selection-by-binding-provenance`
        // Phase 5a). A doc that kept saying "blocking checks stop at the first
        // failure" while one of them no longer does would be a document lying
        // about behaviour, which is the defect class that plan exists to end.
        let suffix = if s.advisory {
            " — ADVISORY"
        } else if s.always_run {
            " — BLOCKING, ALWAYS RUNS"
        } else {
            ""
        };
        out.push_str(&format!(
            "### {}. {} (`{}`){}\n\n",
            i + 1,
            s.title,
            s.name,
            suffix
        ));
        out.push_str(&format!("Verifies: {}\n\n", s.verifies));
        if s.advisory {
            out.push_str(
                "Advisory: a failure here is a **warning**, not a blocker — it does not \
                 stop gate registration and does not fail the report. It also runs even \
                 when an earlier check went red.\n\n",
            );
        } else if s.always_run {
            out.push_str(
                "Always runs: this check's input does not depend on any check before it, \
                 so an earlier red does not suppress it. It is still **blocking** — a \
                 failure here withholds gate registration exactly as any other blocking \
                 check does.\n\n",
            );
        }
        out.push_str(&format!("**Fix:** {}\n\n", s.fix));
    }
    out.push_str("---\n\n");
    out.push_str(
        "`coord doctor` runs these checks live. The **blocking** checks stop at the \
         first failure, naming that one link plus its fix — except any marked ALWAYS \
         RUNS, which are blocking but independent of everything before them, so they \
         execute anyway; **advisory** checks always run and only ever warn. Run it from \
         **Settings → Account** in the runner app, or headless via the `coord_doctor` \
         bin (`cargo run --bin coord_doctor`). Green on all of them ⇒ this runner can \
         set gates.\n",
    );
    out
}

// ===========================================================================
// Real wiring — the 10 checks, reusing existing predicates / on-disk state.
// ===========================================================================

/// Runtime-only facts the lib can't observe on its own. Injected so the Tauri
/// command (running inside the live runner) and the standalone bin produce the
/// same report shape, differing only where the bin genuinely cannot know the
/// live bound port.
#[derive(Debug, Clone, Default)]
pub struct DoctorInputs {
    /// The runner's ACTUALLY-BOUND loopback API port, from the live managed
    /// `AppState` (`coord_mcp::resolve_bound_api_port()`). `None` from the
    /// standalone bin (no running runtime) ⇒ check 6 reports the port as
    /// unverifiable rather than guessing.
    pub bound_api_port: Option<u16>,
    /// Optional explicit Claude config dir to check for check 1; `None` means
    /// "the ambient default location" (matches the spawn path's behavior).
    pub claude_config_dir: Option<String>,
}

/// Run the full 8-check self-check and return the structured report.
///
/// Each check reuses the canonical state:
/// 1. Claude account — `.credentials.json` validity (same paths + expiry logic
///    as `ai_provider::oauth_refresh::default_location_has_valid_credentials`).
/// 2. Tier — the tier RESOLVES to `qontinui_account` (`profiles::read_runner_tier`,
///    which applies the shared inference), tri-state so an unreadable
///    settings.json reports as UNKNOWN rather than `local`. On a non-account
///    tier it also reports the box's credential state, so "credentialed but
///    NOT authorized" reads differently from "no credential" — see
///    [`tier_check_verdict`].
///    2b. Credential store readable — a store read ERROR is reported as
///    itself, ahead of every bearer-consuming check it would otherwise
///    misdiagnose.
/// 3. Paired + signed in — `paired_user.json`
///    (`pair::read_paired_user_id_from_disk`) + a bearer in the access-token
///    slot (`auth::AuthManager::get_access_token`).
/// 4. Tenant resolvable — OAuth claim → outgoing-JWT claim →
///    `machine.json::active_tenant_id` (mirrors
///    `device_jwt_refresher::resolve_pair_tenant_id`).
/// 5. Device JWT live — `auth::AuthManager::device_jwt_needs_refresh()` ==
///    `Ok(false)` with a token present.
/// 6. `.mcp.json` valid — its coord-mcp port == the bound port AND its nonce
///    is registered + the bearer is a device JWT (mirrors
///    `coord_mcp::proxy_request_gate`).
/// 7. Coord reachable — a one-shot `tools/list` round-trips 200 against the
///    configured coord-mcp endpoint.
pub fn diagnose(inputs: &DoctorInputs) -> DoctorReport {
    let cfg_dir = inputs.claude_config_dir.clone();
    let bound_port = inputs.bound_api_port;

    // Each check sources its `name`/`fix` from the matching `CHECK_SPECS`
    // entry so the live report can never drift from the generated doc.
    let s_claude = spec("claude_account");
    let s_tier = spec("tier");
    let s_cred_store = spec("credential_store_readable");
    let s_paired = spec("paired_signed_in");
    let s_tenant = spec("tenant_resolvable");
    let s_mcp = spec("mcp_json_valid");
    let s_coord = spec("coord_reachable");
    let s_markers = spec("no_inherited_session_markers");
    let s_dcr = spec("mcp_json_not_dcr_escalating");

    // ONE credential-store read, lazily taken, shared by check 2 (tier) and
    // check 3 (credential_store_readable). See [`SharedTokenProbe`].
    //
    // M4 / NO-DOWNGRADE is UNAFFECTED: the check ORDER below is unchanged, so
    // `credential_store_readable` still sits ahead of every bearer-CONSUMING
    // check. Sharing the read does not move it; the tier check consumes no
    // bearer, it only reports whether one is there.
    let token_probe = SharedTokenProbe::default();

    let checks = vec![
        Check::new(s_claude.name, s_claude.fix, move || {
            let ok = claude_account_has_valid_credentials(cfg_dir.as_deref());
            if ok {
                (
                    true,
                    "a Claude account with live credentials is present".into(),
                )
            } else {
                (
                    false,
                    "no authenticated Claude account (no valid ~/.claude/.credentials.json)".into(),
                )
            }
        }),
        // The tier check reports the WHOLE truth: not just the field, but what
        // the box's credential state says about it. A paired, tenant-bound,
        // bearer-holding box that reads `local` is "credentialed but NOT
        // authorized" — a materially different failure from "this box has no
        // credential", with a different fix. The verdict itself is the pure
        // `tier_check_verdict`; this closure only gathers the observations.
        {
            let probe = token_probe.clone();
            Check::new(s_tier.name, s_tier.fix, move || {
                let tier = read_runner_tier();
                // A tier that already resolves to qontinui_account needs no
                // evidence to explain itself, so the credential store is not
                // touched at all on the green path.
                if tier.known() == Some(crate::profiles::QONTINUI_ACCOUNT_TIER) {
                    return tier_check_verdict(&tier, &TierEvidence::default());
                }
                let evidence = TierEvidence {
                    // `pair::device_is_paired` is a plain `paired_user.json`
                    // read — no credential store, no OS keychain — so asking
                    // it here costs the doctor nothing.
                    paired: crate::pair::device_is_paired(),
                    tenant_id: crate::pair::read_paired_tenant_id_from_disk(),
                    credential: CredentialEvidence::from_store_read(probe.read()),
                };
                tier_check_verdict(&tier, &evidence)
            })
        },
        // M4 / NO-DOWNGRADE: the credential-store read is its own check, placed
        // AHEAD of every check that consumes a bearer. Previously each consumer
        // did `get_access_token().ok().unwrap_or_default()` (or `.ok()`),
        // feeding an empty bearer downstream — so an unreadable store was
        // misdiagnosed as "not signed in" / "machine.json missing
        // active_tenant_id" / "bearer is not a device JWT". Since `run_checks`
        // stops at the first red, an unreadable store now reports itself
        // instead of blaming the next check in line.
        {
            // Same ONE store read the tier check above may already have taken
            // (see `SharedTokenProbe`): the two must never disagree about what
            // the store said, and the doctor must not pay for two round-trips.
            let probe = token_probe.clone();
            Check::new(s_cred_store.name, s_cred_store.fix, move || {
                match probe.read() {
                    crate::secure_storage::StoredTokenRead::Unreadable(e) => (
                        false,
                        format!(
                            "credential store could not be read ({e}) — every check \
                             below would misreport as 'not signed in' / 'no tenant'. \
                             The runner has NOT been signed out."
                        ),
                    ),
                    _ => (true, "credential store is readable".into()),
                }
            })
        },
        {
            let auth_ref = crate::auth::AuthManager::new();
            Check::new(s_paired.name, s_paired.fix, move || {
                let paired = crate::pair::read_paired_user_id_from_disk().is_some();
                let bearer = auth_ref
                    .get_access_token()
                    .ok()
                    .filter(|t| !t.trim().is_empty())
                    .is_some();
                match (paired, bearer) {
                    (true, true) => (
                        true,
                        "paired_user.json present and a bearer is stored".into(),
                    ),
                    (false, _) => (false, "paired_user.json missing — runner not paired".into()),
                    (true, false) => (
                        false,
                        "paired but access-token slot is empty — sign in".into(),
                    ),
                }
            })
        },
        {
            let auth_ref = crate::auth::AuthManager::new();
            Check::new(s_tenant.name, s_tenant.fix, move || {
                let bearer = match auth_ref.get_access_token() {
                    Ok(b) => b,
                    Err(e) => {
                        return (
                            false,
                            format!(
                                "credential store unreadable ({e}) — the tenant is UNKNOWN, \
                                 not missing; this is not a tenant misconfiguration"
                            ),
                        )
                    }
                };
                match resolve_tenant_for_doctor(&bearer) {
                    Some((tid, src)) => (true, format!("tenant {tid} resolved from {src}")),
                    None => (
                        false,
                        "no tenant_id from OAuth claim, outgoing device-JWT, or \
                             machine.json::active_tenant_id"
                            .into(),
                    ),
                }
            })
        },
        {
            let auth_ref = crate::auth::AuthManager::new();
            Check::new(
                DEVICE_JWT_LIVE_CHECK_NAME,
                DEVICE_JWT_LIVE_CHECK_FIX,
                move || device_jwt_live_predicate(&auth_ref),
            )
        },
        {
            let auth_ref = crate::auth::AuthManager::new();
            Check::new(s_mcp.name, s_mcp.fix, move || {
                mcp_json_check(bound_port, &auth_ref)
            })
        },
        Check::new(s_coord.name, s_coord.fix, coord_reachable_check),
        // ADVISORY (flag comes from CHECK_SPECS, see `Check::from_spec`): an
        // inherited marker is worth fixing but does not stop this runner
        // registering gates, so it must not produce a BLOCKED verdict. Being
        // advisory it also runs even when an earlier check went red — which is
        // the point, since env hygiene is independent of the credential chain.
        Check::from_spec(s_markers, inherited_session_markers_check),
        // ADVISORY, and last. A legacy-only config is a real finding but not a
        // blocker: it authenticates perfectly today, so failing the report on
        // it would withhold gate registration from a runner whose coord access
        // works. Being advisory it also runs after a blocking red — which is
        // the point here, because the header shape is independent of the
        // credential chain, and the machines most likely to carry a pre-Phase-2
        // config are exactly the ones with something else wrong too.
        Check::from_spec(s_dcr, mcp_json_dcr_escalation_check),
    ];

    run_checks(checks)
}

/// Check 9 — this process did not inherit Claude Code's process-topology
/// markers.
///
/// Ordered LAST deliberately: the driver stops at the first red, and every
/// preceding check is a hard blocker for gate registration whereas this one is
/// hygiene — a marked runner still registers gates fine. Putting it earlier
/// would mask a genuine credential failure behind an env-cleanliness warning.
fn inherited_session_markers_check() -> (bool, String) {
    let inherited = crate::claude_env::inherited_session_markers();
    if inherited.is_empty() {
        (
            true,
            "no inherited Claude Code session markers on this process".into(),
        )
    } else {
        (
            false,
            crate::claude_env::inherited_markers_detail(&inherited),
        )
    }
}

// ===========================================================================
// Check 5 (device JWT live) — extracted so the Phase-5 provisioning gate can
// reuse the EXACT predicate `diagnose` runs (no second device-JWT impl).
// ===========================================================================

/// The stable `name`/`fix` for the device-JWT-live check, shared between [`diagnose`] and
/// [`device_jwt_live_check`] so the provisioning gate and the full doctor
/// report name the link identically. DERIVED from `CHECK_SPECS[5]` — the
/// single source of truth — so the provisioning gate, the report, and the
/// onboarding doc all name that check with the SAME strings. The
/// `device_jwt_live_spec_matches_constants` test pins index 5 == `device_jwt_live`.
pub const DEVICE_JWT_LIVE_CHECK_NAME: &str = CHECK_SPECS[5].name;
pub const DEVICE_JWT_LIVE_CHECK_FIX: &str = CHECK_SPECS[5].fix;

/// The check-5 predicate over an `AuthManager`: a live device JWT is present
/// and not near expiry. `(ok, detail)`. The single source of truth for "does
/// this runner hold a usable coord device JWT" — both the full [`diagnose`]
/// chain and the Phase-5 provisioning gate call it.
fn device_jwt_live_predicate(auth: &crate::auth::AuthManager) -> (bool, String) {
    match auth.device_jwt_needs_refresh() {
        Ok(false) => {
            if auth
                .get_access_token()
                .ok()
                .filter(|t| !t.trim().is_empty())
                .is_some()
            {
                (true, "device JWT present and not near expiry".into())
            } else {
                (false, "no device JWT in the access-token slot".into())
            }
        }
        Ok(true) => (
            false,
            "device JWT missing, expired, or near expiry (needs refresh)".into(),
        ),
        Err(e) => (false, format!("could not read device-JWT freshness: {e}")),
    }
}

/// Run ONLY check 5 (device JWT live) against the default `AuthManager` and
/// return its structured [`CheckResult`]. This is the single check Phase 5
/// gates "ready" on: a runner is provisioning-complete only once it holds a
/// live coord device JWT. Reuses [`device_jwt_live_predicate`] — it does NOT
/// reimplement the device-JWT freshness logic.
pub fn device_jwt_live_check() -> CheckResult {
    let auth = crate::auth::AuthManager::new();
    let (ok, detail) = device_jwt_live_predicate(&auth);
    CheckResult {
        name: DEVICE_JWT_LIVE_CHECK_NAME.to_string(),
        ok,
        detail,
        fix: DEVICE_JWT_LIVE_CHECK_FIX.to_string(),
        advisory: false,
    }
}

// ===========================================================================
// Phase 5 — provisioning completeness gate.
//
// A runner should not report "ready" until it holds a live coord device JWT
// (doctor check 5). The gate is ADVISORY by default — it surfaces the gap
// (via the Phase-1b credential-status path) and logs loudly WITHOUT blocking
// the runner from starting (a hard block could brick a runner mid-provision).
// Setting `QONTINUI_PROVISIONING_GATE_ENFORCE` to a non-`0` value flips it to
// ENFORCING: readiness is withheld until check 5 passes.
// ===========================================================================

/// Env flag that flips the provisioning gate from advisory (default) to
/// enforcing. Unset / `0` / `false` ⇒ advisory.
pub const PROVISIONING_GATE_ENFORCE_ENV: &str = "QONTINUI_PROVISIONING_GATE_ENFORCE";

/// Outcome of the provisioning-completeness gate over doctor check 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisioningReadiness {
    /// Check 5 passed (a live device JWT) ⇒ provisioning-complete, report ready.
    Ready,
    /// Check 5 failed but the gate is ADVISORY ⇒ surface + log, but DO NOT
    /// withhold ready (never brick a runner mid-provision).
    IncompleteAdvisory,
    /// Check 5 failed AND the gate is ENFORCING ⇒ withhold ready; the runner
    /// is provisioning-incomplete until a device JWT goes live.
    IncompleteEnforced,
}

impl ProvisioningReadiness {
    /// Whether the runner may report "ready" given this verdict. `true` for
    /// `Ready` and for `IncompleteAdvisory` (advisory never blocks); only
    /// `IncompleteEnforced` withholds readiness.
    pub fn ready_to_report(self) -> bool {
        !matches!(self, ProvisioningReadiness::IncompleteEnforced)
    }

    /// Whether the runner is provisioning-INCOMPLETE (check 5 red), regardless
    /// of advisory-vs-enforce. Drives the Phase-1b credential-status surface.
    pub fn is_incomplete(self) -> bool {
        !matches!(self, ProvisioningReadiness::Ready)
    }
}

/// Pure provisioning-gate predicate: given check 5's pass/fail and whether the
/// gate is enforcing, decide the readiness verdict. Pure over its two inputs so
/// it is unit-testable with no live runner / no env. The caller resolves
/// `check5_ok` from [`device_jwt_live_check`] and `enforce` from
/// [`provisioning_gate_enforce_enabled`].
pub fn provisioning_readiness(check5_ok: bool, enforce: bool) -> ProvisioningReadiness {
    match (check5_ok, enforce) {
        (true, _) => ProvisioningReadiness::Ready,
        (false, false) => ProvisioningReadiness::IncompleteAdvisory,
        (false, true) => ProvisioningReadiness::IncompleteEnforced,
    }
}

/// Resolve the advisory-vs-enforce flag from
/// [`PROVISIONING_GATE_ENFORCE_ENV`]. Enforcing only when the var is set to a
/// non-empty value other than `0`/`false` (default: advisory).
pub fn provisioning_gate_enforce_enabled() -> bool {
    match std::env::var(PROVISIONING_GATE_ENFORCE_ENV) {
        Ok(v) => {
            let v = v.trim();
            !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Check 1 — Claude account credentials (relocated pure file logic).
//
// Mirrors `ai_provider::oauth_refresh::{find_creds_path, creds_path_is_valid,
// is_expired}` for the UNEXPIRED case (the doctor reports a verdict; it does
// NOT silently refresh, so it deliberately does not invoke the OAuth refresh
// grant — an expired-but-refreshable account reads as red, prompting /login,
// which is the honest doctor answer rather than a side-effecting refresh).
// ---------------------------------------------------------------------------

fn find_creds_path(config_dir: Option<&str>) -> Option<PathBuf> {
    if let Some(dir) = config_dir {
        let p = PathBuf::from(dir).join(".credentials.json");
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let p = PathBuf::from(&dir).join(".credentials.json");
        if p.exists() {
            return Some(p);
        }
    }
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".claude").join(".credentials.json");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// True iff the creds file exists and its `claudeAiOauth.expiresAt` (ms) is in
/// the future (or absent — a creds file with no expiry is treated as live,
/// matching the source predicate's `is_expired == false` branch).
fn creds_path_is_valid(creds_path: &Path) -> bool {
    let content = match std::fs::read_to_string(creds_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let expires_at_ms = json["claudeAiOauth"]["expiresAt"].as_i64().unwrap_or(0);
    if expires_at_ms == 0 {
        return true; // no expiry recorded → treat as live
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    expires_at_ms > now_ms
}

fn claude_account_has_valid_credentials(config_dir: Option<&str>) -> bool {
    if let Some(path) = find_creds_path(config_dir) {
        if creds_path_is_valid(&path) {
            return true;
        }
    }
    // macOS stores Claude Code's OAuth credentials in the login Keychain
    // (service "Claude Code-credentials"), NOT in ~/.claude/.credentials.json —
    // so the file-only check above reports a logged-in operator as
    // unauthenticated, telling them to "run /login" when they already have.
    // Accept the Keychain item as proof of a Claude account.
    #[cfg(target_os = "macos")]
    {
        if macos_keychain_has_claude_credentials() {
            return true;
        }
    }
    false
}

/// True iff the macOS login Keychain holds a Claude Code credentials item.
///
/// Queries item ATTRIBUTES only (no `-w`/`-g`), so it never reads the secret
/// and therefore never triggers a Keychain access-control prompt. Presence of
/// the item means the operator has completed `/login` on this machine (Claude
/// Code refreshes the token itself, so on-disk expiry isn't meaningful here).
#[cfg(target_os = "macos")]
fn macos_keychain_has_claude_credentials() -> bool {
    // console-ok: macOS keychain CLI — this arm never compiles on Windows.
    std::process::Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Check 2 — runner tier. The tier reader now lives in `crate::profiles`
// (relocated so the coord-base policy layer and the doctor share ONE reader);
// this thin delegate keeps the doctor's call sites unchanged.
// ---------------------------------------------------------------------------

/// The runner tier THIS SETTINGS DOCUMENT declares, as a tri-state — `Known` /
/// `Absent` / `Unknown`. The doctor must report "could not read settings.json"
/// rather than the misleading "runner tier is local".
///
/// Delegates to [`crate::profiles::read_runner_tier_from_document`], NOT to
/// `read_runner_tier`, and the distinction is deliberate: the doctor asks what
/// the document says, so `QONTINUI_SERVER_MODE` — a property of a running
/// runner's process — is explicitly not consulted. `coord_doctor` can be the
/// standalone bin, whose environment is the operator's shell and says nothing
/// about how any runner was launched; reading it here would report the
/// diagnostician's env as the patient's state. The tier check's own message
/// tells the operator exactly that, and this call is what makes it true.
fn read_runner_tier() -> crate::profiles::TierRead {
    crate::profiles::read_runner_tier_from_document()
}

/// A ONE-SHOT, lazily-taken read of the credential store, shared by the tier
/// check and the `credential_store_readable` check that follows it.
///
/// Shared rather than taken twice for two reasons. The cheap one: the tier
/// check must not add a second keychain round-trip to every doctor run. The
/// load-bearing one: two independent reads could DISAGREE (a store that flips
/// between the checks), and a report whose check 2 says "a bearer is stored"
/// while check 3 says "unreadable" is worse than either answer alone.
///
/// Lazily taken, so the green path pays nothing: a tier that already resolves
/// to `qontinui_account` never asks, and if the chain then stops before check 3
/// the store is never touched.
#[derive(Clone, Default)]
struct SharedTokenProbe(std::rc::Rc<std::cell::OnceCell<crate::secure_storage::StoredTokenRead>>);

impl SharedTokenProbe {
    /// The store read, taken on first call and memoized thereafter.
    fn read(&self) -> &crate::secure_storage::StoredTokenRead {
        self.0
            .get_or_init(|| crate::auth::AuthManager::new().probe_access_token())
    }
}

/// What the credential store said about the access-token slot, projected down
/// to the three facts the tier report needs.
///
/// Module-private, like [`TierEvidence`], [`tier_check_verdict`] and the three
/// `TIER_FIX_*` strings: nothing outside this module consumes any of them, and
/// `mod tests` is a CHILD, so the unit tests reach them regardless. (The
/// `pub`s in `profiles` / `instance_env` are a different case and stay — a
/// second bin genuinely cannot reach the runner bin's module tree.
/// [`CheckOutcome`] also stays `pub`: it appears in [`Check`]'s public field.)
///
/// `Unreadable` is kept distinct from `NoBearer` for the same reason
/// [`crate::profiles::TierRead`] is tri-state: "we could not read it" is not
/// the fact "there is nothing there", and collapsing the two sends the
/// operator to the wrong fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CredentialEvidence {
    /// A non-empty bearer is stored.
    BearerPresent,
    /// The store read cleanly and the slot is empty.
    #[default]
    NoBearer,
    /// The store could not be READ — the bearer is UNKNOWN, not absent.
    Unreadable,
}

impl CredentialEvidence {
    /// Project a raw store read onto this three-way fact.
    fn from_store_read(read: &crate::secure_storage::StoredTokenRead) -> Self {
        use crate::secure_storage::StoredTokenRead;
        match read {
            StoredTokenRead::Present(t) if !t.trim().is_empty() => Self::BearerPresent,
            StoredTokenRead::Present(_) | StoredTokenRead::Absent => Self::NoBearer,
            StoredTokenRead::Unreadable(_) => Self::Unreadable,
        }
    }
}

/// Everything the tier check observed about this box BESIDES the tier field
/// itself — the input that lets it tell *"credentialed but not authorized"*
/// apart from *"no credential"*.
///
/// Every field is already gathered elsewhere in the same chain
/// (`paired_signed_in`, `credential_store_readable`, `tenant_resolvable`), so
/// populating it is a reporting change, not new I/O — the credential read is
/// literally the same one, via [`SharedTokenProbe`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct TierEvidence {
    /// `paired_user.json` carries an account binding
    /// ([`crate::pair::device_is_paired`] — a plain file read, no keychain).
    paired: bool,
    /// The tenant this device is bound to, from `paired_user.json`.
    tenant_id: Option<String>,
    /// What the credential store said about the access-token slot.
    credential: CredentialEvidence,
}

impl TierEvidence {
    /// The observed facts as one clause, in the order an operator reads them:
    /// pairing, tenant, bearer.
    fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(3);
        parts.push(
            if self.paired {
                "paired (paired_user.json carries an account binding)"
            } else {
                "NOT paired (paired_user.json carries no account binding)"
            }
            .to_string(),
        );
        parts.push(match &self.tenant_id {
            Some(t) => format!("bound to tenant {t}"),
            None => "no tenant recorded in paired_user.json".to_string(),
        });
        parts.push(
            match self.credential {
                CredentialEvidence::BearerPresent => "a bearer is stored in the access-token slot",
                CredentialEvidence::NoBearer => "the access-token slot is empty",
                CredentialEvidence::Unreadable => {
                    "the credential store is UNREADABLE, so the bearer is UNKNOWN (not absent)"
                }
            }
            .to_string(),
        );
        parts.join(", ")
    }
}

/// The remediation for a box that is credentialed and only lacks the tier.
const TIER_FIX_UNPIN: &str = "this box is already paired — nothing else is \
missing. Un-pin it so the pairing inference can resolve Tier 2 \u{2014} headless: \
`qontinui_profile tier --clear-choice`, which re-opens the inference when \
settings.json says `local` or carries no tier at all. On a `local_provider` \
document clearing the flag re-opens nothing (only `local` is open to \
inference), so set the tier outright there: `qontinui_profile tier --set \
qontinui_account`. In the app: the SetupWizard's tier step";

/// The remediation for a box that holds no Qontinui account binding at all.
const TIER_FIX_PAIR: &str = "pair this device — headless: `qontinui_profile \
device pair --pair-code <code>`, which promotes the tier as it pairs; in the \
app: Settings \u{2192} Account. A headless runner can also be launched with \
QONTINUI_SERVER_MODE=1, which defaults it to the tier that talks to coord";

/// The remediation when `settings.json` could not be read. NO-DOWNGRADE
/// applies to the FIX as much as to the detail: the blocked report's last line
/// is the one an operator acts on, so it must not tell them to set a tier on
/// top of a file nothing could read.
const TIER_FIX_UNREADABLE: &str = "this is NOT a tier problem — repair the \
unreadable/corrupt settings.json (or the QONTINUI_CONFIG_DIR it resolves to) \
first. Do NOT set a tier on top of a file that could not be read";

/// The tier check's verdict, message AND remediation: PURE over
/// `(tier read x evidence)`.
///
/// Extracted from the check closure so every combination is unit-testable
/// without a live runner, a temp `settings.json`, or process env — the closure
/// above now only *gathers* observations and hands them here.
///
/// # The four shapes it distinguishes
///
/// 1. **Authorized** — resolves to `qontinui_account`. Green.
/// 2. **Credentialed but NOT authorized** — a paired box that still resolves
///    non-account. Since `profiles::read_runner_tier` applies the shared
///    inference, pairing ALONE would have resolved `qontinui_account`; so
///    reaching this arm proves a tier CHOICE is pinning the tier. It does not
///    prove a field: `read_runner_tier_at` reaches `chosen_explicitly` by two
///    routes — the key present and true, or
///    `legacy_tier_choice_is_deducible` back-filling it on a pre-Phase-3
///    document (`profiles.rs`' own documented over-read corner: boot tokenless,
///    then Save a `runner_token` without promoting). On such a box the file
///    carries NO `tier_chosen_explicitly` key, `qontinui_profile tier` prints
///    it as `<absent>`, and naming the field as fact would make this report
///    contradict that one. The message says "a choice, recorded or deduced"
///    instead. Naming the shape is the whole point: "set runner tier to
///    Qontinui account" is the wrong instruction on a headless box, and
///    "you are not signed in" is simply false on this one.
/// 3. **No credential** — non-account AND no account binding at all.
/// 4. **UNKNOWN** — `settings.json` unreadable.
///
/// Each shape carries its OWN fix (see [`CheckOutcome::fix`]), because the
/// blocked report's last line is the one an operator acts on and the four
/// shapes do not share a remedy.
///
/// # NO-DOWNGRADE (non-negotiable)
///
/// The [`crate::profiles::TierRead::Unknown`] arm reports UNKNOWN and nothing
/// else, **even when the box is paired**. Pairing evidence is not a licence to
/// guess at the contents of a file we failed to read: reporting "tier is local"
/// (or "tier is qontinui_account") for what is really an unreadable
/// `settings.json` sends the operator to the wrong remediation entirely. The
/// tri-state stays tri-state, this arm deliberately consumes no evidence, and
/// its fix says so too.
fn tier_check_verdict(tier: &crate::profiles::TierRead, evidence: &TierEvidence) -> CheckOutcome {
    use crate::profiles::{TierRead, QONTINUI_ACCOUNT_TIER};
    let (ok, detail, fix): (bool, String, Option<&'static str>) = match tier {
        TierRead::Known(t) if t.as_str() == QONTINUI_ACCOUNT_TIER => {
            (true, "runner tier is Qontinui account".to_string(), None)
        }
        TierRead::Known(t) if evidence.paired => (
            false,
            format!(
                "runner tier is {t} (not qontinui_account) — but this box IS \
                 credentialed: {}. Credentialed but NOT authorized: the tier \
                 field is the only thing withholding coord access. Pairing \
                 alone infers qontinui_account, so a tier CHOICE is pinning the \
                 tier: either one settings.json records \
                 (tier_chosen_explicitly), one DEDUCED from a legacy document \
                 written before that key existed (tier local plus a \
                 web_integration.runner_token, which no automatic writer could \
                 have produced), or an explicitly-set local_provider.",
                evidence.summary()
            ),
            Some(TIER_FIX_UNPIN),
        ),
        TierRead::Known(t) => (
            false,
            format!(
                "runner tier is {t} (not qontinui_account), and this box holds \
                 no Qontinui account binding: {}. This is 'no credential', not \
                 'credentialed but unauthorized'.",
                evidence.summary()
            ),
            Some(TIER_FIX_PAIR),
        ),
        TierRead::Absent if evidence.paired => (
            false,
            format!(
                "settings.json carries no tier — but this box IS credentialed: \
                 {}. Credentialed but NOT authorized. (Reaching this arm at all \
                 means the pairing inference was closed by an explicit \
                 settings.json::tier_chosen_explicitly.)",
                evidence.summary()
            ),
            Some(TIER_FIX_UNPIN),
        ),
        TierRead::Absent => (
            false,
            format!(
                "settings.json has no tier, and none of the signals this reader \
                 consults infers one — no web_integration.runner_token and no \
                 device pairing (paired_user.json). Observed: {}. \
                 (QONTINUI_SERVER_MODE, QONTINUI_RUNNER_TOKEN and \
                 QONTINUI_RUNNER_TIER are properties of the RUNNING runner's \
                 process, not of the settings document, so this disk read does \
                 not consider any of them.)",
                evidence.summary()
            ),
            Some(TIER_FIX_PAIR),
        ),
        // NO-DOWNGRADE: do not report "runner tier is local" when the real
        // fault is that settings.json could not be read — that sends the
        // operator to the wrong remediation. This arm reads NO evidence: a
        // paired box with an unreadable settings.json is still UNKNOWN, never
        // a guess in either direction. The FIX is redirected too, for exactly
        // the same reason.
        TierRead::Unknown(e) => (
            false,
            format!("runner tier is UNKNOWN — settings.json unreadable ({e})"),
            Some(TIER_FIX_UNREADABLE),
        ),
    };
    CheckOutcome {
        ok,
        detail,
        fix: fix.map(str::to_string),
    }
}

// ---------------------------------------------------------------------------
// Check 4 — tenant resolution. Mirrors
// `device_jwt_refresher::resolve_pair_tenant_id`'s ordered chain without
// depending on the runner binary: OAuth/runner-bearer claim → outgoing
// device-JWT claim (same slot) → machine.json::active_tenant_id.
//
// Phase 8b semantics: this diagnoses the DEVICE-LEVEL DEFAULT binding
// (machine.json's active_tenant_id is default-for-new-sessions, not
// the-only-tenant). A multi-bound device with per-session tenants can be
// healthy for its default while individual sessions run under other
// bindings — the doctor's check is scoped to the default surface.
// ---------------------------------------------------------------------------

fn read_active_tenant_id_from_machine_json() -> Option<uuid::Uuid> {
    let path = dirs::home_dir()?.join(".qontinui").join("machine.json");
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let raw = value.get("active_tenant_id").and_then(|v| v.as_str())?;
    uuid::Uuid::parse_str(raw.trim()).ok()
}

/// `(tenant, source-label)` from the ordered chain, or `None`. `bearer` is the
/// access-token-slot token, which doubles as both the "OAuth claim" and the
/// "outgoing device-JWT" source here (the doctor has one bearer in hand).
fn resolve_tenant_for_doctor(bearer: &str) -> Option<(uuid::Uuid, &'static str)> {
    if let Some(t) = crate::pair::tenant_id_from_oauth_claim(bearer)
        .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok())
    {
        return Some((t, "access-token JWT claim"));
    }
    if let Some(t) = read_active_tenant_id_from_machine_json() {
        return Some((t, "machine.json::active_tenant_id"));
    }
    None
}

// ---------------------------------------------------------------------------
// Check 6 — `.mcp.json` validity. Mirrors `coord_mcp::proxy_request_gate`:
// the config's coord-mcp port must equal the live bound port, its nonce must
// be a registered loopback key (persisted in secure storage), and the stored
// bearer must decode `sub_type == "device"`.
// ---------------------------------------------------------------------------

/// What a session `.mcp.json`'s coord-mcp loopback proxy entry holds, when it
/// holds the proxy shape at all.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProxyConfigFacts {
    /// The loopback port the `url` names.
    port: u16,
    /// The proxy nonce, under either accepted header name.
    nonce: String,
    /// Whether the `headers` map carries a static `Authorization` key AT ALL —
    /// a question about the map's SHAPE, not about the credential in it.
    ///
    /// It is a separate fact from `nonce` precisely because the two can
    /// disagree in the direction that matters: a legacy-only config resolves a
    /// perfectly live `nonce` and still has no `Authorization` key, which is
    /// the shape that leaves the next MCP client escalating a 401 into
    /// OAuth/DCR. Check 6 (credential health) reads `nonce`; check 10 (DCR
    /// safety) reads this.
    has_static_authorization: bool,
    /// Whether the config carries the AGENT principal marker
    /// (`X-Coord-Mcp-Principal: agent`).
    ///
    /// The agent PROXY shape is byte-identical to the device shape apart from
    /// this header — same loopback URL, same 64-hex nonce — so WITHOUT this
    /// fact every device-oriented question check 6 asks lands on an agent
    /// config and answers wrongly in a way that reads as a real fault: the
    /// agent's nonce is deliberately never in the persisted DEVICE set, so
    /// `nonce_is_registered` is false and the doctor reports "nonce is not a
    /// registered proxy key" for a config that is working exactly as designed.
    is_agent_marked: bool,
}

/// Extract the proxy facts from a session `.mcp.json`'s coord-mcp entry, if it
/// is the proxy shape (`url` = `http://127.0.0.1:<port>/coord-mcp` plus a proxy
/// nonce). `None` for a static-bearer (agent-path) config or a non-coord
/// config.
///
/// The nonce is resolved by [`crate::coord_mcp_config::proxy_nonce_from_header_object`],
/// which accepts BOTH the Phase 2 `Authorization: Bearer <nonce>` shape and the
/// legacy `X-Coord-Mcp-Proxy-Key` header — hardcoding either name here would
/// make `coord doctor` blind to one half of the configs on disk, and silently
/// (a `None` reads as "not a proxy config", not as an error).
fn parse_mcp_json_proxy(path: &Path) -> Option<ProxyConfigFacts> {
    let bytes = std::fs::read(path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let server = v.get("mcpServers")?.get("coord-mcp")?;
    let url = server.get("url")?.as_str()?;
    // Expect http://127.0.0.1:<port>/coord-mcp
    let after = url.strip_prefix("http://127.0.0.1:")?;
    let port_str = after.strip_suffix("/coord-mcp")?;
    let port: u16 = port_str.parse().ok()?;
    let nonce = crate::coord_mcp_config::proxy_nonce_from_header_object(server.get("headers")?)?;
    // Asked of the WHOLE document, through the same predicate the boot
    // self-heal's upgrade arm keys on, so the doctor and the repair can never
    // disagree about which files are still escalating.
    let has_static_authorization = crate::coord_mcp_config::config_doc_has_static_authorization(&v);
    // Asked of the WHOLE document through the same predicate the boot
    // reconcile's refusal keys on, for the same reason as above: the doctor and
    // the repair must never disagree about which files are agent-class.
    let is_agent_marked = crate::coord_mcp_config::config_doc_is_agent_marked(&v);
    Some(ProxyConfigFacts {
        port,
        nonce,
        has_static_authorization,
        is_agent_marked,
    })
}

/// Locate the coord-mcp proxy `.mcp.json` the way check 6 does: cwd first, then
/// one level up (a common layout is cwd = sub-repo, config at the repo root).
/// Shared by checks 6 and 10 so they can never report on two different files.
fn find_proxy_mcp_json() -> Option<(PathBuf, ProxyConfigFacts)> {
    let cwd = std::env::current_dir().ok()?;
    let mut candidates = vec![cwd.join(".mcp.json")];
    if let Some(parent) = cwd.parent() {
        candidates.push(parent.join(".mcp.json"));
    }
    candidates
        .into_iter()
        .find_map(|p| parse_mcp_json_proxy(&p).map(|facts| (p, facts)))
}

fn jwt_sub_type(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.trim().split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(parts[1]))
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    json.get("sub_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn mcp_json_check(bound_port: Option<u16>, auth: &crate::auth::AuthManager) -> (bool, String) {
    let Some((path, facts)) = find_proxy_mcp_json() else {
        return (
            false,
            "no coord-mcp proxy .mcp.json found in cwd or repo root".into(),
        );
    };
    let is_agent = facts.is_agent_marked;
    let (cfg_port, nonce) = (facts.port, facts.nonce);

    // Port == bound port (when we know the bound port).
    //
    // Asked of an agent config too, deliberately, and BEFORE the agent
    // not-applicable arm below: "does this file's loopback URL point at the
    // runner that is actually listening" is a property of the proxy shape, not
    // of the principal class. A stale-port agent config is just as dead as a
    // stale-port device one, so exempting it from this check would trade one
    // false report for a missed real one.
    match bound_port {
        Some(bp) if bp != cfg_port => {
            return (
                false,
                format!(
                    "{}: coord-mcp port :{cfg_port} != bound port :{bp}",
                    path.display()
                ),
            );
        }
        None => {
            // The standalone bin can't know the live bound port. Validate the
            // nonce + bearer (still meaningful) but flag the port unverified.
            // Treat as red so the operator runs it from inside the runner for
            // the authoritative answer — honest about uncertainty.
            //
            // For an agent config the nonce half is NOT meaningful — see the
            // not-applicable arm below — so say the port is unverified without
            // attaching a device-set answer that would read as a fault.
            if is_agent {
                return (
                    false,
                    format!(
                        "{}: bound port unknown (run from inside the running runner); \
                         this is an AGENT proxy config, so nonce registration is not a \
                         meaningful question here",
                        path.display()
                    ),
                );
            }
            let nonce_ok = nonce_is_registered(&nonce);
            return (
                false,
                format!(
                    "{}: bound port unknown (run from inside the running runner); \
                     nonce {} registered",
                    path.display(),
                    if nonce_ok { "is" } else { "is NOT" }
                ),
            );
        }
        _ => {}
    }

    // The port is confirmed live. Everything BELOW this point is a device-scoped
    // question that an AGENT proxy config answers to falsely.
    //
    // `nonce_is_registered` reads the persisted DEVICE nonce set; an agent
    // nonce is deliberately never persisted and never restored across a
    // restart, so it is always absent there. The bearer test asks the
    // credential store for a DEVICE JWT, while an agent session's credential is
    // the per-agent JWT the proxy injects from `AGENT_TOKENS` — not in that
    // slot, and not this file's business. Answering anyway reported a healthy
    // agent workdir as "nonce is not a registered proxy key".
    //
    // GREEN with an explicit NOT-APPLICABLE, following check 10's precedent for
    // the analogous case: a warning here would sit permanently on every agent
    // workdir. The detail names what WAS verified and what was not, so the
    // green is not read as a health claim it did not earn.
    if is_agent {
        return (
            true,
            format!(
                "{}: an AGENT proxy config ({}: {}) — device-credential checks NOT \
                 APPLICABLE. Verified: proxy shape, and port :{cfg_port} IS this \
                 runner's bound port. Not verified: nonce registration and bearer \
                 class, both device-scoped (an agent nonce is never in the persisted \
                 device set, and the credential is the per-agent JWT the proxy \
                 injects, not the device access-token). Agent configs are \
                 re-provisioned at agent spawn and are never repaired by the boot \
                 reconcile.",
                path.display(),
                crate::coord_mcp_config::COORD_MCP_PRINCIPAL_HEADER_JSON,
                crate::coord_mcp_config::COORD_MCP_PRINCIPAL_AGENT,
            ),
        );
    }

    // Nonce must be a registered loopback key (persisted store).
    if !nonce_is_registered(&nonce) {
        return (
            false,
            format!("{}: nonce is not a registered proxy key", path.display()),
        );
    }

    // Bearer must decode sub_type == device (mirrors proxy_request_gate).
    // NO-DOWNGRADE (M4): a store READ ERROR is not "the bearer is not a device
    // JWT" — say which one it is.
    let bearer = match auth.get_access_token() {
        Ok(b) => b,
        Err(e) => {
            return (
                false,
                format!(
                    "{}: credential store unreadable ({e}) — the bearer is UNKNOWN, not invalid",
                    path.display()
                ),
            )
        }
    };
    match jwt_sub_type(&bearer).as_deref() {
        Some("device") => (
            true,
            format!(".mcp.json port :{cfg_port}, nonce registered, device bearer"),
        ),
        other => (
            false,
            format!("access-token bearer is not a coord DEVICE JWT (sub_type={other:?})"),
        ),
    }
}

// ---------------------------------------------------------------------------
// Check 10 — the `.mcp.json` header SHAPE (advisory).
//
// Post-merge follow-up to plan
// `2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning`. That plan
// introduced the distinction this check reports and wired the predicate
// (`coord_mcp_config::config_doc_has_static_authorization`) to exactly ONE
// consumer: the boot self-heal's in-place upgrade arm. Nothing an operator can
// run could see it — check 6 reports port, nonce registration and bearer type,
// all of which a legacy-only config passes, so `coord doctor` called the
// DCR-escalating shape fully green. A diagnostic that cannot see the failure
// class its own plan exists to close is the gap this closes.
// ---------------------------------------------------------------------------

/// Report whether the discovered coord-mcp proxy `.mcp.json` still carries only
/// the legacy `X-Coord-Mcp-Proxy-Key` header.
///
/// **Green is not "the credential is healthy"** — that is check 6's question,
/// and the two deliberately disagree on exactly one input: a live, registered
/// nonce carried ONLY in the legacy header is green there and red here. The
/// mechanism is measured (client 2.1.236/2.1.237): an MCP client whose static
/// `headers` map has no `Authorization` key attaches an OAuth provider, so a
/// later stale-key 401 runs RFC 9728 → RFC 8414 discovery, falls through to
/// Dynamic Client Registration at `<origin>/register`, gets this runner's own
/// 404, and writes a durable `mcpOAuth` entry after which it sends the (now
/// healthy) server **zero** requests forever.
///
/// "No proxy config found" is reported as GREEN with an explicit not-applicable
/// detail, not as a failure. This check asks one question about a file, and a
/// runner with no proxy `.mcp.json` in reach has no escalating file — saying
/// so is honest, whereas warning would put a permanent advisory on every
/// agent-path and bare-cwd runner. The absent-config case is check 6's to
/// fail, and it does.
fn mcp_json_dcr_escalation_check() -> (bool, String) {
    let Some((path, facts)) = find_proxy_mcp_json() else {
        return (
            true,
            "no coord-mcp proxy .mcp.json in cwd or repo root — nothing here can \
             escalate (check 6 owns whether one SHOULD be present)"
                .into(),
        );
    };
    if facts.has_static_authorization {
        (
            true,
            format!(
                "{}: carries a static Authorization header, so a stale-key 401 \
                 cannot escalate into OAuth/DCR",
                path.display()
            ),
        )
    } else {
        (
            false,
            format!(
                "{}: carries ONLY the legacy X-Coord-Mcp-Proxy-Key header. The \
                 credential is fine — this is about the header SHAPE: the next MCP \
                 client launched against this file will attach an OAuth provider, so \
                 a future stale-key 401 escalates into discovery -> Dynamic Client \
                 Registration -> this runner's 404 -> a durable client-side poison \
                 entry that silences the server permanently",
                path.display()
            ),
        )
    }
}

/// Whether `nonce` is in the persisted coord-mcp loopback nonce set. Mirrors
/// `coord_mcp::proxy_nonce_is_valid` for the headless/bin path (which has no
/// in-memory `PROXY_NONCES` map — it reads the encrypted store the running
/// runner mirrors to).
fn nonce_is_registered(nonce: &str) -> bool {
    if nonce.is_empty() {
        return false;
    }
    // Membership only — the value shape (widened to carry `terminal_id` by plan
    // 2026-08-20 Phase 4) is deliberately not spelled out here, so this stays
    // correct across further widenings.
    let map = match crate::secure_storage::SecureStorage::new() {
        Ok(s) => s.load_coord_mcp_nonces(),
        Err(_) => return false,
    };
    map.contains_key(nonce)
}

// ---------------------------------------------------------------------------
// Check 7 — coord reachable. One-shot `tools/list` JSON-RPC against the
// configured coord `/mcp` endpoint, with the device bearer if present.
// ---------------------------------------------------------------------------

fn coord_reachable_check() -> (bool, String) {
    // The shared tier-aware policy fn — the doctor and the loopback proxy can
    // never disagree about the upstream OR its source (plan D4).
    let (base, source) = crate::profiles::coord_base_with_source();
    let url = format!("{}/mcp", base.trim_end_matches('/'));
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {}
    });
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (false, format!("could not build HTTP client: {e}")),
    };
    // ⚠ IT MUST PROBE WITH THE CREDENTIAL THE PROXY WOULD SEND — plan
    // `2026-08-31-coord-mcp-credential-selection-by-binding-provenance`
    // Phase 5a. This used to call `AuthManager::new().get_access_token()`, the
    // LEGACY slot, while the coord-mcp proxy injects the PER-TENANT slot. On
    // the operator box those were precisely the healthy credential and the dead
    // one, so this — the doctor's only direct-door probe — would have reported
    // GREEN while every `Pinned` session 401'd. Making the check reachable
    // (below) without fixing the credential would have converted a silent check
    // into a confidently wrong one, which is strictly worse.
    //
    // `device_bearer_for` is the SAME selector the proxy calls, given the same
    // machine pin, so the two can no longer disagree about which slot is under
    // test.
    let pin = crate::tenant_pin::resolve_tenant_pin();
    let tenant = pin.pinned();
    let probed_slot = match (&pin, tenant.as_ref()) {
        (crate::tenant_pin::TenantPin::Unresolvable, _) => "unresolvable-pin",
        (_, Some(_)) => "per-tenant",
        (_, None) => "default/legacy",
    };
    let bearer = crate::auth::device_bearer_for(tenant.as_ref());
    // Say WHICH credential answered, always. A green line that does not name
    // the slot it used is what let this check's divergence from the proxy stay
    // invisible for the life of the defect.
    let creds = match &bearer {
        Some(_) => format!("slot={probed_slot}"),
        None => format!("slot={probed_slot}, NO usable bearer selected — probing unauthenticated"),
    };
    // coord-auth-exempt(diagnostic): the doctor REPORTS on the raw credential
    // chain, so it selects and attaches the bearer itself and says which slot it
    // used. Routing this through `attach_device_auth` would hide the very state
    // the check exists to diagnose, and would bill a diagnostic probe to the
    // data-plane coverage counter.
    //
    // KEEP THIS COMMENT ADJACENT TO THE BUILDER. `tests/coord_auth_pin.rs` scans
    // a window around the write site for the marker, so an annotation that
    // drifts more than a few lines above reads as ABSENT and fails the pin —
    // which is exactly what happened when the credential selection above was
    // inserted between the two.
    let mut rb = client.post(&url).json(&body);
    if let Some(bearer) = bearer.as_deref().filter(|t| !t.trim().is_empty()) {
        rb = rb.header("Authorization", format!("Bearer {bearer}"));
    }
    match rb.send() {
        Ok(resp) if resp.status().is_success() => (
            true,
            format!("coord /mcp tools/list returned 200 ({url}, source={source}, {creds})"),
        ),
        // A 401/403 here is the credential, not reachability — and naming that
        // is the difference between "coord is down" and "this box's slot is
        // dead", which is the misdiagnosis this whole plan was written from.
        Ok(resp) if matches!(resp.status().as_u16(), 401 | 403) => (
            false,
            format!(
                "coord REACHED and REJECTED the credential: HTTP {} ({url}, source={source}, \
                 {creds}) — coord is up; this is a credential fault. GET /coord-mcp/doctor for \
                 the selected slot's kid/exp",
                resp.status()
            ),
        ),
        Ok(resp) => (
            false,
            format!(
                "coord /mcp returned HTTP {} ({url}, source={source}, {creds})",
                resp.status()
            ),
        ),
        Err(e) => (
            false,
            format!("coord /mcp unreachable ({url}, source={source}, {creds}): {e}"),
        ),
    }
}

// ===========================================================================
// Tests — the pure driver (ordering / first-red-stops) is fully exercised
// here without any live runner.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_env::env_lock;

    fn green(name: &'static str) -> Check<'static> {
        Check::new(name, "fix-it", || (true, "green".into()))
    }
    fn red(name: &'static str) -> Check<'static> {
        Check::new(name, "fix-it", || (false, "red".into()))
    }
    fn advisory_red(name: &'static str) -> Check<'static> {
        Check::advisory(name, "fix-it", || (false, "advisory red".into()))
    }

    // Advisory tier (plan 2026-07-28-runner-transcript-persistence-env-leak).
    // The report answers "can this runner set gates?" — a hygiene finding must
    // not make it answer "no".

    #[test]
    fn advisory_red_does_not_stop_the_chain_or_fail_the_report() {
        let report = run_checks(vec![green("a"), advisory_red("hygiene"), green("c")]);
        assert!(
            report.overall_ok,
            "an advisory red must not flip the overall verdict"
        );
        assert_eq!(
            report.checks.len(),
            3,
            "an advisory red must not stop later checks running"
        );
        assert!(!report.checks[1].ok);
        assert!(report.checks[1].advisory);
    }

    #[test]
    fn blocking_red_still_stops_even_after_an_advisory_red() {
        let report = run_checks(vec![advisory_red("hygiene"), red("b"), green("c")]);
        assert!(!report.overall_ok);
        assert_eq!(report.checks.len(), 2, "blocking red still stops the chain");
    }

    #[test]
    fn render_marks_advisory_red_as_warn_and_never_blocked() {
        let out = run_checks(vec![green("a"), advisory_red("hygiene")]).render();
        assert!(
            out.contains("[WARN] hygiene"),
            "advisory red renders WARN:\n{out}"
        );
        assert!(
            !out.contains("[FAIL]"),
            "advisory red must not render FAIL:\n{out}"
        );
        assert!(
            out.contains("OK: this runner can set gates."),
            "verdict must stay OK:\n{out}"
        );
        assert!(
            out.contains("warnings, not blocking: hygiene"),
            "the warning must still be named under the OK line:\n{out}"
        );
        assert!(!out.contains("BLOCKED"), "must never claim BLOCKED:\n{out}");
    }

    #[test]
    fn render_blocked_verdict_names_the_blocking_check_not_the_advisory_one() {
        let out = run_checks(vec![advisory_red("hygiene"), red("real")]).render();
        assert!(
            out.contains("BLOCKED at: real"),
            "the BLOCKED line must name the blocking check:\n{out}"
        );
        assert!(
            !out.contains("BLOCKED at: hygiene"),
            "an advisory red must never be reported as the blocker:\n{out}"
        );
    }

    #[test]
    fn marker_check_is_registered_advisory_in_the_live_chain() {
        // Guards the whole point of the tier: if someone re-registers this
        // check with `Check::new`, an inherited env marker would start
        // reporting "this runner cannot set gates", which is false.
        let _g = env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[
            crate::claude_env::CLAUDECODE_ENV,
            crate::claude_env::CLAUDE_CHILD_SESSION_ENV,
        ]);
        for name in crate::claude_env::INHERITED_SESSION_MARKERS {
            std::env::set_var(name, "1");
        }
        let report = run_checks(vec![Check::from_spec(
            spec("no_inherited_session_markers"),
            inherited_session_markers_check,
        )]);
        assert!(
            report.overall_ok,
            "inherited markers must not block gate access"
        );
        assert!(!report.checks[0].ok, "inherited markers must still be RED");
        assert!(
            report.checks[0]
                .detail
                .contains("CLAUDE_CODE_CHILD_SESSION"),
            "detail names the marker: {}",
            report.checks[0].detail
        );
    }

    #[test]
    fn all_green_runs_every_check_and_passes() {
        let report = run_checks(vec![green("a"), green("b"), green("c")]);
        assert!(report.overall_ok);
        assert_eq!(report.checks.len(), 3);
        assert!(report.checks.iter().all(|c| c.ok));
    }

    #[test]
    fn stops_at_first_red_and_omits_later_checks() {
        let report = run_checks(vec![green("a"), red("b"), green("c")]);
        assert!(!report.overall_ok);
        // Only a (green) + b (red) ran; c never executed.
        assert_eq!(report.checks.len(), 2);
        assert_eq!(report.checks[0].name, "a");
        assert!(report.checks[0].ok);
        assert_eq!(report.checks[1].name, "b");
        assert!(!report.checks[1].ok);
    }

    #[test]
    fn first_check_red_short_circuits_immediately() {
        let report = run_checks(vec![red("a"), green("b")]);
        assert!(!report.overall_ok);
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].name, "a");
    }

    #[test]
    fn checks_run_in_declared_order() {
        // A side-effect log proves the driver preserves order and stops.
        use std::cell::RefCell;
        use std::rc::Rc;
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let mk = |name: &'static str, ok: bool| {
            let log = log.clone();
            Check::new(name, "fix", move || {
                log.borrow_mut().push(name);
                (ok, "d".into())
            })
        };
        let _ = run_checks(vec![
            mk("one", true),
            mk("two", true),
            mk("three", false),
            mk("four", true),
        ]);
        assert_eq!(*log.borrow(), vec!["one", "two", "three"]);
    }

    #[test]
    fn render_marks_pass_fail_and_names_the_block() {
        let report = run_checks(vec![green("alpha"), red("beta"), green("gamma")]);
        let text = report.render();
        assert!(text.contains("[PASS] alpha"));
        assert!(text.contains("[FAIL] beta"));
        // gamma never ran → not in the report.
        assert!(!text.contains("gamma"));
        assert!(text.contains("BLOCKED at: beta"));
        assert!(text.contains("fix: fix-it"));
    }

    #[test]
    fn render_all_green_says_can_set_gates() {
        let report = run_checks(vec![green("a"), green("b")]);
        let text = report.render();
        assert!(text.contains("OK: this runner can set gates."));
    }

    #[test]
    fn empty_check_set_is_vacuously_ok() {
        let report = run_checks(vec![]);
        assert!(report.overall_ok);
        assert!(report.checks.is_empty());
    }

    // ---- pure helpers ----

    #[test]
    fn parse_mcp_json_proxy_reads_port_and_nonce() {
        let dir = std::env::temp_dir().join("qontinui_doctor_test_mcp");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"coord-mcp":{"type":"http",
               "url":"http://127.0.0.1:9877/coord-mcp",
               "headers":{"X-Coord-Mcp-Proxy-Key":"abc123"}}}}"#,
        )
        .unwrap();
        let facts = parse_mcp_json_proxy(&path).expect("parses proxy config");
        let (port, nonce) = (facts.port, facts.nonce);
        assert_eq!(port, 9877);
        assert_eq!(nonce, "abc123");
        let _ = std::fs::remove_file(&path);
    }

    /// Phase 2 (plan 2026-08-20): the runner also emits the nonce as
    /// `Authorization: Bearer <nonce>`. Check 6 must keep recognising a proxy
    /// config in that shape — a miss is SILENT here (a `None` reads as "not a
    /// proxy config"), so `coord doctor` would simply stop reporting on exactly
    /// the configs this phase produces.
    #[test]
    fn parse_mcp_json_proxy_reads_the_authorization_shape_too() {
        let dir = std::env::temp_dir().join("qontinui_doctor_test_mcp_auth");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"coord-mcp":{"type":"http",
               "url":"http://127.0.0.1:9877/coord-mcp",
               "headers":{"Authorization":"Bearer noncefromauth"}}}}"#,
        )
        .unwrap();
        let facts = parse_mcp_json_proxy(&path).expect("parses the new shape");
        assert_eq!(facts.port, 9877);
        assert_eq!(facts.nonce, "noncefromauth");
        assert!(
            facts.has_static_authorization,
            "the shape fact travels alongside the nonce — check 10 reads it"
        );

        // Both present, disagreeing → Authorization wins, matching the
        // request-side resolver the runner authenticates with.
        std::fs::write(
            &path,
            r#"{"mcpServers":{"coord-mcp":{"type":"http",
               "url":"http://127.0.0.1:9877/coord-mcp",
               "headers":{"Authorization":"Bearer fromauth",
                          "X-Coord-Mcp-Proxy-Key":"fromlegacy"}}}}"#,
        )
        .unwrap();
        assert_eq!(parse_mcp_json_proxy(&path).unwrap().nonce, "fromauth");
        let _ = std::fs::remove_file(&path);
    }

    /// The predicate check 10 is built on: a legacy-only config resolves a
    /// perfectly good nonce AND reports the escalating shape. The two facts
    /// must be independent, because that combination — healthy credential,
    /// escalating shape — is the entire population this check exists to find,
    /// and it is exactly the one check 6 passes.
    #[test]
    fn parse_mcp_json_proxy_reports_a_legacy_only_config_as_escalating() {
        let dir = std::env::temp_dir().join("qontinui_doctor_test_mcp_legacy_shape");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"coord-mcp":{"type":"http",
               "url":"http://127.0.0.1:9876/coord-mcp",
               "headers":{"X-Coord-Mcp-Proxy-Key":"livenonce"}}}}"#,
        )
        .unwrap();
        let facts = parse_mcp_json_proxy(&path).expect("a legacy config is still a proxy config");
        assert_eq!(facts.nonce, "livenonce", "the credential reads fine");
        assert!(
            !facts.has_static_authorization,
            "...and the SHAPE is still the DCR-escalating one — that is the \
             whole finding"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The AGENT proxy shape must be DISTINGUISHABLE here, because it is
    /// byte-identical to the device shape apart from one header — and every
    /// device-scoped question check 6 asks answers wrongly on it.
    ///
    /// Without the marker fact, `nonce_is_registered` (the persisted DEVICE
    /// set, which an agent nonce is deliberately never in) reports a perfectly
    /// healthy agent workdir as "nonce is not a registered proxy key".
    #[test]
    fn parse_mcp_json_proxy_distinguishes_an_agent_marked_config() {
        let dir = std::env::temp_dir().join("qontinui_doctor_test_mcp_agent_marker");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".mcp.json");

        // The agent emitter's document: device shape + the principal marker.
        std::fs::write(
            &path,
            r#"{"mcpServers":{"coord-mcp":{"type":"http",
               "url":"http://127.0.0.1:9876/coord-mcp",
               "headers":{"Authorization":"Bearer deadbeef",
                          "X-Coord-Mcp-Proxy-Key":"deadbeef",
                          "X-Coord-Mcp-Principal":"agent"}}}}"#,
        )
        .unwrap();
        let facts = parse_mcp_json_proxy(&path).expect("an agent proxy config IS a proxy config");
        assert!(facts.is_agent_marked, "the marker must reach the facts");
        assert_eq!(
            facts.nonce, "deadbeef",
            "it is still the proxy shape — that is exactly why it needs the marker"
        );

        // The same document WITHOUT the marker is the device shape, and nothing
        // already on disk may change class.
        std::fs::write(
            &path,
            r#"{"mcpServers":{"coord-mcp":{"type":"http",
               "url":"http://127.0.0.1:9876/coord-mcp",
               "headers":{"Authorization":"Bearer deadbeef",
                          "X-Coord-Mcp-Proxy-Key":"deadbeef"}}}}"#,
        )
        .unwrap();
        assert!(
            !parse_mcp_json_proxy(&path).unwrap().is_agent_marked,
            "an unmarked config stays device-class"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Check 10's own arms. Driven through `run_checks` so the advisory
    /// contract is asserted too: a red here must NOT fail the report.
    #[test]
    fn dcr_escalation_check_warns_without_blocking() {
        let escalating = || {
            (
                false,
                "…: carries ONLY the legacy X-Coord-Mcp-Proxy-Key header".to_string(),
            )
        };
        let report = run_checks(vec![Check::from_spec(
            spec("mcp_json_not_dcr_escalating"),
            escalating,
        )]);
        assert!(
            report.overall_ok,
            "a legacy-only config authenticates fine — it must never withhold \
             gate registration"
        );
        assert!(!report.checks[0].ok, "but it must still be RED");
        assert!(report.checks[0].advisory);
    }

    /// The live predicate must never PANIC and must never report an absent
    /// proxy config as a finding: a runner with no `.mcp.json` in reach has no
    /// escalating file, and warning there would put a permanent advisory on
    /// every agent-path and bare-cwd runner. Whether one should be present is
    /// check 6's question, and it fails on it.
    ///
    /// Runs against whatever the real cwd holds, so it asserts the invariant
    /// that holds either way rather than faking a posture.
    #[test]
    fn dcr_escalation_check_is_total_and_green_when_no_proxy_config_exists() {
        let (ok, detail) = mcp_json_dcr_escalation_check();
        assert!(!detail.is_empty(), "every arm must say what it observed");
        if find_proxy_mcp_json().is_none() {
            assert!(ok, "no proxy config in reach ⇒ nothing can escalate");
            assert!(
                detail.contains("nothing here can escalate"),
                "and it must say so rather than implying a healthy file: {detail}"
            );
        }
    }

    #[test]
    fn parse_mcp_json_proxy_none_for_static_bearer_config() {
        let dir = std::env::temp_dir().join("qontinui_doctor_test_mcp2");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".mcp.json");
        // Agent/static-bearer shape has no loopback url → not a proxy config.
        std::fs::write(
            &path,
            r#"{"mcpServers":{"coord-mcp":{"type":"http",
               "url":"https://coord.qontinui.io/mcp",
               "headers":{"Authorization":"Bearer xyz"}}}}"#,
        )
        .unwrap();
        assert!(parse_mcp_json_proxy(&path).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn jwt_sub_type_decodes_device() {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"alg":"EdDSA","typ":"JWT"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"sub":"x","sub_type":"device"}"#);
        let token = format!("{header}.{payload}.sig");
        assert_eq!(jwt_sub_type(&token).as_deref(), Some("device"));
        // An agent JWT decodes to agent, an opaque token to None.
        let agent_payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"sub_type":"agent"}"#);
        let agent = format!("{header}.{agent_payload}.sig");
        assert_eq!(jwt_sub_type(&agent).as_deref(), Some("agent"));
        assert_eq!(jwt_sub_type("opaque-token"), None);
    }

    // ---- Phase 5 — CHECK_SPECS single-source-of-truth + onboarding doc ----

    #[test]
    fn check_specs_has_exactly_ten_entries_eight_blocking_two_advisory() {
        assert_eq!(CHECK_SPECS.len(), 10);
        // The split matters more than the total: the doc prose, the module
        // doc, and `render_onboarding_doc` all describe the two classes, and
        // the blocking count is what "green on all of them ⇒ can set gates"
        // actually refers to.
        //
        // The BLOCKING count is the load-bearing half and it is deliberately
        // unchanged at 8. `mcp_json_not_dcr_escalating` was added as the
        // second ADVISORY check precisely so that "green on all of them ⇒ this
        // runner can set gates" keeps meaning what it meant: a legacy-only
        // `.mcp.json` authenticates fine, so it must not withhold gate
        // registration. A change to the 8 is a change to that sentence; a
        // change to the 2 is not.
        assert_eq!(CHECK_SPECS.iter().filter(|s| !s.advisory).count(), 8);
        assert_eq!(CHECK_SPECS.iter().filter(|s| s.advisory).count(), 2);
    }

    #[test]
    fn onboarding_doc_mentions_every_spec_name_and_fix() {
        let doc = render_onboarding_doc();
        for s in CHECK_SPECS {
            assert!(
                doc.contains(s.name),
                "onboarding doc missing check name {:?}",
                s.name
            );
            assert!(
                doc.contains(s.fix),
                "onboarding doc missing fix {:?} for check {:?}",
                s.fix,
                s.name
            );
            assert!(
                doc.contains(s.title),
                "onboarding doc missing title {:?}",
                s.title
            );
        }
        // The do-not-edit banner must be present so the checked-in file warns
        // editors to regenerate instead.
        assert!(doc.contains("GENERATED FILE"));
        assert!(doc.contains("--onboarding-doc"));
    }

    // ------------------------------------------------------------------
    // Check 2 — the tier verdict. `tier_check_verdict` is PURE, so every
    // combination is driven directly: no temp settings.json, no
    // QONTINUI_CONFIG_DIR / QONTINUI_SECURE_STORAGE_DIR pinning, no env lock.
    // (Plan 2026-08-29-headless-runner-tier-never-reaches-qontinui-account,
    // Phase 4.)
    // ------------------------------------------------------------------

    /// The paired-and-provisioned box the plan was written from: paired,
    /// tenant-bound, holding a bearer.
    fn credentialed() -> TierEvidence {
        TierEvidence {
            paired: true,
            tenant_id: Some("c231d9da-0000-0000-0000-000000000000".to_string()),
            credential: CredentialEvidence::BearerPresent,
        }
    }

    /// A box that has never been bound to a Qontinui account at all.
    fn uncredentialed() -> TierEvidence {
        TierEvidence {
            paired: false,
            tenant_id: None,
            credential: CredentialEvidence::NoBearer,
        }
    }

    fn known(t: &str) -> crate::profiles::TierRead {
        crate::profiles::TierRead::Known(t.to_string())
    }

    #[test]
    fn qontinui_account_tier_passes_cleanly() {
        let out = tier_check_verdict(
            &known(crate::profiles::QONTINUI_ACCOUNT_TIER),
            &TierEvidence::default(),
        );
        assert!(out.ok, "qontinui_account must pass: {}", out.detail);
        assert_eq!(out.detail, "runner tier is Qontinui account");
        assert_eq!(out.fix, None, "a passing check refines no fix");
        // …and the verdict does not change with the evidence: a green tier
        // explains itself, which is why the live closure never touches the
        // credential store on this path.
        let out = tier_check_verdict(
            &known(crate::profiles::QONTINUI_ACCOUNT_TIER),
            &credentialed(),
        );
        assert!(out.ok);
        assert_eq!(out.detail, "runner tier is Qontinui account");
    }

    /// THE reproduction: a box that is paired, tenant-bound and holding a
    /// bearer, whose tier field still reads `local`. The old message named
    /// only the field; the operator's only listed remedy was a button that
    /// does not exist on a headless box.
    #[test]
    fn paired_but_local_reports_credentialed_but_not_authorized() {
        let out = tier_check_verdict(&known("local"), &credentialed());
        let detail = &out.detail;
        assert!(!out.ok, "a non-account tier still blocks");
        assert!(
            detail.contains("Credentialed but NOT authorized"),
            "paired box must be diagnosed as credentialed: {detail}"
        );
        assert!(detail.contains("paired (paired_user.json"), "{detail}");
        assert!(
            detail.contains("bound to tenant c231d9da-0000-0000-0000-000000000000"),
            "{detail}"
        );
        assert!(detail.contains("a bearer is stored"), "{detail}");
        // It names WHAT is pinning the tier, because on a paired box the
        // shared inference would otherwise have resolved qontinui_account.
        assert!(detail.contains("tier_chosen_explicitly"), "{detail}");
        assert!(detail.contains("runner tier is local"), "{detail}");
        // The remediation matches the diagnosis: unpin, do not "pair".
        assert_eq!(out.fix.as_deref(), Some(TIER_FIX_UNPIN));
    }

    /// The other side of the same fork: same tier value, no credential. This
    /// must NOT read as "credentialed but unauthorized" — the fix is entirely
    /// different (pair the device, versus un-pin the tier).
    #[test]
    fn unpaired_local_reports_no_credential_not_credentialed() {
        let out = tier_check_verdict(&known("local"), &uncredentialed());
        let detail = &out.detail;
        assert!(!out.ok);
        assert!(
            !detail.contains("Credentialed but NOT authorized"),
            "an unpaired box must not be reported as credentialed: {detail}"
        );
        assert!(detail.contains("no Qontinui account binding"), "{detail}");
        assert!(detail.contains("NOT paired"), "{detail}");
        assert_eq!(out.fix.as_deref(), Some(TIER_FIX_PAIR));
        assert!(
            TIER_FIX_PAIR.contains("qontinui_profile device pair"),
            "the headless door belongs in the remediation"
        );
    }

    /// The two `local` boxes must be distinguishable from the message AND the
    /// remediation alone — that distinction IS Phase 4a.
    #[test]
    fn the_two_local_boxes_produce_different_diagnoses() {
        let paired = tier_check_verdict(&known("local"), &credentialed());
        let unpaired = tier_check_verdict(&known("local"), &uncredentialed());
        assert_ne!(paired.detail, unpaired.detail);
        assert_ne!(paired.fix, unpaired.fix);
    }

    /// Tier 1 is reachable only as an explicit operator choice, so a paired
    /// box reading `local_provider` is the same "pinned" shape.
    #[test]
    fn paired_local_provider_is_also_credentialed_but_not_authorized() {
        let out = tier_check_verdict(&known("local_provider"), &credentialed());
        assert!(!out.ok);
        assert!(
            out.detail.contains("runner tier is local_provider"),
            "{}",
            out.detail
        );
        assert!(
            out.detail.contains("Credentialed but NOT authorized"),
            "{}",
            out.detail
        );
        assert_eq!(out.fix.as_deref(), Some(TIER_FIX_UNPIN));
    }

    /// NO-DOWNGRADE, the regression guard. An unreadable settings.json is
    /// UNKNOWN — and stays UNKNOWN even on a fully credentialed box. Pairing
    /// evidence is not a licence to guess at a file we failed to read.
    ///
    /// The guard covers the FIX too: the blocked report's last line is the one
    /// an operator acts on, so "set runner tier to Qontinui account" on top of
    /// an unreadable settings.json is the same misdirection in a different
    /// field.
    #[test]
    fn unknown_tier_stays_unknown_even_when_paired() {
        for evidence in [credentialed(), uncredentialed()] {
            let out = tier_check_verdict(
                &crate::profiles::TierRead::Unknown("permission denied".to_string()),
                &evidence,
            );
            assert!(!out.ok);
            assert_eq!(
                out.detail, "runner tier is UNKNOWN — settings.json unreadable (permission denied)",
                "the Unknown arm must report UNKNOWN verbatim and consume no evidence"
            );
            assert!(
                !out.detail.contains("tier is local"),
                "NO-DOWNGRADE violated — an unreadable settings.json was reported as local"
            );
            assert!(
                !out.detail.contains("Credentialed but NOT authorized"),
                "NO-DOWNGRADE violated — pairing evidence leaked into the UNKNOWN arm"
            );
            assert_eq!(
                out.fix.as_deref(),
                Some(TIER_FIX_UNREADABLE),
                "NO-DOWNGRADE violated — the UNKNOWN arm must not carry a set-the-tier fix"
            );
            let fix = out.fix.unwrap();
            assert!(fix.contains("NOT a tier problem"), "{fix}");
            assert!(
                !fix.contains("device pair") && !fix.contains("tier_chosen_explicitly"),
                "the UNKNOWN fix must not prescribe a tier action: {fix}"
            );
        }
    }

    /// Phase 4b: the `Absent` message used to claim `runner_token` was the
    /// only signal that could infer a tier. Phase 3 made pairing a signal too,
    /// so the string must name what the reader actually consults — and must
    /// NOT imply it consults any of the process-scoped inputs
    /// (`QONTINUI_SERVER_MODE`, `QONTINUI_RUNNER_TOKEN`,
    /// `QONTINUI_RUNNER_TIER`), which are properties of the reading process,
    /// not of the document.
    #[test]
    fn absent_tier_names_the_signals_actually_consulted() {
        let out = tier_check_verdict(&crate::profiles::TierRead::Absent, &uncredentialed());
        let detail = &out.detail;
        assert!(!out.ok);
        assert!(detail.contains("settings.json has no tier"), "{detail}");
        assert!(detail.contains("web_integration.runner_token"), "{detail}");
        assert!(detail.contains("paired_user.json"), "{detail}");
        assert!(
            !detail.contains("(and no runner_token to infer one from)"),
            "the stale single-signal claim survived: {detail}"
        );
        // Named only to say they are NOT consulted here — all three of the
        // process-scoped inputs `read_runner_tier` applies and
        // `read_runner_tier_from_document` (this reader) does not.
        for var in [
            "QONTINUI_SERVER_MODE",
            "QONTINUI_RUNNER_TOKEN",
            "QONTINUI_RUNNER_TIER",
        ] {
            assert!(detail.contains(var), "{var} unnamed in: {detail}");
        }
        assert!(
            detail.contains("properties of the RUNNING runner's process"),
            "{detail}"
        );
        assert_eq!(out.fix.as_deref(), Some(TIER_FIX_PAIR));
    }

    #[test]
    fn absent_tier_on_a_paired_box_is_still_credentialed_but_not_authorized() {
        let out = tier_check_verdict(&crate::profiles::TierRead::Absent, &credentialed());
        assert!(!out.ok);
        assert!(
            out.detail.contains("Credentialed but NOT authorized"),
            "{}",
            out.detail
        );
        assert!(out.detail.contains("carries no tier"), "{}", out.detail);
        assert_eq!(out.fix.as_deref(), Some(TIER_FIX_UNPIN));
    }

    /// An unreadable credential store must not be reported as "no bearer" —
    /// the same absence-is-not-unknown rule the tri-state `TierRead` encodes,
    /// applied to the store.
    #[test]
    fn unreadable_credential_store_reads_as_unknown_bearer_not_empty() {
        let ev = TierEvidence {
            paired: true,
            tenant_id: None,
            credential: CredentialEvidence::Unreadable,
        };
        let detail = tier_check_verdict(&known("local"), &ev).detail;
        assert!(
            detail.contains("bearer is UNKNOWN (not absent)"),
            "{detail}"
        );
        assert!(!detail.contains("access-token slot is empty"), "{detail}");
    }

    #[test]
    fn credential_evidence_projects_the_store_read_faithfully() {
        use crate::secure_storage::StoredTokenRead;
        assert_eq!(
            CredentialEvidence::from_store_read(&StoredTokenRead::Present("t".into())),
            CredentialEvidence::BearerPresent
        );
        // A whitespace-only slot is not a bearer.
        assert_eq!(
            CredentialEvidence::from_store_read(&StoredTokenRead::Present("   ".into())),
            CredentialEvidence::NoBearer
        );
        assert_eq!(
            CredentialEvidence::from_store_read(&StoredTokenRead::Absent),
            CredentialEvidence::NoBearer
        );
        assert_eq!(
            CredentialEvidence::from_store_read(&StoredTokenRead::Unreadable("boom".into())),
            CredentialEvidence::Unreadable
        );
    }

    /// A refined fix must actually reach the rendered report — the driver
    /// falls back to the spec string, so a silently-dropped override would
    /// look exactly like the bug this phase fixes.
    #[test]
    fn a_refined_fix_reaches_the_report_and_the_blocked_line() {
        let report = run_checks(vec![Check::new("tier", "static-spec-fix", || {
            CheckOutcome {
                ok: false,
                detail: "d".into(),
                fix: Some("refined-fix".into()),
            }
        })]);
        assert_eq!(report.checks[0].fix, "refined-fix");
        assert!(report.render().contains("BLOCKED at: tier — refined-fix"));
    }

    /// …and a check that refines nothing still reports the spec string, which
    /// is what keeps the generated onboarding doc honest for the other nine.
    #[test]
    fn an_unrefined_check_still_reports_the_spec_fix() {
        let report = run_checks(vec![Check::new("x", "static-spec-fix", || {
            (false, "d".to_string())
        })]);
        assert_eq!(report.checks[0].fix, "static-spec-fix");
    }

    /// Every door the tier fixes name must be one that EXISTS. The unpin
    /// remediation used to say "clear settings.json::tier_chosen_explicitly",
    /// and nothing in the tree could: `set_runner_tier` only ever writes
    /// `true`, and it is a Tauri command behind a WebView a headless box does
    /// not have. So the remediation reduced to hand-editing a runner-managed
    /// JSON file — the same defect class the headless-tier plan exists to
    /// close. `qontinui_profile tier --clear-choice` is the door that was
    /// added; this test is what keeps the text pointing at it.
    #[test]
    fn tier_fix_names_the_doors_a_headless_box_actually_has() {
        let fix = CHECK_SPECS.iter().find(|s| s.name == "tier").unwrap().fix;
        assert!(fix.contains("qontinui_profile device pair"), "{fix}");
        assert!(fix.contains("QONTINUI_SERVER_MODE=1"), "{fix}");
        assert!(
            fix.contains("qontinui_profile tier --clear-choice"),
            "{fix}"
        );

        // The per-diagnosis unpin string names the same door, and does NOT
        // instruct the operator to hand-edit the file.
        assert!(
            TIER_FIX_UNPIN.contains("qontinui_profile tier --clear-choice"),
            "{TIER_FIX_UNPIN}"
        );
        assert!(
            !TIER_FIX_UNPIN.contains("Clear settings.json"),
            "the remediation must not be 'hand-edit a runner-managed file': {TIER_FIX_UNPIN}"
        );
    }

    /// `--clear-choice` is what both remediations name FIRST, and on the
    /// document they name second — an explicitly-set `local_provider` — it
    /// re-opens nothing: `tier_is_open_to_inference` is closed on every
    /// persisted tier but `local`/empty, and `clear_tier_choice_at`
    /// deliberately leaves `tier` alone. So the fix text has to carry the other
    /// door too, or it sends the operator round a loop that cannot terminate.
    #[test]
    fn the_tier_fixes_name_what_a_local_provider_box_actually_needs() {
        let spec_fix = CHECK_SPECS.iter().find(|s| s.name == "tier").unwrap().fix;
        for fix in [spec_fix, TIER_FIX_UNPIN] {
            assert!(
                fix.contains("qontinui_profile tier --clear-choice"),
                "{fix}"
            );
            assert!(
                fix.contains("local_provider"),
                "the fix must say which documents --clear-choice does not \
                 re-open: {fix}"
            );
            assert!(
                fix.contains("--set qontinui_account"),
                "…and name the door that works on them: {fix}"
            );
        }
    }

    /// The pin must be reported as a CHOICE (recorded or deduced), never as a
    /// field the document may not carry.
    ///
    /// `read_runner_tier_at` reaches `chosen_explicitly` by two routes: the key
    /// present and true, or `legacy_tier_choice_is_deducible` back-filling it.
    /// The fixture below is `profiles.rs`' own documented over-read corner —
    /// boot tokenless (the old inference latches `local`), then Save a
    /// `runner_token` without promoting. It carries NO `tier_chosen_explicitly`
    /// key and no human chose anything, yet it reads `Known("local")` and, when
    /// paired, lands on the credentialed-but-not-authorized arm. `qontinui_profile
    /// tier` prints `tier_chosen_explicitly: <absent>` on that same file, so
    /// asserting the field as fact made the branch's two surfaces contradict
    /// each other.
    #[test]
    fn the_pin_is_reported_as_a_choice_recorded_or_deduced_not_as_a_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let doc = r#"{"tier":"local","tier_initialized":true,"web_integration":{"runner_token":"legacy"}}"#;
        std::fs::write(&path, doc).unwrap();

        let raw: serde_json::Value = serde_json::from_str(doc).unwrap();
        assert!(
            raw.get("tier_chosen_explicitly").is_none(),
            "fixture must be a document that does NOT carry the field"
        );

        let tier = crate::profiles::read_runner_tier_at(
            &path,
            /* paired = */ true,
            &crate::profiles::ProcessTierInputs::none(),
        );
        assert_eq!(
            tier,
            crate::profiles::TierRead::Known("local".to_string()),
            "the back-fill must close the inference, or this fixture reaches a \
             different arm and the test proves nothing"
        );

        let out = tier_check_verdict(&tier, &credentialed());
        assert!(
            out.detail.contains("Credentialed but NOT authorized"),
            "{}",
            out.detail
        );
        assert!(
            out.detail.contains("DEDUCED"),
            "the report must allow for a pin deduced from a legacy document: {}",
            out.detail
        );
        assert!(
            !out.detail
                .contains("an explicit operator choice (settings.json::tier_chosen_explicitly)"),
            "the report must not state as fact a field this document does not \
             carry: {}",
            out.detail
        );
    }

    #[test]
    fn diagnose_order_matches_specs() {
        // The live diagnose() chain (with default inputs) must produce checks
        // whose names match CHECK_SPECS in order — adding/removing/reordering
        // a check without updating CHECK_SPECS fails here. We compare only the
        // checks that ran up to (and including) the first red against the
        // corresponding prefix of CHECK_SPECS; in a bare test env check 1
        // (claude_account) typically fails first, but whichever checks DO run
        // must align positionally with the spec table.
        let report = diagnose(&DoctorInputs::default());
        assert!(
            !report.checks.is_empty(),
            "diagnose should always run at least check 1"
        );

        // Advisory checks run even after a blocking red, so the report is no
        // longer one contiguous prefix of CHECK_SPECS. Compare the two classes
        // separately: the BLOCKING results must still be a positional prefix of
        // the blocking specs, and every advisory result must be a spec that is
        // actually flagged advisory.
        let blocking_specs: Vec<&CheckSpec> = CHECK_SPECS.iter().filter(|s| !s.advisory).collect();
        let blocking_ran: Vec<&CheckResult> =
            report.checks.iter().filter(|c| !c.advisory).collect();
        for (i, c) in blocking_ran.iter().enumerate() {
            assert_eq!(
                c.name, blocking_specs[i].name,
                "blocking check at position {i} is {:?} but the blocking spec there is {:?}",
                c.name, blocking_specs[i].name
            );
            // The live `fix` is sourced from the spec → must match too.
            //
            // ONE exception, and it is declared here rather than discovered:
            // `tier` REFINES its fix per diagnosis (see [`CheckOutcome::fix`]),
            // because "settings.json is unreadable" and "the tier is pinned to
            // local" have genuinely different remedies and the blocked line is
            // what an operator acts on. Its refinements are pinned by name
            // above (TIER_FIX_UNPIN / TIER_FIX_PAIR / TIER_FIX_UNREADABLE), so
            // they are still exhaustively asserted — just not against the doc.
            if c.name == "tier" {
                assert!(
                    c.fix == blocking_specs[i].fix
                        || [TIER_FIX_UNPIN, TIER_FIX_PAIR, TIER_FIX_UNREADABLE]
                            .contains(&c.fix.as_str()),
                    "the tier check reported an unregistered fix: {:?}",
                    c.fix
                );
            } else {
                assert_eq!(
                    c.fix, blocking_specs[i].fix,
                    "fix at blocking position {i} drifted from CHECK_SPECS"
                );
            }
        }

        for c in report.checks.iter().filter(|c| c.advisory) {
            let spec = CHECK_SPECS
                .iter()
                .find(|s| s.name == c.name)
                .unwrap_or_else(|| panic!("advisory check {:?} has no CHECK_SPECS entry", c.name));
            assert!(
                spec.advisory,
                "check {:?} ran advisory but CHECK_SPECS says it blocks — \
                 build it with Check::from_spec so the two cannot drift",
                c.name
            );
            assert_eq!(c.fix, spec.fix, "advisory fix drifted from CHECK_SPECS");
        }
    }

    #[test]
    fn advisory_checks_run_even_when_an_earlier_check_is_red() {
        // The regression this guards: with the old first-red-stops driver, an
        // advisory check registered last was unreachable on any runner with a
        // credential problem — i.e. the hygiene detector was disabled on
        // exactly the machines most likely to need it.
        let report = diagnose(&DoctorInputs::default());
        assert!(
            report.checks.iter().any(|c| c.advisory),
            "every advisory check must run regardless of blocking failures; \
             report ran: {:?}",
            report.checks.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
        assert_eq!(
            report.checks.iter().filter(|c| c.advisory).count(),
            CHECK_SPECS.iter().filter(|s| s.advisory).count(),
            "ALL advisory specs must appear in the report"
        );
    }

    /// The advisory roster is pinned by NAME, not by count: an advisory check
    /// does not block gate access, so adding one has to be an explicit edit
    /// here rather than something a new `CheckSpec` can do quietly.
    ///
    /// `mcp_json_not_dcr_escalating` is advisory on purpose. A legacy-only
    /// `.mcp.json` authenticates perfectly — check 6 passes it on every
    /// dimension it measures — so failing the report on it would withhold gate
    /// registration from a runner whose coord access demonstrably works. The
    /// finding is about what the NEXT client will do with a future 401, which
    /// is a warning, not a blocker.
    #[test]
    fn exactly_the_marker_check_is_advisory_in_the_spec_table() {
        let advisory: Vec<&str> = CHECK_SPECS
            .iter()
            .filter(|s| s.advisory)
            .map(|s| s.name)
            .collect();
        assert_eq!(
            advisory,
            vec![
                "no_inherited_session_markers",
                "mcp_json_not_dcr_escalating"
            ],
            "adding an advisory check is a deliberate act — a check that does \
             not block gate access must be justified here"
        );
    }

    /// Phase 5a. `coord_reachable` is the ONLY check that leaves this machine,
    /// so it is the only one whose input is independent of every check before
    /// it. Adding another `always_run` check is a deliberate act: it means
    /// asserting that check cannot be invalidated by an earlier red.
    #[test]
    fn exactly_coord_reachable_always_runs_in_the_spec_table() {
        let always: Vec<&str> = CHECK_SPECS
            .iter()
            .filter(|s| s.always_run)
            .map(|s| s.name)
            .collect();
        assert_eq!(always, vec!["coord_reachable"]);
    }

    /// `always_run` must NOT be a second spelling of `advisory`. The check that
    /// always runs still blocks when it is red — otherwise a runner that cannot
    /// reach coord would start reporting that it can register gates, which is
    /// the regression the first attempt at this fix would have shipped.
    #[test]
    fn always_run_is_not_advisory() {
        let spec = CHECK_SPECS
            .iter()
            .find(|s| s.name == "coord_reachable")
            .expect("coord_reachable is in the spec table");
        assert!(spec.always_run, "it must execute after an earlier red");
        assert!(
            !spec.advisory,
            "and a red in it must still withhold gate access"
        );
    }

    /// The behavioural half, driven through the real `run_checks` with fake
    /// checks: an `always_run` check executes after a blocking red, and its own
    /// red still fails the report.
    #[test]
    fn an_always_run_check_executes_after_a_blocking_red_and_still_blocks() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static RAN: AtomicBool = AtomicBool::new(false);
        RAN.store(false, Ordering::SeqCst);

        let report = run_checks(vec![
            Check::new("blocking_red", "fix", || (false, "red".to_string())),
            Check {
                name: "always",
                fix: "fix",
                run: Box::new(|| {
                    RAN.store(true, Ordering::SeqCst);
                    CheckOutcome::from((false, "also red".to_string()))
                }),
                advisory: false,
                always_run: true,
            },
            Check::new("suppressed", "fix", || -> (bool, String) {
                panic!("an ordinary blocking check must still be skipped")
            }),
        ]);

        assert!(
            RAN.load(Ordering::SeqCst),
            "the always_run check must execute even though a blocking check went red first"
        );
        assert!(
            !report.overall_ok,
            "and its own red must still fail the report"
        );
        assert!(
            report.checks.iter().any(|c| c.name == "always"),
            "its result must reach the report"
        );
        assert!(
            !report.checks.iter().any(|c| c.name == "suppressed"),
            "an ordinary blocking check after a red is still skipped"
        );
    }

    #[test]
    fn device_jwt_live_spec_matches_constants() {
        // Index 5 is the device-JWT-live check, and the shared constants the
        // provisioning gate uses must equal its spec entry.
        assert_eq!(CHECK_SPECS[5].name, "device_jwt_live");
        assert_eq!(CHECK_SPECS[5].name, DEVICE_JWT_LIVE_CHECK_NAME);
        assert_eq!(CHECK_SPECS[5].fix, DEVICE_JWT_LIVE_CHECK_FIX);
    }

    // ---- Phase 5 — provisioning completeness gate (pure predicate) ----

    #[test]
    fn provisioning_ready_when_check5_passes_regardless_of_enforce() {
        // A live device JWT ⇒ Ready, advisory or enforcing.
        assert_eq!(
            provisioning_readiness(true, false),
            ProvisioningReadiness::Ready
        );
        assert_eq!(
            provisioning_readiness(true, true),
            ProvisioningReadiness::Ready
        );
        assert!(provisioning_readiness(true, true).ready_to_report());
        assert!(!provisioning_readiness(true, true).is_incomplete());
    }

    #[test]
    fn provisioning_incomplete_advisory_does_not_withhold_ready() {
        // Check 5 red + advisory ⇒ incomplete, but STILL allowed to report
        // ready (never brick a runner mid-provision).
        let v = provisioning_readiness(false, false);
        assert_eq!(v, ProvisioningReadiness::IncompleteAdvisory);
        assert!(v.is_incomplete(), "must surface as provisioning-incomplete");
        assert!(
            v.ready_to_report(),
            "advisory must NOT withhold ready (no hard block)"
        );
    }

    #[test]
    fn provisioning_incomplete_enforced_withholds_ready() {
        // Check 5 red + enforcing ⇒ incomplete AND ready withheld.
        let v = provisioning_readiness(false, true);
        assert_eq!(v, ProvisioningReadiness::IncompleteEnforced);
        assert!(v.is_incomplete());
        assert!(
            !v.ready_to_report(),
            "enforcing must withhold ready until a device JWT is live"
        );
    }

    #[test]
    fn provisioning_gate_enforce_env_parsing() {
        let _env_lock = env_lock();
        // Save/restore the process-global so this test is self-contained. This
        // is the ONLY test that touches the flag; no sibling reads it, so the
        // save/restore is sufficient (and there's no parallel reader to race).
        let prev = std::env::var(PROVISIONING_GATE_ENFORCE_ENV).ok();

        std::env::remove_var(PROVISIONING_GATE_ENFORCE_ENV);
        assert!(
            !provisioning_gate_enforce_enabled(),
            "unset ⇒ advisory (default)"
        );

        for off in ["0", "", "false", "False", "  "] {
            std::env::set_var(PROVISIONING_GATE_ENFORCE_ENV, off);
            assert!(
                !provisioning_gate_enforce_enabled(),
                "{off:?} must read as advisory"
            );
        }
        for on in ["1", "true", "yes", "enforce"] {
            std::env::set_var(PROVISIONING_GATE_ENFORCE_ENV, on);
            assert!(
                provisioning_gate_enforce_enabled(),
                "{on:?} must read as enforcing"
            );
        }

        match prev {
            Some(p) => std::env::set_var(PROVISIONING_GATE_ENFORCE_ENV, p),
            None => std::env::remove_var(PROVISIONING_GATE_ENFORCE_ENV),
        }
    }
}
