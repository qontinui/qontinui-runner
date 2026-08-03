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

/// A single check: a static name + fix, plus a thunk that runs the predicate
/// and returns `(ok, detail)`. The thunk is `FnOnce` so a check can own
/// resources; the driver only ever calls it once.
pub struct Check<'a> {
    pub name: &'static str,
    pub fix: &'static str,
    pub run: Box<dyn FnOnce() -> (bool, String) + 'a>,
    /// See [`CheckResult::advisory`]. Constructed via [`Check::advisory`].
    pub advisory: bool,
}

impl<'a> Check<'a> {
    /// A BLOCKING check: a failure stops the chain and fails the report.
    pub fn new(
        name: &'static str,
        fix: &'static str,
        run: impl FnOnce() -> (bool, String) + 'a,
    ) -> Self {
        Self {
            name,
            fix,
            run: Box::new(run),
            advisory: false,
        }
    }

    /// An ADVISORY check: a failure is reported as a warning, but the chain
    /// continues and the overall verdict is unaffected. Use for findings that
    /// are worth an operator's attention yet do not stop this runner from
    /// registering gates.
    pub fn advisory(
        name: &'static str,
        fix: &'static str,
        run: impl FnOnce() -> (bool, String) + 'a,
    ) -> Self {
        Self {
            name,
            fix,
            run: Box::new(run),
            advisory: true,
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
    pub fn from_spec(spec: &'static CheckSpec, run: impl FnOnce() -> (bool, String) + 'a) -> Self {
        Self {
            name: spec.name,
            fix: spec.fix,
            run: Box::new(run),
            advisory: spec.advisory,
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
        // A blocking red suppresses only the remaining BLOCKING checks.
        if blocked && !check.advisory {
            continue;
        }
        let name = check.name;
        let fix = check.fix;
        let advisory = check.advisory;
        let (ok, detail) = (check.run)();
        results.push(CheckResult {
            name: name.to_string(),
            ok,
            detail,
            fix: fix.to_string(),
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
// Single source of truth — the static spec for each of the 9 checks.
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
}

/// The 9 checks in `diagnose()` order. THE single source of truth for check
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
    },
    CheckSpec {
        name: "tier",
        title: "Runner tier is Qontinui account",
        verifies: "the runner tier is set to qontinui_account \
                   (settings.json::tier == \"qontinui_account\")",
        fix: "set runner tier to Qontinui account",
        advisory: false,
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
    },
    CheckSpec {
        name: "paired_signed_in",
        title: "Paired and signed in",
        verifies: "paired_user.json is present and a bearer is stored in the \
                   access-token slot",
        fix: "sign in / re-pair",
        advisory: false,
    },
    CheckSpec {
        name: "tenant_resolvable",
        title: "Tenant resolvable",
        verifies: "a tenant_id resolves from the OAuth/runner-bearer claim, the \
                   outgoing device-JWT, or machine.json::active_tenant_id",
        fix: "machine.json missing active_tenant_id",
        advisory: false,
    },
    CheckSpec {
        name: "device_jwt_live",
        title: "Coord device JWT live",
        verifies: "a live coord device JWT is present in the access-token slot \
                   and is not near expiry",
        fix: "kick refresher / re-pair",
        advisory: false,
    },
    CheckSpec {
        name: "mcp_json_valid",
        title: ".mcp.json valid",
        verifies: "the session .mcp.json coord-mcp port equals the bound API \
                   port, its nonce is a registered proxy key, and the bearer is \
                   a coord device JWT",
        fix: "stale config — reprovision",
        advisory: false,
    },
    CheckSpec {
        name: "coord_reachable",
        title: "Coord reachable",
        verifies: "a one-shot tools/list JSON-RPC round-trips 200 against the \
                   configured coord /mcp endpoint",
        fix: "coord unreachable",
        advisory: false,
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
        let suffix = if s.advisory { " — ADVISORY" } else { "" };
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
        }
        out.push_str(&format!("**Fix:** {}\n\n", s.fix));
    }
    out.push_str("---\n\n");
    out.push_str(
        "`coord doctor` runs these checks live. The **blocking** checks stop at the \
         first failure, naming that one link plus its fix; **advisory** checks always \
         run and only ever warn. Run it from **Settings → Account** in the runner app, \
         or headless via the `coord_doctor` bin (`cargo run --bin coord_doctor`). Green \
         on all of them ⇒ this runner can set gates.\n",
    );
    out
}

// ===========================================================================
// Real wiring — the 9 checks, reusing existing predicates / on-disk state.
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
/// 2. Tier — `settings.json::tier == "qontinui_account"`, tri-state so an
///    unreadable settings.json reports as UNKNOWN rather than `local`.
/// 2b. Credential store readable — a store read ERROR is reported as itself, ahead of every bearer-consuming check it would otherwise misdiagnose.
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
        Check::new(s_tier.name, s_tier.fix, || {
            match read_runner_tier() {
                crate::profiles::TierRead::Known(t)
                    if t == crate::profiles::QONTINUI_ACCOUNT_TIER =>
                {
                    (true, "runner tier is Qontinui account".into())
                }
                crate::profiles::TierRead::Known(t) => {
                    (false, format!("runner tier is {t} (not qontinui_account)"))
                }
                crate::profiles::TierRead::Absent => (
                    false,
                    "settings.json has no tier (and no runner_token to infer one from)".into(),
                ),
                // NO-DOWNGRADE: do not report "runner tier is local" when the
                // real fault is that settings.json could not be read — that
                // sends the operator to the wrong remediation.
                crate::profiles::TierRead::Unknown(e) => (
                    false,
                    format!("runner tier is UNKNOWN — settings.json unreadable ({e})"),
                ),
            }
        }),
        // M4 / NO-DOWNGRADE: the credential-store read is its own check, placed
        // AHEAD of every check that consumes a bearer. Previously each consumer
        // did `get_access_token().ok().unwrap_or_default()` (or `.ok()`),
        // feeding an empty bearer downstream — so an unreadable store was
        // misdiagnosed as "not signed in" / "machine.json missing
        // active_tenant_id" / "bearer is not a device JWT". Since `run_checks`
        // stops at the first red, an unreadable store now reports itself
        // instead of blaming the next check in line.
        {
            let auth_ref = crate::auth::AuthManager::new();
            Check::new(s_cred_store.name, s_cred_store.fix, move || {
                let read = auth_ref.probe_access_token();
                match read {
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

/// The persisted runner tier as a tri-state — `Known` / `Absent` / `Unknown`.
/// Delegates to [`crate::profiles::read_runner_tier`]; the doctor must report
/// "could not read settings.json" rather than the misleading "runner tier is
/// local".
fn read_runner_tier() -> crate::profiles::TierRead {
    crate::profiles::read_runner_tier()
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

/// Extract `(port, nonce)` from a session `.mcp.json`'s coord-mcp loopback
/// proxy entry, if it is the proxy shape (`url` = `http://127.0.0.1:<port>/
/// coord-mcp`, header `X-Coord-Mcp-Proxy-Key`). `None` for a static-bearer
/// (agent-path) config or a non-coord config.
fn parse_mcp_json_proxy(path: &Path) -> Option<(u16, String)> {
    let bytes = std::fs::read(path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let server = v.get("mcpServers")?.get("coord-mcp")?;
    let url = server.get("url")?.as_str()?;
    // Expect http://127.0.0.1:<port>/coord-mcp
    let after = url.strip_prefix("http://127.0.0.1:")?;
    let port_str = after.strip_suffix("/coord-mcp")?;
    let port: u16 = port_str.parse().ok()?;
    let nonce = server
        .get("headers")?
        .get("X-Coord-Mcp-Proxy-Key")?
        .as_str()?
        .to_string();
    Some((port, nonce))
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
    // Look for a coord-mcp proxy `.mcp.json` in the cwd, then the repo root.
    let candidates: Vec<PathBuf> = {
        let mut v = Vec::new();
        if let Ok(cwd) = std::env::current_dir() {
            v.push(cwd.join(".mcp.json"));
            // one level up (common: cwd is a sub-repo, config at repo root)
            if let Some(parent) = cwd.parent() {
                v.push(parent.join(".mcp.json"));
            }
        }
        v
    };
    let found = candidates
        .into_iter()
        .find_map(|p| parse_mcp_json_proxy(&p).map(|pr| (p, pr)));
    let Some((path, (cfg_port, nonce))) = found else {
        return (
            false,
            "no coord-mcp proxy .mcp.json found in cwd or repo root".into(),
        );
    };

    // Port == bound port (when we know the bound port).
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

/// Whether `nonce` is in the persisted coord-mcp loopback nonce set. Mirrors
/// `coord_mcp::proxy_nonce_is_valid` for the headless/bin path (which has no
/// in-memory `PROXY_NONCES` map — it reads the encrypted store the running
/// runner mirrors to).
fn nonce_is_registered(nonce: &str) -> bool {
    if nonce.is_empty() {
        return false;
    }
    let map: HashMap<String, String> = match crate::secure_storage::SecureStorage::new() {
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
    let mut rb = client.post(&url).json(&body);
    if let Some(bearer) = crate::auth::AuthManager::new()
        .get_access_token()
        .ok()
        .filter(|t| !t.trim().is_empty())
    {
        rb = rb.header("Authorization", format!("Bearer {bearer}"));
    }
    match rb.send() {
        Ok(resp) if resp.status().is_success() => (
            true,
            format!("coord /mcp tools/list returned 200 ({url}, source={source})"),
        ),
        Ok(resp) => (
            false,
            format!(
                "coord /mcp returned HTTP {} ({url}, source={source})",
                resp.status()
            ),
        ),
        Err(e) => (
            false,
            format!("coord /mcp unreachable ({url}, source={source}): {e}"),
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
        let (port, nonce) = parse_mcp_json_proxy(&path).expect("parses proxy config");
        assert_eq!(port, 9877);
        assert_eq!(nonce, "abc123");
        let _ = std::fs::remove_file(&path);
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
    fn check_specs_has_exactly_nine_entries_eight_blocking_one_advisory() {
        assert_eq!(CHECK_SPECS.len(), 9);
        // The split matters more than the total: the doc prose, the module
        // doc, and `render_onboarding_doc` all describe the two classes, and
        // the blocking count is what "green on all of them ⇒ can set gates"
        // actually refers to.
        assert_eq!(CHECK_SPECS.iter().filter(|s| !s.advisory).count(), 8);
        assert_eq!(CHECK_SPECS.iter().filter(|s| s.advisory).count(), 1);
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
            assert_eq!(
                c.fix, blocking_specs[i].fix,
                "fix at blocking position {i} drifted from CHECK_SPECS"
            );
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

    #[test]
    fn exactly_the_marker_check_is_advisory_in_the_spec_table() {
        let advisory: Vec<&str> = CHECK_SPECS
            .iter()
            .filter(|s| s.advisory)
            .map(|s| s.name)
            .collect();
        assert_eq!(
            advisory,
            vec!["no_inherited_session_markers"],
            "adding an advisory check is a deliberate act — a check that does \
             not block gate access must be justified here"
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
