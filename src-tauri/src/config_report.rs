//! `config report` — the effective configuration of this runner, layer by
//! layer, with per-value provenance
//! (plan `2026-08-20-effective-config-provenance-and-env-generation`,
//! Phases 1-4).
//!
//! # The report ASKS; it never re-derives
//!
//! Every `source` string in this report is returned BY the resolver that
//! produced the value, in the same call. Nothing here re-implements a
//! precedence order, and nothing here may: a second copy of a resolution rule
//! compiles, agrees with the real one the day it is written, and silently
//! starts lying the first time the real one changes — reproducing, inside the
//! diagnostic, exactly the class of defect the diagnostic exists to expose.
//! That is the plan's stated dominant correctness risk, and Phase 2 is the
//! phase that closed it: `api_config::resolve_api_base_url`,
//! `ai_provider::get_effective_config_dir`, `settings::resolve_config_dir` and
//! `profiles::settings_json_path` all grew `(value, source)` return shapes so
//! that asking is possible at all.
//!
//! Fifteen independent layers feed a runner session, every one of them resolved
//! by a separate hand-rolled resolver, and **nothing anywhere prints them**.
//! `doctor/` is process health; `coord_doctor` answers exactly one question
//! ("can this runner set gates?") and attributes exactly one value. So the
//! operator question "where did this value come from?" has, today, no answer
//! short of reading the source. This module is that answer.
//!
//! # The three things this module refuses to do
//!
//! 1. **It never renders an unreadable layer as its default.** A layer that
//!    could not be read is [`LayerReading::Unknown`] and renders as `UNKNOWN`,
//!    all the way to the output. Reporting "the value is X" when the truth is
//!    "X is what the code would have fallen back to" is worse than printing
//!    nothing — it sends the reader to the wrong remediation. This is the same
//!    discipline the codebase already encodes in `profiles::TierRead` and
//!    `settings::SettingsProvenance::Unreadable`.
//! 2. **It never prints the credential store.** Layer 15 (the OS keyring /
//!    `SecureStorage`) is permanently [`LayerReading::Withheld`] — a row, so a
//!    reader can see it was considered, but structurally incapable of carrying
//!    a value. Withholding at the MODEL layer cannot leak; pattern-matching
//!    rendered text can, and `session/redact.rs` says so about itself in its
//!    own header ("defense in depth, NOT a security boundary").
//! 3. **It never omits a row it could not resolve.** A silently-missing layer
//!    reads as "not applicable" when the truth is "not observable from here".
//!    See the crate-split note below — that honesty is the whole reason the
//!    headless bin is trustworthy.
//!
//! # Why this lives in the lib crate, and what that costs
//!
//! Same reason as [`crate::coord_doctor`]: two consumers (the headless
//! `config_report` bin and the in-app Tauri command in the runner binary), and
//! a `src/bin/*` crate cannot import the runner binary's module tree.
//!
//! The cost is real and is the load-bearing fact of this phase. **Ten of the
//! fifteen layers live in BIN-only modules** (`settings`, `launch_env`,
//! `api_config`, `mcp_api`, `terminal`, `coord_mcp`, `session`, `ai_provider`,
//! `build_drift`, `config_facade`), which a lib module cannot call. So the
//! split is:
//!
//! - `[lib]` (here) — the [`LAYER_SPECS`] data table, the [`LayerReading`]
//!   tri-state, the [`build_report`] driver, the byte-stable [`ConfigReport::render`]
//!   formatter, and the resolvers for the lib-side layers.
//! - `[bin]` — every bin-only layer's resolver, injected as DATA through
//!   [`ConfigReportInputs`], exactly the way `coord_doctor_cmd.rs` injects
//!   `coord_mcp::resolve_bound_api_port()` into [`crate::coord_doctor::DoctorInputs`].
//!
//! The headless bin injects nothing, so every bin-only layer reports
//! `Unknown { reason: "not observable from the headless bin …" }`. That is
//! honest by construction rather than by anyone remembering to be honest.
//!
//! # Phase 3: the env GENERATIONS section
//!
//! Layers 6, 7 and 8 are all "the environment", and reporting them as three
//! independent one-line values would miss the entire point: what an operator
//! needs is the RELATIONSHIP between them. So alongside the three rows this
//! report carries a section — [`crate::env_generations`] — that puts the same
//! variable side by side at three ages and names the differences.
//!
//! That section is where the plan's D1 becomes diagnosable. An operator sets an
//! env var and nothing happens, because the value is three restarts deep: the
//! runner's own process env is frozen (needs a runner restart), an open
//! terminal's env is frozen (needs a new terminal), and a Claude Bash-tool
//! grandchild inherits its terminal's frozen copy. `terminal/session.rs` states
//! that in prose; the section states it in output.
//!
//! It is `Option`al on both [`ConfigReportInputs`] and [`ConfigReport`], and an
//! absent section is PRINTED as absent — see [`ConfigReport::render`].
//!
//! # Phase 4: the carriers, the dial, and the two clocks
//!
//! Phase 4 lands the last five bin-side layers — 9, 10, 12, 13 and 14 — and the
//! two of them that are TIME-VARYING are the reason `captured_at` exists.
//!
//! Layers 9 (`prompt_library`) and 10 (`mcp::fleet_policy_poller`) are served
//! by coord and cached in process globals that a background loop refreshes.
//! Neither needs a restart to change value, so a reading of either is a
//! statement about a moment twice over: **when the report looked**, and **when
//! the cache it looked at was last refreshed**. Those are different facts, and
//! conflating them is not a rounding error — it would make every reading look
//! perfectly fresh regardless of whether coord had been unreachable for an
//! hour. So the report carries both where the cache exposes its own stamp
//! (layer 9's TTL age; the session-briefing cache's `fetched_at` +
//! `provenance`) and says UNKNOWN for the sub-fact where it does not (the three
//! bare `RwLock<T>` dials, which hold a value and nothing else). Substituting
//! the read time for a missing refresh time is the same class of lie as
//! rendering an unread layer as its default.
//!
//! Layers 12, 13 and 14 are the CARRIERS — configuration that reaches a session
//! by argv or by a file the runner writes, rather than by env or by the
//! session's own discovery. All three are credential-adjacent, and all three
//! report identity (path, existence, ownership, verdict) rather than content.
//! Layer 14 in particular surfaces the `coord_mcp_safe_to_write` DECISION,
//! which before this phase existed only as a `warn!` line — so a secondary
//! runner correctly refusing to rewrite the shared root `.mcp.json` looked
//! exactly like one that had never tried.
//!
//! # Phase 5: layer 1, and the attribution this report deliberately does NOT do
//!
//! Phase 5 lands the last row, `settings_struct`, and the interesting thing
//! about it is what it declines to answer.
//!
//! The plan's own Phase 5 sketch proposed a hand-written `serde::Deserialize`
//! over `Settings` recording, per field, whether the value came from disk or
//! from that field's `#[serde(default = "…")]`. That was already rejected once
//! in the plan's Design decision A and it is rejected here for the same reason,
//! now with four phases of evidence behind it: it is ~90 fields of new
//! hand-maintained deserialization on the ONE code path every tier, identity and
//! credential decision in this runner already routes through, and each of the
//! three confusion cases that motivated the plan is answered by something that
//! already shipped — D1 (an env var that "did nothing") by the env-generation
//! section, D2 (a runner silently declining to rewrite `.mcp.json`) by layer 14,
//! D3 (which endpoint/config dir won) by layers 2 and 5. None of them turns on
//! knowing whether `app_mode` was present in the file.
//!
//! What the layer DOES answer is the question underneath all of those: **are
//! these the user's real persisted values, or a placeholder?** That is
//! `settings::SettingsProvenance` — a tri-state (`Loaded` / `FreshInstall` /
//! `Unreadable`) the codebase already maintains, already documents, and already
//! gates its write path on, returned by `settings::read_settings_from_disk()`
//! — the NON-MUTATING reader `load_settings_full` itself starts from, chosen
//! because `load_settings_full` writes (a `claude-accounts.json` migration, a
//! `local_user_id`/tier persist into the operator's real settings.json, an OS
//! keyring read) and a diagnostic must not. The
//! `Unreadable` arm is the one that matters: the `Settings` accompanying it is a
//! DEFAULT PLACEHOLDER whose identity-bearing fields are not the user's values,
//! which is precisely the "rendered a fallback as if it had been read" failure
//! this whole report exists to refuse — and until this row existed the report
//! could not say it had happened.
//!
//! So the row is `Resolved` at a coarser grain than first proposed, and it says
//! so **in the output**, not only here. A reader who mistook `loaded` for
//! per-field attribution would be worse off than one with no row at all, and the
//! same discipline that makes this report distinguish `Unknown` from `Withheld`
//! obliges it to distinguish "we resolved this coarsely" from "we resolved
//! this". A decision recorded only in a commit message is indistinguishable,
//! six months later, from an omission.
//!
//! # Every reading is time-stamped, not just the env ones
//!
//! `captured_at` is on **every** [`LayerReading`], including `Unknown` and
//! `Withheld`. This is not future-proofing: layer 10
//! (`mcp/fleet_policy_poller`) refreshes three process-global caches on a poll
//! interval with **no restart of anything**, so a report without a per-layer
//! capture time is already wrong for a layer that exists today. A reading is a
//! statement about a moment, and a report that cannot say which moment cannot
//! answer "which generation am I looking at?" — the question the whole plan
//! exists to make answerable.

use chrono::{DateTime, SecondsFormat, Utc};

// ===========================================================================
// The layer inventory — a static data table, in the CHECK_SPECS style.
//
// This table is the SINGLE source of truth for: the layer list, its order, the
// rendered report, and the generated markdown doc (`render_layer_doc`). Adding
// a layer means adding a row here and nothing else structural.
// ===========================================================================

/// Which crate — and therefore which observer — can actually resolve a layer.
///
/// This is data, not a comment, because it is what lets [`build_report`]
/// produce a truthful `Unknown` reason without every caller restating the
/// crate-boundary rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerSide {
    /// Resolvable from `qontinui_runner_lib` — so the headless bin and the
    /// in-app command agree exactly.
    Lib,
    /// Lives in a BIN-only module of the runner binary. The lib driver can only
    /// see it if the runner binary injects the reading via
    /// [`ConfigReportInputs`].
    Bin,
    /// Owned by a DIFFERENT executable entirely (the supervisor process; the
    /// `qontinui-shim` bin). Not merely un-injected — the value is not in this
    /// process's address space at all, and reporting it means asking that
    /// binary.
    ExternalBinary,
}

impl LayerSide {
    /// Stable wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            LayerSide::Lib => "lib",
            LayerSide::Bin => "bin",
            LayerSide::ExternalBinary => "external-binary",
        }
    }
}

/// Whether a layer has a live resolver yet.
///
/// A pending layer still gets a ROW — it resolves to `Unknown` naming the phase
/// that lands it. That is the honest shape: "we know this layer exists and we
/// are not reading it yet" is information, and a missing row is not.
///
/// A row moves to `Resolved` only when a REAL resolver can hand back both the
/// value and its arm. Where that needed the underlying resolver to change
/// shape, the resolver changed (Phase 2 did this for four of them); where it
/// would have needed the report to work the precedence out for itself, the row
/// stays `Pending` instead. Forcing a row `Resolved` by re-deriving is the one
/// way this table can make the report worse than having no report.
///
/// **As of Phase 5 every row is `Resolved` and [`Pending`](Self::Pending) has
/// zero instances** — asserted by `config_report_no_layer_is_pending`, so a new
/// layer cannot be added and then quietly forgotten. The variant stays because
/// it is the honest shape for the phase that adds the SIXTEENTH layer, and
/// `config_report_pending_machinery_still_names_its_phase` keeps it from rotting
/// into something that no longer works when that phase arrives. Deleting a
/// mechanism because it currently has no instances is how the next author ends
/// up with a silently-omitted row instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerStatus {
    /// A live resolver exists (lib-side here, or bin-side via injection).
    Resolved,
    /// No resolver yet; the plan phase that lands it. Currently unused — see the
    /// type's docs.
    Pending(u8),
}

/// The static spec for one configuration layer. `name` is the stable machine
/// identifier (it appears in the JSON and in the text report); `title` and
/// `describes` are the human-readable halves, shared by the report and the
/// generated doc so the two cannot drift; `anchor` names the symbol that
/// actually performs the resolution, so a reader can go straight to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct LayerSpec {
    /// Stable machine name (matches the rendered row label and the JSON key).
    pub name: &'static str,
    /// Short human title.
    pub title: &'static str,
    /// One-line "what this layer contributes to the effective configuration".
    pub describes: &'static str,
    /// The SYMBOL that resolves this layer. Deliberately not a line number —
    /// this plan's own anchors drifted 66 commits between vet and implement.
    pub anchor: &'static str,
    /// Which crate/binary owns the resolution. Drives the `Unknown` reason a
    /// non-owning observer reports.
    pub side: LayerSide,
    /// Whether a resolver exists yet.
    pub status: LayerStatus,
}

/// The fifteen layers that feed a runner session, in report order.
///
/// The count is the point: the plan's original draft claimed fifteen and
/// enumerated thirteen. Rows 4 and 15 are the two that were missing, and layer
/// 15 is the one that must never be printed.
pub const LAYER_SPECS: &[LayerSpec] = &[
    LayerSpec {
        name: "settings_struct",
        title: "Settings struct (~90 fields) — WHOLE-FILE provenance",
        describes: "whether the ~90-field settings document this runner is acting \
                    on is the user's real persisted state (`loaded`), the \
                    compiled-in defaults of a genuine first run (`fresh_install`), \
                    or a DEFAULT PLACEHOLDER standing in for a file that exists \
                    and could not be read (`unreadable` — the variant where \
                    `tier`, `web_integration.runner_token`, `setup_completed`, \
                    `qontinui_user_id` and the sync toggles are not the user's \
                    values at all). PER-FIELD attribution — \"did `app_mode` come from disk or \
                    from `default_app_mode()`?\" — was CONSIDERED AND \
                    DELIBERATELY NOT BUILT. It would take a hand-written \
                    `serde::Deserialize` over ~90 fields, each recording whether \
                    its `#[serde(default = \"…\")]` fired: a large new \
                    correctness surface on the one code path every identity, \
                    tier and credential decision in the runner already depends \
                    on, in exchange for a question no reported confusion case \
                    turns on. The three that motivated this report are answered \
                    elsewhere — env-var staleness by the env-generation section, \
                    the shared `.mcp.json` write refusal by layer 14, and the \
                    endpoint/config-dir precedence by layers 2 and 5. This row \
                    is therefore resolved at a deliberately coarser grain than \
                    the plan first proposed, and says so in its own output",
        anchor: "settings::read_settings_from_disk → settings::SettingsProvenance",
        side: LayerSide::Bin,
        status: LayerStatus::Resolved,
    },
    LayerSpec {
        name: "config_dir",
        title: "Config-dir resolution",
        describes: "which directory settings.json is read from and written to, \
                    overridable by the `QONTINUI_CONFIG_DIR` env var; RESOLVED and \
                    statted, never created",
        anchor: "settings::resolve_config_dir",
        side: LayerSide::Bin,
        status: LayerStatus::Resolved,
    },
    LayerSpec {
        name: "settings_json_second_reader",
        title: "Second independent reader of settings.json",
        describes: "the lib-side reader of the SAME settings.json the bin reads — \
                    a second path to the same file with its own error handling, \
                    so the two can disagree about whether it was readable",
        anchor: "profiles::settings_json_path",
        side: LayerSide::Lib,
        status: LayerStatus::Resolved,
    },
    LayerSpec {
        name: "profiles_coord_base",
        title: "profiles.json active profile → coord base",
        describes: "the effective coordinator base URL and which of the five \
                    resolution arms produced it (env `COORD_HTTP_URL`, the \
                    `QONTINUI_ENV`-selected profile's `coord_url`, the tier \
                    default, the unknown-tier production default, or the \
                    dev-localhost guess)",
        anchor: "profiles::coord_base_policy",
        side: LayerSide::Lib,
        status: LayerStatus::Resolved,
    },
    LayerSpec {
        name: "api_endpoint_registry",
        title: "Endpoint registry — qontinui-web backend base URL",
        describes: "the single source of truth for the web-backend base across \
                    every runner subsystem, resolved over a documented four-rung \
                    order (env `QONTINUI_WEB_BACKEND_URL`, env `QONTINUI_API_URL`, \
                    the persisted paired backend, the build default)",
        anchor: "api_config::resolve_api_base_url",
        side: LayerSide::Bin,
        status: LayerStatus::Resolved,
    },
    LayerSpec {
        name: "launch_env_snapshot",
        title: "Launch-env snapshot",
        describes: "the runner's own environment, read ONCE in `main` — the first \
                    of the generations that makes an env var three restarts deep",
        anchor: "launch_env::RunnerLaunchEnv::read",
        side: LayerSide::Bin,
        status: LayerStatus::Resolved,
    },
    LayerSpec {
        name: "adhoc_env_reads",
        title: "Ad-hoc runtime env reads",
        describes: "the `std::env::var` calls scattered through the runtime, which \
                    see the CURRENT process env rather than the launch snapshot — \
                    so two values sourced from \"the environment\" can disagree",
        anchor: "std::env::var call sites (runner binary)",
        side: LayerSide::Bin,
        status: LayerStatus::Resolved,
    },
    LayerSpec {
        name: "supervisor_injected_env",
        title: "Supervisor-injected env",
        describes: "the environment the dev supervisor injects when it spawns a \
                    runner — not present in this process's own configuration at \
                    all, only in the env it was handed",
        anchor: "qontinui-supervisor process::manager (spawn env)",
        side: LayerSide::ExternalBinary,
        status: LayerStatus::Resolved,
    },
    LayerSpec {
        name: "coord_prompt_documents",
        title: "Coord-served policy / prompt documents",
        describes: "configuration served by coord rather than held locally — \
                    fetched over the network, so its content is a function of \
                    when it was fetched",
        anchor: "prompt_library::cache_health",
        side: LayerSide::Bin,
        status: LayerStatus::Resolved,
    },
    LayerSpec {
        name: "fleet_policy_dial",
        title: "Fleet policy dial (TIME-VARYING)",
        describes: "three process-global caches refreshed by one supervised poll \
                    loop — this layer's value can change with NO restart of \
                    anything, which is why every reading in this report carries a \
                    capture time",
        anchor: "mcp::fleet_policy_poller::dial_snapshot",
        side: LayerSide::Bin,
        status: LayerStatus::Resolved,
    },
    LayerSpec {
        name: "claude_config_dir",
        title: "Per-account CLAUDE_CONFIG_DIR selection",
        describes: "which Claude account config directory a session resolves to — \
                    the per-account selection that decides whose credentials and \
                    settings a spawned session uses",
        anchor: "ai_provider::config::get_effective_config_dir",
        side: LayerSide::Bin,
        status: LayerStatus::Resolved,
    },
    LayerSpec {
        name: "claude_settings_carrier",
        title: "`--settings` carrier the runner materializes",
        describes: "the settings file the runner writes to disk and hands to \
                    Claude Code on the command line — configuration that reaches \
                    the session by argv, not by env or by file discovery",
        anchor: "session::claude_hook::settings_path",
        side: LayerSide::Bin,
        status: LayerStatus::Resolved,
    },
    LayerSpec {
        name: "mcp_config_carrier",
        title: "`--mcp-config` carrier",
        describes: "the MCP config argv the identity shim passes through — built \
                    in a SEPARATE binary, so what the session received is not \
                    observable from the runner's own state",
        anchor: "bin/qontinui_shim::identity_mcp_config_args",
        side: LayerSide::ExternalBinary,
        status: LayerStatus::Resolved,
    },
    LayerSpec {
        name: "mcp_json",
        title: "`.mcp.json`",
        describes: "the on-disk MCP server manifest the runner writes (coord-mcp \
                    port, proxy nonce, bearer) — shared state that an ephemeral \
                    runner can rewrite underneath a live session",
        anchor: "coord_mcp::mcp_json_report",
        side: LayerSide::Bin,
        status: LayerStatus::Resolved,
    },
    LayerSpec {
        name: "secure_storage_keyring",
        title: "OS keyring / SecureStorage — WITHHELD",
        describes: "the credential store itself (OS keychain or the encrypted \
                    on-disk slot under `QONTINUI_SECURE_STORAGE_DIR`). It is in \
                    this inventory so the report DECLARES that it was considered; \
                    it is `Withheld` so it can never carry a value into the \
                    renderer",
        anchor: "secure_storage::SecureStorage",
        side: LayerSide::Lib,
        status: LayerStatus::Resolved,
    },
];

/// Look up a [`LayerSpec`] by name. Panics on an unknown name — every caller
/// passes a literal that IS in [`LAYER_SPECS`], and a miss is a programming
/// error that must fail loudly rather than silently drop a layer.
fn layer(name: &'static str) -> &'static LayerSpec {
    LAYER_SPECS
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no LAYER_SPECS entry for layer {name:?}"))
}

// ===========================================================================
// The reading — the tri-state that carries this module's whole UX commitment.
// ===========================================================================

/// What one layer read out to, at one moment.
///
/// The tri-state is the point of the type. Collapsing `Unknown` into a default
/// value, or dropping a `Withheld` row, would each defeat the report:
///
/// - [`Known`](Self::Known) — the layer was read; here is the value AND the arm
///   that produced it.
/// - [`Unknown`](Self::Unknown) — the layer could NOT be read here, and the
///   reason says why. **Never rendered as a value.** There is deliberately no
///   `value` field on this variant, so "render the default instead" is not a
///   mistake the renderer is able to make.
/// - [`Withheld`](Self::Withheld) — the layer was resolved and is deliberately
///   not printed (it is credential-bearing). Also has no `value` field: a
///   withheld reading structurally cannot leak, whereas a redaction pass over
///   rendered text can miss a shape nobody anticipated.
///
/// Every variant carries `captured_at` — see the module docs on layer 10.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LayerReading {
    Known {
        value: String,
        /// The winning arm, in the resolver's own vocabulary (e.g.
        /// `CoordBaseSource::as_str()`). Not a free-text sentence — it is meant
        /// to be greppable and comparable across machines.
        source: String,
        captured_at: DateTime<Utc>,
    },
    Unknown {
        reason: String,
        captured_at: DateTime<Utc>,
    },
    Withheld {
        reason: String,
        captured_at: DateTime<Utc>,
    },
}

impl LayerReading {
    /// A read value plus its winning arm, stamped at `captured_at`. The
    /// explicit stamp (rather than an internal `Utc::now()`) keeps the whole
    /// module pure over time, so the renderer tests can assert byte-stable
    /// output against a literal.
    pub fn known(
        value: impl Into<String>,
        source: impl Into<String>,
        captured_at: DateTime<Utc>,
    ) -> Self {
        LayerReading::Known {
            value: value.into(),
            source: source.into(),
            captured_at,
        }
    }

    /// Could not be read here. `reason` must say what stopped the read — it is
    /// the only thing that distinguishes "this machine is misconfigured" from
    /// "this observer cannot see that layer".
    pub fn unknown(reason: impl Into<String>, captured_at: DateTime<Utc>) -> Self {
        LayerReading::Unknown {
            reason: reason.into(),
            captured_at,
        }
    }

    /// Deliberately not printed.
    pub fn withheld(reason: impl Into<String>, captured_at: DateTime<Utc>) -> Self {
        LayerReading::Withheld {
            reason: reason.into(),
            captured_at,
        }
    }

    /// The uppercase state label used in the text report and in summaries.
    pub fn state_label(&self) -> &'static str {
        match self {
            LayerReading::Known { .. } => "KNOWN",
            LayerReading::Unknown { .. } => "UNKNOWN",
            LayerReading::Withheld { .. } => "WITHHELD",
        }
    }

    /// When this reading was taken.
    pub fn captured_at(&self) -> DateTime<Utc> {
        match self {
            LayerReading::Known { captured_at, .. }
            | LayerReading::Unknown { captured_at, .. }
            | LayerReading::Withheld { captured_at, .. } => *captured_at,
        }
    }
}

// ===========================================================================
// Injection — the bin-only half, handed in as data (the DoctorInputs pattern).
// ===========================================================================

/// Who is producing this report. Recorded in the output because the observer
/// determines which layers are observable at all, and a reader comparing two
/// reports needs to know they were not taken from the same vantage point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Observer {
    /// The standalone `config_report` bin, which links only the lib crate.
    HeadlessBin,
    /// The runner binary (the Tauri command), which can inject bin-only layers.
    RunnerApp,
}

impl Observer {
    /// Stable wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            Observer::HeadlessBin => "headless-bin",
            Observer::RunnerApp => "runner-app",
        }
    }

    /// The `Unknown` reason to report for a `Resolved` bin-side layer that this
    /// observer did not inject a reading for.
    ///
    /// The two arms mean genuinely different things and must not share a
    /// sentence: from the headless bin it is a STRUCTURAL limit (the module is
    /// not in this binary), while from the runner app it can only be a WIRING
    /// bug (the module is right there and the command failed to pass it).
    ///
    /// The headless arm splits again on [`LayerSide::ExternalBinary`]. Saying
    /// "lives in the runner binary, which this bin does not link" about a layer
    /// owned by the SUPERVISOR would be false, and would send a reader to the
    /// wrong remediation — the in-app report cannot read the supervisor's
    /// source either; what it can do is observe the env the supervisor handed
    /// the runner, which a bin launched from a shell never received.
    fn missing_injection_reason(self, spec: &LayerSpec) -> String {
        match self {
            Observer::HeadlessBin if spec.side == LayerSide::ExternalBinary => format!(
                "not observable from the headless bin — `{}` is owned by a DIFFERENT \
                 executable, and this bin was launched from a shell rather than by it, \
                 so it never received what that executable injects; run the in-app \
                 config report, which observes the env the runner was handed",
                spec.anchor
            ),
            Observer::HeadlessBin => format!(
                "not observable from the headless bin — `{}` lives in the runner \
                 binary, which this bin does not link; run the in-app config report",
                spec.anchor
            ),
            Observer::RunnerApp => format!(
                "resolver for `{}` was not injected by the runner binary — this is \
                 a wiring bug in the config-report command, not a machine \
                 misconfiguration",
                spec.anchor
            ),
        }
    }
}

/// The readings only the RUNNER BINARY can resolve, handed to the lib driver as
/// data.
///
/// This is the [`crate::coord_doctor::DoctorInputs`] pattern, and it is what
/// makes a lib-side driver able to report bin-only layers at all. Each field is
/// `Option<LayerReading>`: `Some` when the observer resolved it, `None` when it
/// could not — and `None` becomes an honest `Unknown` naming the reason (see
/// [`Observer::missing_injection_reason`]), never a silently-dropped row.
///
/// Later phases add one field per bin-only layer they land. Deliberately NOT
/// `Default`-derived: a new field must force each caller to decide whether it
/// can resolve that layer, rather than silently defaulting to "no".
#[derive(Debug, Clone)]
pub struct ConfigReportInputs {
    /// Who is asking — see [`Observer`].
    pub observer: Observer,
    /// Layer 1 (`settings_struct`): the WHOLE-FILE provenance of the persisted
    /// settings document — `loaded` / `fresh_install` / `unreadable`, the
    /// resolved `settings.json` path, and for `unreadable` the reason. Resolved
    /// in the bin by `config_report_cmd::settings_struct_reading`; `settings` is
    /// a bin-only module.
    ///
    /// Deliberately NOT per-field attribution, and the reading says so in its
    /// own value text — see this module's Phase 5 section for why that was a
    /// decision rather than an omission. No `Settings` field VALUE may ever be
    /// put in here: the struct carries `web_integration.runner_token`,
    /// `qontinui_user_id` and the sync toggles, and this row reports the
    /// document's provenance and path, never its contents.
    pub settings_struct: Option<LayerReading>,
    /// Layer 2 (`config_dir`): the directory `settings.json` is read from and
    /// written to, plus whether `QONTINUI_CONFIG_DIR` or the platform default
    /// produced it. Resolved in the bin by
    /// `config_report_cmd::config_dir_reading` — `settings` is a bin-only
    /// module.
    pub config_dir: Option<LayerReading>,
    /// Layer 5 (`api_endpoint_registry`): the effective qontinui-web backend
    /// base URL plus which of the four documented rungs won. Resolved in the
    /// bin by `config_report_cmd::api_base_url_reading`, because
    /// `api_config` is a bin-only module.
    pub api_endpoint_registry: Option<LayerReading>,
    /// Layer 11 (`claude_config_dir`): the per-account `CLAUDE_CONFIG_DIR` a
    /// spawned session resolves to, plus which selection arm decided it —
    /// including the two arms that decide there is NO account. Resolved in the
    /// bin by `config_report_cmd::claude_config_dir_reading`; `ai_provider` is
    /// a bin-only module.
    pub claude_config_dir: Option<LayerReading>,
    /// Layer 6 (`launch_env_snapshot`): the typed environment the runner read
    /// ONCE in `main()`, plus whether a re-read of the same process env still
    /// agrees with it. Resolved in the bin by
    /// `config_report_cmd::launch_env_snapshot_reading`; `launch_env` is a
    /// bin-only module.
    pub launch_env_snapshot: Option<LayerReading>,
    /// Layer 7 (`adhoc_env_reads`): the generation every scattered
    /// `std::env::var` call actually sees — this process's own env, frozen when
    /// the runner started. Resolved in the bin by
    /// `config_report_cmd::adhoc_env_reads_reading`.
    pub adhoc_env_reads: Option<LayerReading>,
    /// Layer 8 (`supervisor_injected_env`): which of the variables the dev
    /// supervisor stamps onto a runner at spawn are actually present here.
    /// Owned by a different executable, so this is an OBSERVATION of the env
    /// this process was handed — never a read of the supervisor's source.
    /// Resolved in the bin by `config_report_cmd::supervisor_injected_reading`.
    pub supervisor_injected_env: Option<LayerReading>,
    /// Layer 9 (`coord_prompt_documents`): what the process-global
    /// prompt-document cache holds, and **how long ago it was last confirmed
    /// against coord** — which is a different fact from this reading's
    /// `captured_at`, and the one that decides whether the value is current.
    /// Resolved in the bin by `config_report_cmd::coord_prompt_documents_reading`;
    /// `prompt_library` is a bin-only module.
    pub coord_prompt_documents: Option<LayerReading>,
    /// Layer 10 (`fleet_policy_dial`): the four process-global caches one
    /// supervised poll loop refreshes, as they stand right now. TIME-VARYING
    /// with no restart of anything — the layer this module's per-reading
    /// `captured_at` was put there for. Resolved in the bin by
    /// `config_report_cmd::fleet_policy_dial_reading`.
    pub fleet_policy_dial: Option<LayerReading>,
    /// Layer 12 (`claude_settings_carrier`): the `--settings` file the runner
    /// materializes and hands a session by argv — its resolved path, whether
    /// that file is actually on disk (the shim's own gate), and which
    /// registration variant produced the name. Resolved in the bin by
    /// `config_report_cmd::claude_settings_carrier_reading`; `session` is a
    /// bin-only module.
    pub claude_settings_carrier: Option<LayerReading>,
    /// Layer 13 (`mcp_config_carrier`): the `--mcp-config` argv, built by the
    /// SEPARATE `qontinui-shim` binary. Like layer 8 this is an OBSERVATION of
    /// what this process can see (the env var the shim reads), never a read of
    /// that binary's state. Resolved in the bin by
    /// `config_report_cmd::mcp_config_carrier_reading`.
    pub mcp_config_carrier: Option<LayerReading>,
    /// Layer 14 (`mcp_json`): the shared-root `.mcp.json` — path, existence,
    /// which runner owns it, and **the `coord_mcp_safe_to_write` verdict**,
    /// which was previously observable only as a log line. Shape only; the
    /// bearer and the proxy nonce the file carries are structurally
    /// unreachable from the reporting type. Resolved in the bin by
    /// `config_report_cmd::mcp_json_reading`; `coord_mcp` is a bin-only module.
    pub mcp_json: Option<LayerReading>,
    /// The env-GENERATION section (Phase 3's headline): the same variable at
    /// three ages, the divergence between them, and what each spawn seam adds
    /// to a child env.
    ///
    /// Not a layer — a section — because it is the RELATIONSHIP between layers
    /// 6, 7 and 8 rather than any one of their values, and a one-line
    /// `LayerReading::Known` cannot carry a table. Bin-side for the same reason
    /// the three layers are: capturing it needs `launch_env`, `terminal` and
    /// `portable_pty::CommandBuilder`.
    pub env_generations: Option<crate::env_generations::EnvGenerations>,
}

// ===========================================================================
// The report + the byte-stable renderer.
// ===========================================================================

/// One layer's row: the static spec plus what it read out to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LayerRow {
    pub spec: &'static LayerSpec,
    pub reading: LayerReading,
}

/// The whole report: every layer in [`LAYER_SPECS`] order, plus who took it and
/// when the run started.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConfigReport {
    pub observer: Observer,
    /// When [`build_report`] started. Individual rows carry their OWN
    /// `captured_at`, which is the value that matters for a time-varying layer;
    /// this one just dates the run.
    pub generated_at: DateTime<Utc>,
    pub rows: Vec<LayerRow>,
    /// The env-generation section, when the observer could capture it. `None`
    /// renders an explicit "not observable from here" note rather than nothing
    /// — a silently-absent section reads as "there is no divergence", which is
    /// the same lie as rendering an unread layer as its default.
    pub env_generations: Option<crate::env_generations::EnvGenerations>,
}

/// Column width for the state label — `WITHHELD` is the longest at 8.
const STATE_WIDTH: usize = 8;

/// Format a timestamp the one way this module ever formats one: RFC 3339, UTC,
/// millisecond precision. Fixed precision matters — a variable-width stamp
/// makes the report diff noisily for no information.
fn stamp(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Millis, true)
}

impl ConfigReport {
    /// Render the copy-pasteable text report. Ordering, labels and field order
    /// are the contract: two runs on two machines must differ only where the
    /// configuration differs.
    ///
    /// Note what is NOT here: there is no branch that prints a value for an
    /// `Unknown` or a `Withheld` row, because those variants carry no value to
    /// print. The renderer cannot lie about a layer it could not read.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("qontinui config report — effective configuration by layer\n");
        out.push_str("=========================================================\n");
        out.push_str(&format!("observer:     {}\n", self.observer.as_str()));
        out.push_str(&format!("generated_at: {}\n", stamp(self.generated_at)));
        out.push('\n');

        for (i, row) in self.rows.iter().enumerate() {
            out.push_str(&format!(
                "{:>2}. [{:<width$}] {} — {}\n",
                i + 1,
                row.reading.state_label(),
                row.spec.name,
                row.spec.title,
                width = STATE_WIDTH,
            ));
            match &row.reading {
                LayerReading::Known { value, source, .. } => {
                    out.push_str(&format!("      value:       {value}\n"));
                    out.push_str(&format!("      source:      {source}\n"));
                }
                LayerReading::Unknown { reason, .. } | LayerReading::Withheld { reason, .. } => {
                    out.push_str(&format!("      reason:      {reason}\n"));
                }
            }
            out.push_str(&format!(
                "      captured_at: {}\n",
                stamp(row.reading.captured_at())
            ));
        }

        let known = self.count(|r| matches!(r, LayerReading::Known { .. }));
        let unknown = self.count(|r| matches!(r, LayerReading::Unknown { .. }));
        let withheld = self.count(|r| matches!(r, LayerReading::Withheld { .. }));
        out.push_str("---------------------------------------------------------\n");
        out.push_str(&format!(
            "{} layers — {} known, {} unknown, {} withheld.\n",
            self.rows.len(),
            known,
            unknown,
            withheld
        ));
        // The report is only useful if the reader trusts UNKNOWN to mean what
        // it says, so say it in the output rather than only in the source.
        out.push_str(
            "UNKNOWN means the layer could not be read HERE — it is never a default value.\n",
        );
        out.push_str("WITHHELD means the layer holds credentials and is never printed.\n");

        // The env-generation section. Absent is SAID, not implied: a missing
        // section would read as "no divergence between generations", which is
        // exactly as misleading as printing an unread layer's default.
        match &self.env_generations {
            Some(env) => out.push_str(&env.render()),
            None => out.push_str(
                "\nenv generations — NOT OBSERVABLE from this observer. The three generations \
                 (this\nprocess's env, the once-in-`main()` launch snapshot, and what a PTY child \
                 is spawned\nwith) need `launch_env` and `terminal`, which are runner-binary \
                 modules. This is the\nabsence of a reading, NOT a finding that the generations \
                 agree.\n",
            ),
        }
        out
    }

    fn count(&self, pred: fn(&LayerReading) -> bool) -> usize {
        self.rows.iter().filter(|r| pred(&r.reading)).count()
    }

    /// The row for `name`, or `None` if this report has no such layer. Used by
    /// consumers (and tests) that want one layer rather than the whole render.
    pub fn row(&self, name: &str) -> Option<&LayerRow> {
        self.rows.iter().find(|r| r.spec.name == name)
    }
}

// ===========================================================================
// The driver.
// ===========================================================================

/// Build the full report: every layer in [`LAYER_SPECS`] order, resolved from
/// the lib where possible and from `inputs` where the layer is bin-only.
///
/// Unlike `coord_doctor::run_checks`, there is no first-red-stops behavior —
/// the layers are independent, and a layer that could not be read is exactly
/// the thing the reader most needs to see.
pub fn build_report(inputs: &ConfigReportInputs) -> ConfigReport {
    let generated_at = Utc::now();
    let rows = LAYER_SPECS
        .iter()
        .map(|spec| LayerRow {
            spec,
            reading: resolve_layer(spec, inputs, Utc::now()),
        })
        .collect();
    ConfigReport {
        observer: inputs.observer,
        generated_at,
        rows,
        env_generations: inputs.env_generations.clone(),
    }
}

/// Resolve one layer. `now` is passed in rather than sampled inside so a layer
/// resolver stays a pure function of (spec, inputs, clock) and the tests can
/// pin the rendered output.
fn resolve_layer(
    spec: &'static LayerSpec,
    inputs: &ConfigReportInputs,
    now: DateTime<Utc>,
) -> LayerReading {
    // Pending layers answer from the table, not from a match arm — so landing a
    // later phase is a one-row edit here plus its resolver, and a layer can
    // never be silently forgotten.
    if let LayerStatus::Pending(phase) = spec.status {
        return LayerReading::unknown(format!("not yet implemented (phase {phase})"), now);
    }

    match spec.name {
        "settings_json_second_reader" => resolve_settings_json_second_reader(now),
        "profiles_coord_base" => resolve_profiles_coord_base(now),
        "secure_storage_keyring" => resolve_secure_storage(now),
        // Bin-only layers arrive as data or not at all.
        "settings_struct" => inputs.settings_struct.clone().unwrap_or_else(|| {
            LayerReading::unknown(inputs.observer.missing_injection_reason(spec), now)
        }),
        "config_dir" => inputs.config_dir.clone().unwrap_or_else(|| {
            LayerReading::unknown(inputs.observer.missing_injection_reason(spec), now)
        }),
        "api_endpoint_registry" => inputs.api_endpoint_registry.clone().unwrap_or_else(|| {
            LayerReading::unknown(inputs.observer.missing_injection_reason(spec), now)
        }),
        "claude_config_dir" => inputs.claude_config_dir.clone().unwrap_or_else(|| {
            LayerReading::unknown(inputs.observer.missing_injection_reason(spec), now)
        }),
        "launch_env_snapshot" => inputs.launch_env_snapshot.clone().unwrap_or_else(|| {
            LayerReading::unknown(inputs.observer.missing_injection_reason(spec), now)
        }),
        "adhoc_env_reads" => inputs.adhoc_env_reads.clone().unwrap_or_else(|| {
            LayerReading::unknown(inputs.observer.missing_injection_reason(spec), now)
        }),
        "supervisor_injected_env" => inputs.supervisor_injected_env.clone().unwrap_or_else(|| {
            LayerReading::unknown(inputs.observer.missing_injection_reason(spec), now)
        }),
        "coord_prompt_documents" => inputs.coord_prompt_documents.clone().unwrap_or_else(|| {
            LayerReading::unknown(inputs.observer.missing_injection_reason(spec), now)
        }),
        "fleet_policy_dial" => inputs.fleet_policy_dial.clone().unwrap_or_else(|| {
            LayerReading::unknown(inputs.observer.missing_injection_reason(spec), now)
        }),
        "claude_settings_carrier" => inputs.claude_settings_carrier.clone().unwrap_or_else(|| {
            LayerReading::unknown(inputs.observer.missing_injection_reason(spec), now)
        }),
        "mcp_config_carrier" => inputs.mcp_config_carrier.clone().unwrap_or_else(|| {
            LayerReading::unknown(inputs.observer.missing_injection_reason(spec), now)
        }),
        "mcp_json" => inputs.mcp_json.clone().unwrap_or_else(|| {
            LayerReading::unknown(inputs.observer.missing_injection_reason(spec), now)
        }),
        // A `Resolved` layer with no arm here is a programming error, and the
        // `every_resolved_layer_has_a_resolver` test catches it — but say so in
        // the output too rather than rendering something plausible.
        other => LayerReading::unknown(
            format!("layer {other:?} is marked Resolved but has no resolver wired in build_report"),
            now,
        ),
    }
}

/// Layer 3 — the LIB-side reader of `settings.json`, and which arm produced its
/// path.
///
/// This layer exists to be COMPARED with layer 2, not read on its own. Two
/// independent code paths resolve the same file from the same env var:
/// `settings::resolve_config_dir` (bin) and `profiles::settings_json_path` (lib,
/// here). They are not the same rule — the bin reader discards an
/// exported-but-empty `QONTINUI_CONFIG_DIR` and the lib reader honours it — so
/// on a machine where that variable is exported empty the two rows carry
/// different paths, and the runner is genuinely reading its tier from a
/// different file than it writes its settings to. Printing both rows is the
/// only way that is visible; printing one and calling it "the settings.json
/// path" is the failure mode.
///
/// The arm comes from the resolver's own return value. Nothing here re-reads
/// `QONTINUI_CONFIG_DIR`, so this row cannot drift from what the lib reader
/// actually did.
fn resolve_settings_json_second_reader(now: DateTime<Utc>) -> LayerReading {
    let (path, source) = crate::profiles::settings_json_path();
    match path {
        Some(p) => LayerReading::known(p.display().to_string(), source.as_str(), now),
        // `Unresolvable` — `dirs::config_dir()` yielded nothing, so there is no
        // path. UNKNOWN, not a guessed default: this reader has no fallback and
        // inventing one here would be reporting a file nobody read.
        None => LayerReading::unknown(
            format!(
                "the lib-side settings.json reader could not resolve a path \
                 (source: {}) — no QONTINUI_CONFIG_DIR and no platform config \
                 dir, so `profiles::read_runner_tier` reads TierRead::Unknown \
                 on this machine",
                source.as_str()
            ),
            now,
        ),
    }
}

/// Layer 4 — the effective coord base and the arm that produced it.
///
/// This calls the canonical policy fn rather than re-deriving anything: the
/// report must be able to ASK for the source, never recompute it, or the report
/// and the live dial can disagree. (`coord_base_policy` emits at most one
/// process-global log line per arm the first time it applies; running the
/// report can therefore be what triggers that line, which is harmless.)
///
/// # The value is CLASSIFIED before it is rendered
///
/// This is the second of the two layers whose value is a URL, and its
/// highest-precedence arm is the env var `COORD_HTTP_URL` — which a self-hosted
/// coord behind basic auth carries as `https://user:pw@coord.internal`. The env
/// TABLES would withhold that value while this row printed it, in one document.
/// So it goes through [`EnvVarReading::classify`] under the env variable's own
/// name, which is what makes the classifier's joint `*_URL`-name arm reachable
/// (a bare `scheme://account@host`, with no password, is withheld under that arm
/// and would be printed without it). The ARM is always printed in full — it is
/// the half that stays useful when the value is withheld. The bin-side twin,
/// and the audit of every remaining layer, is
/// `config_report_cmd::api_base_url_reading`.
fn resolve_profiles_coord_base(now: DateTime<Utc>) -> LayerReading {
    use crate::env_generations::{EnvFingerprinter, EnvVarReading};
    use crate::profiles::{CoordBase, CoordBaseSource};

    let (base, source): (CoordBase, CoordBaseSource) = crate::profiles::coord_base_policy();
    match base {
        CoordBase::Configured(b) | CoordBase::DevLocalhost(b) | CoordBase::TierDefault(b) => {
            let fp = EnvFingerprinter::new();
            LayerReading::known(
                EnvVarReading::classify(&fp, "COORD_HTTP_URL", &b)
                    .value
                    .detail(),
                source.as_str(),
                now,
            )
        }
        // `coord_base_policy` documents that it never returns `Unset`. If it
        // ever does, the honest report is UNKNOWN: `coord_base_with_source`
        // substitutes the dev-localhost string here, which is exactly the
        // "render the fallback as if it were read" failure this report exists
        // to stop.
        CoordBase::Unset => LayerReading::unknown(
            "coord_base_policy() returned Unset, which it documents as unreachable — \
             the coord base is genuinely undetermined, not dev-localhost",
            now,
        ),
    }
}

/// Layer 15 — the credential store. Permanently withheld.
///
/// There is no `Known` path out of this function, by construction. It does not
/// read the store, it does not report whether the store has contents, and it
/// takes no input that could make it print one: the only safe amount of a
/// credential store to put in a diagnostic report is none of it.
fn resolve_secure_storage(now: DateTime<Utc>) -> LayerReading {
    LayerReading::withheld(
        "credential store — deliberately never printed; listed so this report \
         declares it was considered rather than leaving a reader to wonder",
        now,
    )
}

// ===========================================================================
// Doc generator — the layer inventory as markdown, from the same table.
//
// Same trick as `coord_doctor::render_onboarding_doc`: one source of truth for
// the layer list means the doc cannot claim a layer the report does not have.
// ===========================================================================

/// Render the layer inventory as markdown, generated entirely from
/// [`LAYER_SPECS`]. Byte-stable. Emit it via `config_report --layer-doc`.
///
/// Published at `docs/runner-config-layers.md` — that file IS this
/// function's output, held byte-exact by
/// `tests::config_report_checked_in_layer_doc_is_the_generators_output`. A
/// generator whose artifact is never checked in publishes nothing, which
/// would leave the three tests below that assert "the generated layer doc
/// must carry X" asserting about a document no reader ever sees.
pub fn render_layer_doc() -> String {
    let mut out = String::new();
    out.push_str("# Runner configuration layers\n\n");
    out.push_str(
        "Every layer below independently contributes to the effective configuration of \
         a runner session. There is no merge function anywhere — each one is resolved by \
         its own hand-rolled resolver — which is why `config report` aggregates them \
         instead of trying to unify them.\n\n",
    );
    out.push_str(
        "<!-- GENERATED — do not edit by hand. Regenerate FROM BASH (Git Bash on \
         Windows): `cargo run --bin config_report -- --layer-doc > \
         docs/runner-config-layers.md`. NOT from PowerShell, whose `>` writes UTF-16 \
         or a BOM that `include_str!` cannot read at all. The source of truth is \
         `LAYER_SPECS` in `src-tauri/src/config_report.rs`, and \
         `config_report_checked_in_layer_doc_is_the_generators_output` fails if the \
         checked-in file drifts from it. -->\n\n",
    );
    for (i, s) in LAYER_SPECS.iter().enumerate() {
        out.push_str(&format!("## {}. {} (`{}`)\n\n", i + 1, s.title, s.name));
        out.push_str(&format!("{}\n\n", s.describes));
        out.push_str(&format!("- Resolved by: `{}`\n", s.anchor));
        out.push_str(&format!("- Owned by: {}\n", s.side.as_str()));
        match s.status {
            LayerStatus::Resolved => out.push_str("- Status: reported\n\n"),
            LayerStatus::Pending(phase) => {
                out.push_str(&format!(
                    "- Status: **not yet reported** — lands in phase {phase}; \
                     until then the report shows this layer as `UNKNOWN`\n\n"
                ));
            }
        }
    }
    out.push_str("---\n\n");
    out.push_str(
        "A layer marked `UNKNOWN` in the report could **not be read** at the point the \
         report was taken. It is never a default value. A layer marked `WITHHELD` holds \
         credentials and is never printed.\n",
    );
    out
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed instant, so every rendered-output assertion can be a literal.
    fn fixed_stamp() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-22T12:34:56.789Z")
            .expect("literal is valid RFC 3339")
            .with_timezone(&Utc)
    }

    fn headless_inputs() -> ConfigReportInputs {
        ConfigReportInputs {
            observer: Observer::HeadlessBin,
            settings_struct: None,
            config_dir: None,
            api_endpoint_registry: None,
            claude_config_dir: None,
            launch_env_snapshot: None,
            adhoc_env_reads: None,
            supervisor_injected_env: None,
            coord_prompt_documents: None,
            fleet_policy_dial: None,
            claude_settings_carrier: None,
            mcp_config_carrier: None,
            mcp_json: None,
            env_generations: None,
        }
    }

    /// A runner-app payload with every bin-only layer injected, so
    /// `Resolved`-but-unwired can only be reported by a genuine wiring gap.
    fn injected_inputs() -> ConfigReportInputs {
        ConfigReportInputs {
            observer: Observer::RunnerApp,
            settings_struct: Some(LayerReading::known(
                "C:/cfg/settings.json — provenance `loaded`",
                "settings::read_settings_from_disk",
                fixed_stamp(),
            )),
            config_dir: Some(LayerReading::known(
                "C:/cfg",
                "platform_config_dir",
                fixed_stamp(),
            )),
            api_endpoint_registry: Some(LayerReading::known(
                "https://example.invalid",
                "test",
                fixed_stamp(),
            )),
            claude_config_dir: Some(LayerReading::known("(none)", "unconfigured", fixed_stamp())),
            launch_env_snapshot: Some(LayerReading::known("primary", "test", fixed_stamp())),
            adhoc_env_reads: Some(LayerReading::known("3 variables", "test", fixed_stamp())),
            supervisor_injected_env: Some(LayerReading::known("0 of 7", "test", fixed_stamp())),
            coord_prompt_documents: Some(LayerReading::known("cache empty", "test", fixed_stamp())),
            fleet_policy_dial: Some(LayerReading::known("off", "test", fixed_stamp())),
            claude_settings_carrier: Some(LayerReading::known("C:/s.json", "test", fixed_stamp())),
            mcp_config_carrier: Some(LayerReading::known("(absent)", "test", fixed_stamp())),
            mcp_json: Some(LayerReading::known("absent", "test", fixed_stamp())),
            env_generations: None,
        }
    }

    /// The inventory is fifteen layers with unique names, and the names are
    /// asserted against LITERALS — comparing the table to itself would pin
    /// nothing, and the count is exactly the fact the plan's draft got wrong.
    #[test]
    fn config_report_layer_table_is_fifteen_unique_named_layers() {
        assert_eq!(LAYER_SPECS.len(), 15, "the inventory is fifteen layers");

        let names: Vec<&str> = LAYER_SPECS.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            vec![
                "settings_struct",
                "config_dir",
                "settings_json_second_reader",
                "profiles_coord_base",
                "api_endpoint_registry",
                "launch_env_snapshot",
                "adhoc_env_reads",
                "supervisor_injected_env",
                "coord_prompt_documents",
                "fleet_policy_dial",
                "claude_config_dir",
                "claude_settings_carrier",
                "mcp_config_carrier",
                "mcp_json",
                "secure_storage_keyring",
            ]
        );

        let mut unique: Vec<&str> = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "layer names must be unique");
    }

    /// Which layers are wired, and on which side of the crate boundary — pinned
    /// as a LITERAL, because the set is the phase's deliverable and deriving it
    /// from the table would assert nothing.
    ///
    /// Phase 2 took this from three rows to six: `config_dir` and
    /// `claude_config_dir` became resolvable once `settings::resolve_config_dir`
    /// and `ai_provider::get_effective_config_dir` started returning their
    /// source, and `settings_json_second_reader` once
    /// `profiles::settings_json_path` did. Every one of them is a resolver
    /// change, not a report change — the report gained no ability to work out a
    /// precedence order for itself.
    ///
    /// Phase 3 takes it to nine, adding the three ENV layers. Note the side of
    /// the third: `supervisor_injected_env` is `ExternalBinary` and stays that
    /// way. The runner does not read the supervisor's source and this row does
    /// not pretend to — it reports which of the injected names are present in
    /// the env this process was HANDED, which is a different (and honest)
    /// claim.
    ///
    /// Phase 4 takes it to FOURTEEN — every layer but `settings_struct`. Two of
    /// the five it adds are the time-varying coord-served ones (9, 10) and
    /// three are the carriers (12, 13, 14). `mcp_config_carrier` is the second
    /// `ExternalBinary` row and is here for the same reason layer 8 is: the
    /// argv is assembled inside the `qontinui-shim` binary, so the runner
    /// reports what it can genuinely see (the env var that binary reads) and
    /// nothing about that binary's own state.
    ///
    /// Phase 5 takes it to FIFTEEN — the whole inventory. The row it adds,
    /// `settings_struct`, is `Bin` because `settings::read_settings_from_disk` is a
    /// runner-binary module, and it is resolved at WHOLE-FILE grain rather than
    /// per-field on purpose (see the module docs). "Resolved coarsely" is still
    /// resolved: the row carries a real reading of a real fact, which is the bar
    /// this column asserts.
    #[test]
    fn config_report_resolved_layers_are_the_full_fifteen() {
        let resolved: Vec<(&str, LayerSide)> = LAYER_SPECS
            .iter()
            .filter(|s| s.status == LayerStatus::Resolved)
            .map(|s| (s.name, s.side))
            .collect();
        assert_eq!(
            resolved,
            vec![
                ("settings_struct", LayerSide::Bin),
                ("config_dir", LayerSide::Bin),
                ("settings_json_second_reader", LayerSide::Lib),
                ("profiles_coord_base", LayerSide::Lib),
                ("api_endpoint_registry", LayerSide::Bin),
                ("launch_env_snapshot", LayerSide::Bin),
                ("adhoc_env_reads", LayerSide::Bin),
                ("supervisor_injected_env", LayerSide::ExternalBinary),
                ("coord_prompt_documents", LayerSide::Bin),
                ("fleet_policy_dial", LayerSide::Bin),
                ("claude_config_dir", LayerSide::Bin),
                ("claude_settings_carrier", LayerSide::Bin),
                ("mcp_config_carrier", LayerSide::ExternalBinary),
                ("mcp_json", LayerSide::Bin),
                ("secure_storage_keyring", LayerSide::Lib),
            ]
        );
        assert_eq!(
            resolved.len(),
            15,
            "after Phase 5 every layer in the inventory is resolved"
        );
    }

    /// Every `Resolved` layer must actually hit a resolver arm. Without this, a
    /// table row flipped to `Resolved` with no wiring renders a plausible-looking
    /// row of nonsense.
    #[test]
    fn config_report_every_resolved_layer_has_a_resolver() {
        let inputs = injected_inputs();
        for spec in LAYER_SPECS
            .iter()
            .filter(|s| s.status == LayerStatus::Resolved)
        {
            let reading = resolve_layer(spec, &inputs, fixed_stamp());
            if let LayerReading::Unknown { reason, .. } = &reading {
                assert!(
                    !reason.contains("has no resolver wired"),
                    "layer {} is Resolved but unwired: {reason}",
                    spec.name
                );
            }
        }
    }

    /// UNKNOWN survives to the rendered output, and the row carries NO value.
    /// This is the plan's central commitment: an unreadable layer must never be
    /// rendered as the default the code would have used.
    #[test]
    fn config_report_renders_unknown_as_unknown_and_never_as_a_value() {
        let report = ConfigReport {
            observer: Observer::HeadlessBin,
            generated_at: fixed_stamp(),
            rows: vec![LayerRow {
                spec: layer("settings_struct"),
                reading: LayerReading::unknown("settings.json unreadable", fixed_stamp()),
            }],
            env_generations: None,
        };
        let text = report.render();
        assert!(text.contains("[UNKNOWN ]"), "state label missing:\n{text}");
        assert!(
            text.contains("reason:      settings.json unreadable"),
            "reason missing:\n{text}"
        );
        assert!(
            !text.contains("value:"),
            "an UNKNOWN row must not render a value line:\n{text}"
        );
        assert!(
            !text.contains("source:"),
            "an UNKNOWN row must not render a source line:\n{text}"
        );
        assert!(
            text.contains("UNKNOWN means the layer could not be read HERE"),
            "the report must state what UNKNOWN means:\n{text}"
        );
    }

    /// The credential layer is Withheld, appears as a ROW (so a reader can see
    /// it was considered), and carries no value anywhere in the render.
    #[test]
    fn config_report_withheld_layer_is_present_and_valueless() {
        let report = build_report(&headless_inputs());
        let row = report
            .row("secure_storage_keyring")
            .expect("the keyring layer must appear as a row");
        match &row.reading {
            LayerReading::Withheld { reason, .. } => {
                assert!(
                    reason.contains("never printed"),
                    "withheld reason should say it is never printed: {reason}"
                );
            }
            other => panic!("the keyring layer must be Withheld, got {other:?}"),
        }

        let text = report.render();
        // Isolate the keyring row and assert it has no value/source line.
        let start = text
            .find("secure_storage_keyring")
            .expect("keyring row rendered");
        let keyring_row = &text[start..];
        let row_body = keyring_row.split("\n---").next().unwrap_or(keyring_row);
        assert!(
            !row_body.contains("value:"),
            "a WITHHELD row must not render a value line:\n{row_body}"
        );
        assert!(text.contains("[WITHHELD]"), "state label missing:\n{text}");
    }

    /// Byte-stability, asserted against a LITERAL. If the header, the column
    /// widths, the field order or the summary line move, this fails.
    #[test]
    fn config_report_render_is_byte_stable() {
        let report = ConfigReport {
            observer: Observer::RunnerApp,
            generated_at: fixed_stamp(),
            rows: vec![
                LayerRow {
                    spec: layer("profiles_coord_base"),
                    reading: LayerReading::known(
                        "https://coord.qontinui.io",
                        "tier_default",
                        fixed_stamp(),
                    ),
                },
                LayerRow {
                    spec: layer("settings_struct"),
                    reading: LayerReading::unknown("settings.json unreadable", fixed_stamp()),
                },
            ],
            env_generations: None,
        };
        let expected = "\
qontinui config report — effective configuration by layer
=========================================================
observer:     runner-app
generated_at: 2026-08-22T12:34:56.789Z

 1. [KNOWN   ] profiles_coord_base — profiles.json active profile → coord base
      value:       https://coord.qontinui.io
      source:      tier_default
      captured_at: 2026-08-22T12:34:56.789Z
 2. [UNKNOWN ] settings_struct — Settings struct (~90 fields) — WHOLE-FILE provenance
      reason:      settings.json unreadable
      captured_at: 2026-08-22T12:34:56.789Z
---------------------------------------------------------
2 layers — 1 known, 1 unknown, 0 withheld.
UNKNOWN means the layer could not be read HERE — it is never a default value.
WITHHELD means the layer holds credentials and is never printed.

env generations — NOT OBSERVABLE from this observer. The three generations (this
process's env, the once-in-`main()` launch snapshot, and what a PTY child is spawned
with) need `launch_env` and `terminal`, which are runner-binary modules. This is the
absence of a reading, NOT a finding that the generations agree.
";
        assert_eq!(report.render(), expected);
        // Same value in ⇒ same bytes out, twice.
        assert_eq!(report.render(), report.render());
    }

    /// The headless observer reports the bin-only layer as UNKNOWN with a
    /// STRUCTURAL reason — not as a silently-omitted row, and not as a guess.
    #[test]
    fn config_report_headless_bin_reports_bin_layers_as_unknown() {
        let report = build_report(&headless_inputs());
        assert_eq!(report.rows.len(), 15, "every layer gets a row, always");
        let row = report
            .row("api_endpoint_registry")
            .expect("the bin-only layer still gets a row");
        match &row.reading {
            LayerReading::Unknown { reason, .. } => {
                assert!(
                    reason.contains("not observable from the headless bin"),
                    "reason must name the structural limit: {reason}"
                );
            }
            other => panic!("headless must not claim to know a bin-only layer: {other:?}"),
        }
    }

    /// THE FALSIFICATION CHECK, lib side: a reading resolved in the runner
    /// binary reaches the lib driver unchanged and renders as KNOWN. If this
    /// could not be made to work, the driver's placement would be wrong and the
    /// whole plan would need re-scoping.
    #[test]
    fn config_report_injected_bin_reading_reaches_the_report() {
        let inputs = ConfigReportInputs {
            observer: Observer::RunnerApp,
            settings_struct: None,
            config_dir: None,
            api_endpoint_registry: Some(LayerReading::known(
                "http://127.0.0.1:8000",
                "env:QONTINUI_API_URL",
                fixed_stamp(),
            )),
            claude_config_dir: None,
            launch_env_snapshot: None,
            adhoc_env_reads: None,
            supervisor_injected_env: None,
            coord_prompt_documents: None,
            fleet_policy_dial: None,
            claude_settings_carrier: None,
            mcp_config_carrier: None,
            mcp_json: None,
            env_generations: None,
        };
        let report = build_report(&inputs);
        let row = report.row("api_endpoint_registry").expect("row present");
        assert_eq!(
            row.reading,
            LayerReading::known(
                "http://127.0.0.1:8000",
                "env:QONTINUI_API_URL",
                fixed_stamp()
            )
        );
        assert!(report
            .render()
            .contains("      value:       http://127.0.0.1:8000\n"));
    }

    /// The lib-side layer resolves for real against this machine's state, and
    /// its source is one of the five documented arms (literal list — the arms
    /// are a contract, not an implementation detail).
    #[test]
    fn config_report_coord_base_layer_resolves_to_a_documented_arm() {
        let report = build_report(&headless_inputs());
        let row = report.row("profiles_coord_base").expect("row present");
        match &row.reading {
            LayerReading::Known { value, source, .. } => {
                assert!(!value.is_empty(), "a known coord base must have a value");
                assert!(
                    [
                        "env",
                        "profile",
                        "tier_default",
                        "dev_localhost_fallback",
                        "unknown_tier_prod_default",
                    ]
                    .contains(&source.as_str()),
                    "unexpected coord-base arm: {source}"
                );
            }
            other => panic!("coord_base_policy always resolves; got {other:?}"),
        }
    }

    /// Layer 3 resolves against this machine's real state and names one of the
    /// three documented arms. The arm list is a LITERAL — the wire strings are
    /// a contract that a reader compares across machines, so asserting them
    /// against `SettingsJsonPathSource::as_str()` would pin nothing.
    ///
    /// A `Known` row must also carry a path that actually ends in
    /// `settings.json`: the whole value of this layer is that it names the
    /// second file, and a row that agreed on the arm while pointing somewhere
    /// else would be worse than no row.
    #[test]
    fn config_report_settings_json_second_reader_names_a_documented_arm() {
        let report = build_report(&headless_inputs());
        let row = report
            .row("settings_json_second_reader")
            .expect("row present");
        match &row.reading {
            LayerReading::Known { value, source, .. } => {
                assert!(
                    ["env:QONTINUI_CONFIG_DIR", "platform_config_dir"].contains(&source.as_str()),
                    "unexpected settings.json path arm: {source}"
                );
                assert!(
                    value.ends_with("settings.json"),
                    "layer 3 must name the settings.json file itself, got {value}"
                );
            }
            // Only reachable where `dirs::config_dir()` yields nothing. Still
            // has to be honest about which arm it fell out of.
            LayerReading::Unknown { reason, .. } => {
                assert!(
                    reason.contains("source: unresolvable"),
                    "an unresolvable path must name the arm: {reason}"
                );
            }
            other => panic!("layer 3 is never withheld, got {other:?}"),
        }
    }

    /// **NO layer is `Pending`, and no row renders as "not yet implemented".**
    ///
    /// Two halves, and both are needed. The first is about the TABLE: a phase
    /// that adds a sixteenth layer and forgets to wire it fails here rather than
    /// shipping a row that reads like a legitimate finding. The second is about
    /// the OUTPUT, and it is the half a table-only assertion would miss — the
    /// pending sentence is produced in `resolve_layer`, so a row could in
    /// principle acquire it from somewhere other than `LayerStatus`, and the
    /// operator reads the render, not the table.
    ///
    /// The literal is the phrase itself rather than `LayerStatus`, because the
    /// thing being pinned is what a reader sees.
    #[test]
    fn config_report_no_layer_is_pending() {
        let pending: Vec<&str> = LAYER_SPECS
            .iter()
            .filter(|s| matches!(s.status, LayerStatus::Pending(_)))
            .map(|s| s.name)
            .collect();
        assert_eq!(
            pending,
            Vec::<&str>::new(),
            "every layer in the inventory must have a live resolver"
        );

        let text = build_report(&headless_inputs()).render();
        assert!(
            !text.contains("not yet implemented"),
            "no rendered row may still say it is unimplemented:\n{text}"
        );
    }

    /// The `Pending` mechanism itself still works, exercised through a synthetic
    /// spec because the real inventory no longer has an instance.
    ///
    /// Without this, `LayerStatus::Pending` becomes a shape that compiles and
    /// has not been run since Phase 4 — and the phase that finally needs it
    /// discovers that at the worst moment. Asserted against the LITERAL sentence
    /// an operator would read, not against the format string.
    #[test]
    fn config_report_pending_machinery_still_names_its_phase() {
        static PROBE: LayerSpec = LayerSpec {
            name: "probe_layer_that_is_not_in_the_inventory",
            title: "synthetic probe",
            describes: "exercises the Pending arm now that no real row uses it",
            anchor: "config_report::tests::PROBE",
            side: LayerSide::Lib,
            status: LayerStatus::Pending(99),
        };
        assert_eq!(
            resolve_layer(&PROBE, &headless_inputs(), fixed_stamp()),
            LayerReading::unknown("not yet implemented (phase 99)", fixed_stamp())
        );
        // …and since no REAL row is pending, the published doc must not carry
        // the "not yet reported" note for any of the fifteen either.
        let doc = render_layer_doc();
        assert!(
            !doc.contains("not yet reported"),
            "the generated layer doc still marks a layer unreported:\n{doc}"
        );
    }

    /// Layer 1 is a BIN-module layer, so the headless bin gives it the
    /// bin-module `Unknown` sentence naming `settings::read_settings_from_disk`
    /// — **not** `load_settings`, which discards provenance by design and would
    /// send a reader to a symbol that cannot answer this layer at all, and not
    /// `load_settings_full`, which answers it but WRITES on the way (see the
    /// module docs); the anchor has to name the call the row actually makes.
    #[test]
    fn config_report_settings_struct_layer_points_at_the_provenance_reader() {
        let report = build_report(&headless_inputs());
        match &report.row("settings_struct").expect("row present").reading {
            LayerReading::Unknown { reason, .. } => {
                assert!(
                    reason.contains("lives in the runner binary"),
                    "layer 1 is a bin-module layer: {reason}"
                );
                assert!(
                    reason.contains(
                        "settings::read_settings_from_disk → settings::SettingsProvenance"
                    ),
                    "the reason must name the symbol that resolves the layer: {reason}"
                );
                assert!(
                    !reason.contains("owned by a DIFFERENT executable"),
                    "layer 1 is not owned by another executable: {reason}"
                );
            }
            other => panic!("headless cannot observe layer 1, got {other:?}"),
        }
    }

    /// The layer table RECORDS that per-field attribution was a decision, not an
    /// omission — in the `describes` text, which is what the generated layer doc
    /// publishes. A future reader must be able to tell the difference without
    /// the commit history.
    #[test]
    fn config_report_layer_1_records_the_per_field_attribution_decision() {
        let spec = layer("settings_struct");
        assert!(
            spec.describes
                .contains("CONSIDERED AND DELIBERATELY NOT BUILT"),
            "the closure must be recorded in the table: {}",
            spec.describes
        );
        assert!(
            spec.title.contains("WHOLE-FILE provenance"),
            "the row header must not read as per-field attribution: {}",
            spec.title
        );
        // …and it reaches the published doc, which is the artifact a reader
        // outside this file actually gets.
        let doc = render_layer_doc();
        assert!(
            doc.contains("CONSIDERED AND DELIBERATELY NOT BUILT"),
            "the generated layer doc must carry the decision:\n{doc}"
        );
    }

    /// The three env layers Phase 3 landed are no longer `Pending` — asserted
    /// against the LITERAL phase-3 wording so a row silently reverting to
    /// "not yet implemented" fails here rather than reading as a legitimate
    /// UNKNOWN in the output.
    #[test]
    fn config_report_env_layers_are_no_longer_pending() {
        let report = build_report(&headless_inputs());
        for name in [
            "launch_env_snapshot",
            "adhoc_env_reads",
            "supervisor_injected_env",
        ] {
            match &report.row(name).expect("row present").reading {
                LayerReading::Unknown { reason, .. } => assert!(
                    !reason.contains("not yet implemented"),
                    "{name} still reports as pending: {reason}"
                ),
                other => panic!("headless cannot KNOW {name}, got {other:?}"),
            }
        }
    }

    /// The five layers Phase 4 landed are no longer `Pending`, and the headless
    /// bin still cannot KNOW any of them — they are all bin-side or
    /// external-binary. Asserted against the LITERAL phase-4 wording so a row
    /// silently reverting to "not yet implemented" fails here rather than
    /// reading as a legitimate UNKNOWN in the output.
    #[test]
    fn config_report_phase_4_layers_are_no_longer_pending() {
        let report = build_report(&headless_inputs());
        for name in [
            "coord_prompt_documents",
            "fleet_policy_dial",
            "claude_settings_carrier",
            "mcp_config_carrier",
            "mcp_json",
        ] {
            match &report.row(name).expect("row present").reading {
                LayerReading::Unknown { reason, .. } => assert!(
                    !reason.contains("not yet implemented"),
                    "{name} still reports as pending: {reason}"
                ),
                other => panic!("headless cannot KNOW {name}, got {other:?}"),
            }
        }
    }

    /// Layer 13 is the SECOND `ExternalBinary` row, and the headless bin must
    /// give it the external-binary sentence rather than the bin-module one.
    ///
    /// This is the distinction Phase 3 had to fix for layer 8, restated for the
    /// `qontinui-shim` binary — and it is not cosmetic: "run the in-app config
    /// report" is the right remediation for a bin-module layer and the WRONG
    /// one for a layer whose value lives in another executable's argv.
    #[test]
    fn config_report_shim_carrier_layer_names_the_other_executable() {
        let report = build_report(&headless_inputs());
        match &report
            .row("mcp_config_carrier")
            .expect("row present")
            .reading
        {
            LayerReading::Unknown { reason, .. } => {
                assert!(
                    reason.contains("owned by a DIFFERENT executable"),
                    "reason must name the external binary: {reason}"
                );
                assert!(
                    reason.contains("bin/qontinui_shim::identity_mcp_config_args"),
                    "reason must name the owning symbol: {reason}"
                );
                assert!(
                    !reason.contains("lives in the runner binary"),
                    "an external-binary layer must not borrow the bin-module sentence: {reason}"
                );
            }
            other => panic!("headless cannot observe layer 13, got {other:?}"),
        }
        // …and layer 14 IS a runner-binary module, so it gets the OTHER
        // sentence. Both arms asserted together: a single-arm test would pass
        // just as well if `missing_injection_reason` collapsed to one string.
        match &report.row("mcp_json").expect("row present").reading {
            LayerReading::Unknown { reason, .. } => {
                assert!(
                    reason.contains("lives in the runner binary"),
                    "a bin-module layer must get the bin-module sentence: {reason}"
                );
                assert!(
                    !reason.contains("owned by a DIFFERENT executable"),
                    "layer 14 is not owned by another executable: {reason}"
                );
            }
            other => panic!("headless cannot observe layer 14, got {other:?}"),
        }
    }

    /// Layer 8 is owned by a DIFFERENT executable, and the headless bin says so
    /// in those words rather than borrowing the "lives in the runner binary"
    /// sentence — which would be false and would send a reader to the wrong
    /// remediation.
    #[test]
    fn config_report_external_binary_layer_gets_its_own_unknown_reason() {
        let report = build_report(&headless_inputs());
        match &report
            .row("supervisor_injected_env")
            .expect("row present")
            .reading
        {
            LayerReading::Unknown { reason, .. } => {
                assert!(
                    reason.contains("owned by a DIFFERENT executable"),
                    "reason must name the external binary: {reason}"
                );
                assert!(
                    !reason.contains("lives in the runner binary"),
                    "an external-binary layer must not borrow the bin-module sentence: {reason}"
                );
            }
            other => panic!("headless cannot observe layer 8, got {other:?}"),
        }
    }

    /// An absent env-generation section is STATED. A silently-missing section
    /// reads as "the generations agree", which is the same class of lie as
    /// rendering an unread layer as its default.
    #[test]
    fn config_report_absent_env_section_says_so() {
        let text = build_report(&headless_inputs()).render();
        assert!(
            text.contains("env generations — NOT OBSERVABLE from this observer"),
            "the absent section must be announced:\n{text}"
        );
        assert!(
            text.contains("absence of a reading, NOT a finding that the generations agree"),
            "the note must refuse the wrong inference:\n{text}"
        );
    }

    /// An injected env-generation section reaches the rendered report, and a
    /// credential-classed value planted in it does NOT.
    #[test]
    fn config_report_injected_env_section_renders_without_its_credentials() {
        use crate::env_generations::{
            EnvFingerprinter, EnvGeneration, EnvGenerationSpec, EnvGenerations,
        };

        let fp = EnvFingerprinter::new();
        let mut inputs = headless_inputs();
        inputs.env_generations = Some(EnvGenerations {
            generations: vec![EnvGeneration::capture(
                &fp,
                EnvGenerationSpec {
                    id: "G1",
                    name: "runner_process",
                    describes: "this process's own env",
                    freshness: "frozen when the runner started",
                    is_full_env: true,
                },
                fixed_stamp(),
                [
                    ("QONTINUI_CONFIG_DIR", "C:/cfg"),
                    ("QONTINUI_TEST_LOGIN_PASSWORD", "hunter2"),
                ],
            )],
            divergences: vec![],
            launch_drift: None,
            seams: vec![],
        });

        let text = build_report(&inputs).render();
        assert!(
            text.contains("env generations — the same variable at three ages"),
            "the section must reach the report:\n{text}"
        );
        assert!(!text.contains("hunter2"), "a credential leaked:\n{text}");
        assert!(
            text.contains("1 variable readings withheld"),
            "the withheld count must be stated:\n{text}"
        );
    }

    /// Every reading carries a capture time — including the Unknown and
    /// Withheld ones. Layer 10 changes with no restart, so a report without a
    /// per-layer stamp is already wrong today.
    #[test]
    fn config_report_every_reading_is_timestamped_and_rendered() {
        let report = build_report(&headless_inputs());
        for row in &report.rows {
            assert!(
                row.reading.captured_at() >= report.generated_at,
                "row {} captured before the run started",
                row.spec.name
            );
        }
        let text = report.render();
        assert_eq!(
            text.matches("captured_at: ").count(),
            15,
            "one capture stamp per layer:\n{text}"
        );
    }

    /// The doc is generated from the same table, so it can never list a layer
    /// the report does not have.
    #[test]
    fn config_report_layer_doc_covers_every_layer_and_is_stable() {
        let doc = render_layer_doc();
        for spec in LAYER_SPECS {
            assert!(doc.contains(spec.name), "doc omits layer {}", spec.name);
            assert!(doc.contains(spec.title), "doc omits title {}", spec.title);
        }
        assert!(doc.contains("15. OS keyring / SecureStorage — WITHHELD"));
        assert_eq!(doc, render_layer_doc());
    }

    /// The checked-in `docs/runner-config-layers.md` IS the generator's output,
    /// byte for byte.
    ///
    /// Without this, `render_layer_doc` is a generator with no artifact: it
    /// emits a `GENERATED — do not edit by hand` header naming a file that
    /// nothing publishes, and three tests above ("the artifact a reader outside
    /// this file actually gets") assert against a doc no reader ever sees. The
    /// doc is the deliverable; this is the gate that keeps it true.
    ///
    /// `include_str!` rather than the CI-workflow shape `coord_doctor`'s
    /// onboarding doc uses (`.github/workflows/onboarding-doc-fresh.yml`,
    /// regenerate-and-`git diff --exit-code`), for two reasons. It runs
    /// unconditionally in `cargo test --lib` instead of behind that workflow's
    /// `detect` gate, so it cannot be skipped by a path filter; and a PR that
    /// edits a gating workflow trips `ci-integrity.yml`'s self-edit guard by
    /// design, which would make the anti-drift gate itself unmergeable through
    /// the normal train. A stale doc fails an ordinary unit test here, rather
    /// than a workflow that has to be able to edit itself to be maintained.
    ///
    /// Line endings are not a hazard: `.gitattributes` pins `*.md text eol=lf`,
    /// so the working-tree bytes `include_str!` reads are LF on every platform,
    /// exactly like the newline the generator emits.
    ///
    /// The ENCODING of the redirect is. Because there is no CI job that
    /// regenerates this doc, a human on Windows is the only thing that ever
    /// refreshes it — and PowerShell's `>` writes UTF-16LE with a BOM, which
    /// `include_str!` rejects outright. That surfaces as `stream did not
    /// contain valid UTF-8` and stops the crate compiling, so it is worth
    /// naming everywhere the command appears rather than leaving the next
    /// person to decode it. `coord_doctor`'s onboarding doc never meets this
    /// because its workflow regenerates it under bash on ubuntu.
    #[test]
    fn config_report_checked_in_layer_doc_is_the_generators_output() {
        const CHECKED_IN: &str = include_str!("../../docs/runner-config-layers.md");
        assert_eq!(
            CHECKED_IN,
            render_layer_doc(),
            "docs/runner-config-layers.md is stale — regenerate it FROM BASH with \
             `cargo run --bin config_report -- --layer-doc > \
             docs/runner-config-layers.md` and commit the result. PowerShell's `>` \
             would write UTF-16/BOM here, which is a COMPILE error rather than a \
             failure of this test"
        );
    }

    /// JSON keeps the tri-state tagged, and an Unknown serializes with no
    /// `value` key at all — the wire shape carries the same guarantee the
    /// renderer does.
    #[test]
    fn config_report_json_unknown_has_no_value_key() {
        let reading = LayerReading::unknown("settings.json unreadable", fixed_stamp());
        let json = serde_json::to_value(&reading).expect("serializes");
        assert_eq!(json["state"], "unknown");
        assert_eq!(json["reason"], "settings.json unreadable");
        assert!(json.get("value").is_none(), "unknown must carry no value");
    }
}
