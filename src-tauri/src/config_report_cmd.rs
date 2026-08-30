//! In-app `config report` Tauri command
//! (plan `2026-08-20-effective-config-provenance-and-env-generation`, Phases 1-5).
//!
//! Thin runner-binary wrapper over the lib-crate driver
//! (`qontinui_runner_lib::config_report`). The layer table, the `LayerReading`
//! tri-state, the driver and the byte-stable formatter all live in the lib so
//! the standalone `config_report` bin shares them.
//!
//! **This module's only job is the half the lib structurally cannot do.** Ten
//! of the fifteen configuration layers live in BIN-only modules of the runner
//! binary, and a lib module cannot call them. So each bin-only layer is
//! resolved HERE and injected as data through
//! [`ConfigReportInputs`](qontinui_runner_lib::config_report::ConfigReportInputs)
//! — exactly the way `coord_doctor_cmd::coord_doctor_run` injects
//! `coord_mcp::resolve_bound_api_port()` into `DoctorInputs`.
//!
//! Phase 1 injected one such layer, `api_endpoint_registry`
//! (`crate::api_config`), chosen because its resolver is already a pure
//! function of its four arguments. It was the phase's falsification check: if a
//! bin-only layer could not be injected into a lib-side driver, the driver's
//! placement would be wrong and the plan would need re-scoping. It could.
//!
//! Phase 2 adds `config_dir` (`crate::settings`) and `claude_config_dir`
//! (`crate::ai_provider`), and — the point of that phase — deletes the last
//! place where this module derived provenance for itself. Every arm below now
//! comes back FROM the resolver that produced the value, in the same call.
//! **Nothing in this module may re-implement a precedence rule.** A second copy
//! of a resolution order is the plan's stated dominant correctness risk: it
//! compiles, it agrees with the real resolver on the day it is written, and it
//! silently starts lying the first time the real one changes — which is exactly
//! the failure a provenance report exists to prevent, reproduced inside the
//! report itself.
//!
//! Phase 3 adds the three ENV layers (6, 7, 8) and the env-GENERATION section,
//! and it obeys the same rule one level down. It does not restate what a spawn
//! seam does to a child environment: it CALLS the seam's own extracted
//! env-construction function on a throwaway `Command` and reads the overrides
//! back. All eight seams in the `crate::terminal` table were already extracted
//! and unit-tested (that is what made them observable), so nothing on the spawn
//! path changes — the eight functions only widened from private to
//! `pub(crate)` so this module can call them.
//!
//! Phase 5 adds the last bin-only layer, `settings_struct`, and obeys the rule a
//! third way: it does not re-derive whether the settings document was the user's
//! — it ASKS `settings::load_settings_full`, whose `SettingsProvenance` return
//! is the same value the settings module's own write path gates on. What that
//! phase deliberately does NOT build is per-field attribution; see
//! [`settings_struct_reading`] and the `config_report` module docs for why that
//! is a recorded decision rather than a missing feature.
//!
//! Values never reach this module in raw form for long: every environment value
//! goes through `env_generations::EnvVarReading::classify` at ingestion, and a
//! credential-classed one is dropped there. See that module's header for why
//! the alternative — a redaction pass over the rendered text — cannot be the
//! control for a report shaped like this one.

use chrono::{DateTime, Utc};
use qontinui_runner_lib::config_report::{
    build_report, ConfigReport, ConfigReportInputs, LayerReading, Observer,
};
use qontinui_runner_lib::env_generations::{
    diff_generations, lossy_env_pairs, EnvFingerprinter, EnvGeneration, EnvGenerationSpec,
    EnvGenerations, EnvValue, EnvVarReading, LaunchFieldDrift, LaunchSnapshotDrift, SeamEnvReport,
};

use crate::ai_provider::ClaudeConfigDirSource;
use crate::api_config::{resolve_api_base_url, ApiBaseUrlInputs};
use crate::launch_env::RunnerLaunchEnv;

/// Layer 5 — the effective qontinui-web backend base URL plus the rung that
/// produced it.
///
/// Pure over its inputs (the I/O is [`gather_api_base_url_inputs`]), so the
/// mapping from inputs to a reading is unit-testable without touching the
/// environment or settings.json.
///
/// Both halves come out of ONE [`resolve_api_base_url`] call. Phase 1 had to
/// derive the arm in a second function walking the same rungs, because that
/// resolver returned only the `String`; Phase 2 folded the arm into its return
/// shape and deleted the derivation, so the value and the arm are now incapable
/// of disagreeing.
///
/// # The value is CLASSIFIED before it is rendered
///
/// This row's value is a URL, and a URL is the one shape that carries a
/// credential in a field a name-based classifier never looks at:
/// `QONTINUI_WEB_BACKEND_URL=https://ops:S3cretPw@qontinui.internal` is a real
/// configuration for a self-hosted backend behind basic auth.
///
/// Until this pass, `classify_env_var` was wired into `EnvGeneration::capture`
/// ONLY — the env tables — and no LAYER row ran its value through anything. So
/// one rendered report showed the G1/G3 rows for that exact variable as
/// `<withheld #…: value is a URL with a password in its userinfo>` while this
/// row printed the password in full, six lines away: the same secret, in the
/// same document, withheld in one place and published in the other. The
/// persisted rung (`web_integration.backend_url`) reached the row without going
/// past the classifier at all.
///
/// So the value goes through [`EnvVarReading::classify`] under
/// `ApiBaseUrlArm::value_origin_name` — the name of the slot it actually came
/// from, which is what makes the classifier's joint `*_URL`-name arm reachable
/// (see that function). `.detail()` renders a withheld value as its per-run
/// fingerprint plus the reason, never as any part of the URL.
///
/// **The ARM is still printed in full, always.** It is the half an operator
/// needs when the value is withheld — "which rung produced this?" is answerable
/// without seeing the URL, and a row that withheld both would be useless.
///
/// # The rest of the layer table, audited
///
/// Layer 4 (`profiles_coord_base`) is the only other row whose value is a URL;
/// it is classified the same way, lib-side, in
/// `config_report::resolve_profiles_coord_base`. Layers 12, 13 and 14 already
/// classified their env values before this pass. Every remaining row's value is
/// a filesystem path, a count, a bool or an enum arm — none can carry `://`, so
/// [`url_userinfo`](qontinui_runner_lib::env_generations::url_userinfo) is
/// structurally unreachable for them, and the entropy arm's charset excludes
/// the `:` a Windows path carries in its drive letter and a Unix `PATH` carries
/// as its separator. It does NOT exclude `/`, so the arm is reachable for a
/// long enough mixed-case Unix path — over-withholding one path row, which is
/// the direction `env_generations` errs in deliberately; the claim that `/`
/// excluded it was simply wrong about `env_generations::token_charset`, whose
/// set is `[A-Za-z0-9+/=_.-]` plus U+FFFD, and is corrected here rather than
/// left as a protection the code does not provide.
pub(crate) fn api_base_url_reading(
    inputs: &ApiBaseUrlInputs,
    captured_at: DateTime<Utc>,
) -> LayerReading {
    let (value, arm) = resolve_api_base_url(
        inputs.env_web.clone(),
        inputs.env_api.clone(),
        inputs.persisted.clone(),
        inputs.is_debug,
    );
    let fp = EnvFingerprinter::new();
    LayerReading::known(
        EnvVarReading::classify(&fp, arm.value_origin_name(), &value)
            .value
            .detail(),
        arm.as_str(),
        captured_at,
    )
}

/// Layer 1 — is the settings document this runner is acting on the user's real
/// persisted state, or a placeholder?
///
/// Pure over its arguments, so all three provenance arms — including the
/// `Unreadable` one that is hard to produce on a healthy machine — are
/// table-testable without touching the real `settings.json`.
///
/// # What this row answers, and what it deliberately does NOT
///
/// It answers **"are these the user's persisted values?"**. It does **not**
/// answer "did field X come from disk or from `default_app_mode()`" — and the
/// value text says so out loud, because a reader who took `loaded` for per-field
/// attribution would be worse off than one with no row at all.
///
/// That coarser grain is a decision, not a gap. Per-field attribution needs a
/// hand-written `serde::Deserialize` over ~90 fields recording whether each
/// `#[serde(default = "…")]` fired, bolted onto the one code path every tier,
/// identity and credential verdict in this runner already routes through — a
/// large new correctness surface for a question none of the plan's three
/// confusion cases turns on (all three are answered by layers 2/5/14 and the
/// env-generation section). See the `config_report` module docs.
///
/// # Why all three arms are `Known` and none is `Unknown`
///
/// `Unknown` in this report means "this layer could not be READ **here**", and
/// its remediation is "go find a better vantage point". All three provenance
/// arms are the opposite of that: the layer WAS read, and the answer is a fact
/// about this machine. `unreadable` in particular is the most actionable reading
/// the row can produce — the fix is to repair or move that file — and burying it
/// in an `Unknown` reason would dress a machine fault up as an observer
/// limitation. Same call layer 9 makes for an empty cache and layer 11 for "no
/// account".
///
/// # Why the reader is `read_settings_from_disk` and NOT `load_settings_full`
///
/// Both return the same `LoadedSettings { settings, provenance, error }`, so
/// they answer this layer identically — but only one of them is a READ.
/// `load_settings_full` additionally runs `claude_accounts::load_with_migration`
/// (which writes `claude-accounts.json`), mints a `local_user_id` UUID plus a
/// tier migration and calls `save_settings` — **writing the operator's real
/// settings file** — and reaches the OS keyring through
/// `AuthManager::new().get_access_token()`. A diagnostic that did all that would
/// change the answer by asking the question, exactly as
/// [`claude_settings_carrier_reading`] refuses to call `materialize`; and
/// because two tests below drive the live command, `cargo test` on a dev box
/// would have done it too. `read_settings_from_disk` resolves through the
/// non-creating `resolve_settings_path`, runs no migration, persists nothing and
/// touches no credential store.
///
/// The cost of the swap is stated in the row rather than hidden: the overlays
/// `load_settings_full` layers on top (the Restate ports,
/// `QONTINUI_WEB_BACKEND_URL`, `QONTINUI_RUNNER_TOKEN`, `QONTINUI_RUNNER_TIER`,
/// the machine-global Claude account roster) are NOT reflected here — which is
/// the correct grain anyway, since this layer is about the DOCUMENT, and the
/// overlays are covered by layers 5, 6 and 7.
///
/// # The `unreadable` arm is the point of the row
///
/// When `settings.json` exists and cannot be read or parsed, the reader
/// hands back `Settings::default()` — a DEFAULT PLACEHOLDER whose `tier`,
/// `web_integration.runner_token`, `setup_completed`, `qontinui_user_id` and
/// sync toggles are **not** the user's values. That is precisely the "a fallback
/// rendered as if it had been read" failure this whole report refuses to commit,
/// occurring one layer down in the code the report describes, and until this row
/// existed the report could not say it had happened. So the arm leads with it.
///
/// # The load error: bounded at the source, then classified as PROSE
///
/// Two independent controls, because one of them used to be decorative.
///
/// The error is free text assembled around an OS or `serde` message.
/// `serde_json::Error`'s Display for a DATA error (`invalid type`,
/// `invalid value`) **quotes the offending value out of the file** — and the
/// file is `settings.json`, which carries `web_integration.runner_token` and
/// `qontinui_user_id`. So `settings::read_settings_from_path` no longer formats
/// the message body at all: it emits `JSON <category> error at line L column C`,
/// which is everything a reader needs to repair the file and cannot carry a
/// credential.
///
/// The classification pass is then [`EnvVarReading::classify_free_text`], NOT
/// [`EnvVarReading::classify`]. The plain classifier examines the value as a
/// whole, and its entropy arm requires `chars().all(token_charset)` — it dies
/// on the first space, so against a sentence it returns `None` for any embedded
/// secret whatsoever. Calling it here and documenting it as a protection was a
/// claim the code did not honour; `classify_free_text` tokenises on whitespace
/// and classifies per token, so it catches what the bounding at the source is
/// there to prevent from arriving in the first place.
///
/// NO `Settings` FIELD VALUE is an input to this function — the provenance, the
/// path and the error are the whole of it.
pub(crate) fn settings_struct_reading(
    provenance: crate::settings::SettingsProvenance,
    error: Option<&str>,
    settings_json_path: Result<std::path::PathBuf, String>,
    captured_at: DateTime<Utc>,
) -> LayerReading {
    use crate::settings::SettingsProvenance;

    let fp = EnvFingerprinter::new();
    let path = match &settings_json_path {
        Ok(p) => p.display().to_string(),
        // `get_settings_path` failed. The provenance is still authoritative
        // about the load (it will be `unreadable`, which is exactly what an
        // unresolvable path produces), but the report must not name a file it
        // could not resolve.
        Err(e) => format!("(path unresolvable: {e})"),
    };

    let verdict = match provenance {
        SettingsProvenance::Loaded => "a settings.json existed and parsed, so the document this \
             runner is acting on IS the user's real persisted state"
            .to_string(),
        SettingsProvenance::FreshInstall => {
            "NO settings.json exists — a genuine first run. Authoritative (there is nothing to \
             lose), but nothing in the struct came from disk: every field is its compiled-in \
             default"
                .to_string()
        }
        SettingsProvenance::Unreadable => format!(
            "the file or its directory EXISTS and could NOT be read or parsed ({}). The \
             `Settings` this runner is acting on is a DEFAULT PLACEHOLDER — `tier`, \
             `web_integration.runner_token`, `setup_completed`, `qontinui_user_id` and the sync \
             toggles are NOT the user's values. Repair or move that file; nothing here is a \
             fallback worth acting on",
            error
                .map(
                    |e| EnvVarReading::classify_free_text(&fp, "settings_load_error", e)
                        .value
                        .detail()
                )
                .unwrap_or_else(|| "no reason recorded".to_string()),
        ),
    };

    LayerReading::known(
        format!(
            "{path} — provenance `{}`: {verdict}. WHOLE-FILE only: this row says whether the \
             DOCUMENT is the user's, NOT which individual fields were present in it — an absent \
             key is filled by its `#[serde(default = \"…\")]` silently and is indistinguishable \
             here. Per-field attribution was considered and deliberately not built (see the layer \
             doc). ON-DISK DOCUMENT only: the runner's own `load_settings_full` layers \
             in-memory-only overlays over this parsed document — the Restate ports, \
             `QONTINUI_WEB_BACKEND_URL`, `QONTINUI_RUNNER_TOKEN`, `QONTINUI_RUNNER_TIER` and the \
             machine-global Claude account roster — and this row does NOT run them (they write), \
             so even `loaded` does not mean every effective value came from that file",
            provenance.as_str(),
        ),
        "settings::read_settings_from_disk → settings::SettingsProvenance (WHOLE-FILE provenance \
         of settings.json, read WITHOUT the roster migration, the local_user_id/tier persist and \
         the keyring read that `load_settings_full` performs; NOT per-field attribution)",
        captured_at,
    )
}

/// Layer 2 — the config directory `settings.json` lives in, plus whether the
/// `QONTINUI_CONFIG_DIR` override or the platform default produced it.
///
/// Pure over the resolver's `(dir, source)` result — the same shape layers 5
/// and 11 take — so both arms, and the load-bearing "this stats, it does not
/// create" property, are table-testable against a path the test owns.
///
/// An `Err` is UNKNOWN carrying the resolver's own message: the directory
/// genuinely could not be resolved, and the report must not print the path the
/// code *would* have used.
///
/// # Why the caller passes `resolve_config_dir` and NEVER `get_config_dir`
///
/// `get_config_dir` **creates** the directory (`fs::create_dir_all`). A report
/// that called it would take a typo'd `QONTINUI_CONFIG_DIR=D:/qonitnui`, bring
/// that directory into existence, and then print it as though the machine had
/// always been configured that way — materializing the thing it is describing
/// and erasing the single most useful piece of evidence about the fault. So the
/// path comes from the non-creating twin and EXISTENCE is reported as a
/// separate, statted fact. Same rule [`claude_settings_carrier_reading`] states
/// for `materialize`, one layer up.
///
/// Taking the resolution as an ARGUMENT is what makes that assertable: this
/// function is now incapable of creating anything, whatever it is handed, and
/// `config_report_config_dir_row_stats_and_never_creates` pins it. The
/// non-creation of the resolver itself is pinned separately by
/// `settings::…::resolve_config_dir_creates_nothing`.
///
/// # The row's claim is about the WHOLE report, so it needed a THIRD guard
///
/// "this report does not" is a statement about every reader that runs before
/// this one, not merely about this function — and it was FALSE as written.
/// `mcp::fleet_policy_poller::briefing_store_path` resolved through the
/// creating twin, and the briefing cache is a `OnceLock` that TWO of this
/// report's own readers initialise: layer 11 via
/// `fleet_policy_poller::dial_snapshot` → `briefing_snapshot` →
/// `briefing_cache`, and
/// [`env_generations_section`] via [`pty_child_command`] →
/// `apply_base_child_env` → `terminal::runner_context` → `cached_briefing`.
/// Both run before this row stats the directory, so a typo'd
/// `QONTINUI_CONFIG_DIR` was created by the report and then printed
/// `on disk: true` — through a door neither this argument nor the F5
/// regression test could see, because a `#[cfg(test)]` twin of
/// `initial_briefing_cache` keeps every test build out of that path. That
/// resolver is now the non-creating one, pinned by
/// `fleet_policy_poller::…::briefing_store_path_resolves_the_config_dir_without_creating_it`
/// and `…::the_briefing_store_read_path_creates_nothing`.
pub(crate) fn config_dir_reading(
    resolved: Result<(std::path::PathBuf, crate::settings::ConfigDirSource), String>,
    captured_at: DateTime<Utc>,
) -> LayerReading {
    match resolved {
        Ok((dir, source)) => LayerReading::known(
            format!(
                "{} — on disk: {} (STATTED, never created: the runner's writers call the \
                 directory-CREATING twin, this report does not, so a `false` here is a \
                 configured-but-absent directory and not something this report made)",
                dir.display(),
                dir.is_dir(),
            ),
            source.as_str(),
            captured_at,
        ),
        Err(e) => LayerReading::unknown(
            format!("settings::resolve_config_dir() failed: {e}"),
            captured_at,
        ),
    }
}

/// Layer 11 — the per-account `CLAUDE_CONFIG_DIR` a spawned session resolves
/// to, plus the selection arm that decided it.
///
/// Pure over its `(value, source)` argument so every arm — including the two
/// that resolve to no account at all — is table-testable.
///
/// # Why "no account" is a KNOWN reading and not an UNKNOWN one
///
/// The tri-state's `Unknown` means "this layer could not be READ here". This
/// layer was read; the answer is that no config dir is exported, and that is a
/// fact about the machine, not a limit of the observer. Rendering it as
/// `UNKNOWN` would tell an operator to go find a better vantage point, when
/// what they actually need to do is log in — so it renders as `(none)` with the
/// arm saying WHICH kind of nothing it is: `rejected_no_credentials` (an
/// account is configured and its credentials died) versus `unconfigured` (no
/// account was ever selected). Those are different machines and different
/// fixes, and before this phase they were the same `None`.
///
/// # What the arm CANNOT say: roster-derived versus per-instance
///
/// [`ClaudeConfigDirSource`]'s six variants distinguish `LeastUsage` from
/// `Manual`, the picker's resolved dir from the configured fallback, and the two
/// kinds of nothing. **None of them distinguishes whether the
/// `ai.claude_cli` fields the resolver weighed came from the machine-global
/// roster (`claude-accounts.json`) or from this instance's own `settings.json`.**
/// That is not an oversight in this row — the resolver is handed a
/// `ClaudeCliSettings` and cannot see where those fields were sourced — and the
/// fix is not a seventh variant invented here: a report that manufactured a
/// provenance the resolver never returned would be re-deriving attribution,
/// which is the exact defect this module forbids.
///
/// The consequence has to be stated rather than papered over. The report applies
/// the roster overlay from `claude_accounts::load()` — the plain read — while
/// the runner's own `load_settings_full` applies it from `load_with_migration()`,
/// which first runs the one-shot seed migration from the unscoped
/// `settings.json`. On a machine where `claude-accounts.json` is ABSENT and that
/// unscoped file holds a non-empty roster, the migration would create the roster
/// and the runner would then resolve from it, while this report resolves from
/// the per-instance values — **so the two can name different accounts, and this
/// row cannot tell you that it happened.** The report takes the read anyway:
/// running the seed migration would make the diagnostic write
/// `claude-accounts.json`, which is the failure class layers 1, 2 and 12 all
/// refuse. Reading the row on such a machine means reading it as "the account
/// this process would select from what is on disk right now", not "the account
/// the runner selected".
pub(crate) fn claude_config_dir_reading(
    resolved: (Option<String>, ClaudeConfigDirSource),
    captured_at: DateTime<Utc>,
) -> LayerReading {
    let (dir, source) = resolved;
    LayerReading::known(
        dir.unwrap_or_else(|| "(none)".to_string()),
        source.as_str(),
        captured_at,
    )
}

// ===========================================================================
// Phase 3 — the env GENERATIONS.
//
// The plan's D1: an operator sets an env var and nothing happens, because the
// value is three restarts deep. Everything below exists to turn that from an
// inference into a line of output.
// ===========================================================================

/// The variables the dev supervisor stamps onto a runner when it spawns one.
///
/// This list is what layer 8 REPORTS ON, not what it reads: the supervisor is a
/// different executable and this process cannot read its source or its state.
/// What it can do — and all this layer claims — is say which of these names are
/// present in the environment this process was HANDED, and in which generation.
/// A name absent here means "this runner did not receive it", which on a
/// hand-launched runner is the expected answer, not a fault.
pub(crate) const SUPERVISOR_INJECTED_ENV_VARS: &[&str] = &[
    "QONTINUI_CONFIG_DIR",
    "QONTINUI_SECURE_STORAGE_DIR",
    "QONTINUI_INSTANCE_NAME",
    "QONTINUI_PRIMARY_PORT",
    "QONTINUI_PORT",
    "QONTINUI_API_URL",
    "QONTINUI_RUNNER_TIER",
];

/// The launch snapshot's fields, flattened to `(field, rendered value)` in a
/// FIXED order.
///
/// Rendering through `Debug`/`Option` here rather than diffing the struct
/// wholesale is deliberate: `RunnerLaunchEnv` derives `PartialEq`, so a
/// whole-struct comparison can only say "something moved". The report has to
/// name WHICH field moved, because the remediation differs per field.
fn launch_fields(e: &RunnerLaunchEnv) -> Vec<(&'static str, String)> {
    fn opt(v: &Option<String>) -> String {
        v.clone().unwrap_or_else(|| "(unset)".to_string())
    }
    fn opt_path(v: &Option<std::path::PathBuf>) -> String {
        v.as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(unset)".to_string())
    }
    fn opt_port(v: Option<u16>) -> String {
        v.map(|p| p.to_string())
            .unwrap_or_else(|| "(unset)".to_string())
    }
    vec![
        ("kind", format!("{:?}", e.kind)),
        ("instance_name", opt(&e.instance_name)),
        ("port", opt_port(e.port)),
        ("api_url", opt(&e.api_url)),
        ("primary_port", opt_port(e.primary_port)),
        ("server_mode", e.server_mode.to_string()),
        ("window", format!("{:?}", e.window)),
        ("panic_log_dir", opt_path(&e.panic_log_dir)),
        ("runner_log_dir", opt_path(&e.runner_log_dir)),
        (
            "webview2_user_data_dir",
            opt_path(&e.webview2_user_data_dir),
        ),
        ("restate", format!("{:?}", e.restate)),
    ]
}

/// The launch snapshot as an env-var-keyed map, for the side-by-side table.
///
/// These are PARSED values, which is why the generation is flagged
/// `is_full_env: false` and never value-diffed against a raw environment: the
/// snapshot holds `server_mode: false`, not the `"0"` / `"true"` / whatever the
/// operator exported, so a textual diff against G1 would manufacture
/// differences that do not exist. The snapshot's staleness is measured against
/// ITSELF instead — see [`launch_drift`].
fn launch_snapshot_pairs(e: &RunnerLaunchEnv) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut push = |name: &str, value: Option<String>| {
        if let Some(v) = value {
            pairs.push((name.to_string(), v));
        }
    };
    push("QONTINUI_INSTANCE_NAME", e.instance_name.clone());
    push("QONTINUI_PORT", e.port.map(|p| p.to_string()));
    push("QONTINUI_API_URL", e.api_url.clone());
    push(
        "QONTINUI_PRIMARY_PORT",
        e.primary_port.map(|p| p.to_string()),
    );
    push(
        "QONTINUI_SERVER_MODE",
        Some(format!("{} (parsed)", e.server_mode)),
    );
    push("QONTINUI_WINDOW_X", e.window.x.map(|v| v.to_string()));
    push("QONTINUI_WINDOW_Y", e.window.y.map(|v| v.to_string()));
    push(
        "QONTINUI_WINDOW_WIDTH",
        e.window.width.map(|v| v.to_string()),
    );
    push(
        "QONTINUI_WINDOW_HEIGHT",
        e.window.height.map(|v| v.to_string()),
    );
    push(
        "QONTINUI_WINDOW_DECORATIONS",
        e.window.decorations.map(|v| v.to_string()),
    );
    push(
        "QONTINUI_PANIC_LOG_DIR",
        e.panic_log_dir.as_ref().map(|p| p.display().to_string()),
    );
    push(
        "QONTINUI_RUNNER_LOG_DIR",
        e.runner_log_dir.as_ref().map(|p| p.display().to_string()),
    );
    push(
        "WEBVIEW2_USER_DATA_FOLDER",
        e.webview2_user_data_dir
            .as_ref()
            .map(|p| p.display().to_string()),
    );
    push(
        "QONTINUI_RESTATE_EXTERNAL_ADMIN_URL",
        e.restate.external_admin_url.clone(),
    );
    push(
        "QONTINUI_RESTATE_EXTERNAL_INGRESS_URL",
        e.restate.external_ingress_url.clone(),
    );
    pairs
}

/// Classify one rendered [`launch_fields`] row — as PROSE, not as a value.
///
/// # Why the plain classifier was inert here
///
/// [`launch_fields`] renders three of its rows through `Debug` — `kind`,
/// `window` and `restate` — so the field's text is a struct literal, not a
/// value. [`EnvVarReading::classify`] examines the value AS A WHOLE and is
/// therefore inert on that shape, which made the call here the exact vacuity
/// [`EnvVarReading::classify_free_text`]'s own doc names: a classify call that
/// reads as a protection and provides none. `RestateEnvHints` holds two RAW env
/// strings (`QONTINUI_RESTATE_EXTERNAL_ADMIN_URL`,
/// `…_INGRESS_URL` — `launch_env.rs`), and executed against the previous body:
///
/// | call | result |
/// |---|---|
/// | `classify_env_var("restate", <Debug>)` | `None` |
/// | `classify_env_var("restate", <the bare URL>)` | `ValueUrlPassword` |
/// | `classify_free_text("restate", <Debug>)` | `ValueUrlPassword` |
///
/// The `Debug` wrapper puts `"),` inside the host span, so `url_userinfo`'s
/// host charset rejects it, and the field label `restate` has no `_URL` suffix
/// for the joint arm to fire on. `api_url` was protected only by the accident
/// of its NAME ending `_URL`.
///
/// Every field goes through the prose classifier, not just the three `Debug`
/// ones. `classify_free_text` runs `classify_env_var` FIRST and only then
/// tokenises, so it can never withhold less than `classify` would. Applying it
/// per-field by hand would mean a field added later is unprotected until
/// someone remembers.
///
/// **Known over-withhold, accepted.** An earlier version of this comment
/// claimed none of these fields is "a separator-joined list like `PATH`".
/// That is false and worth stating plainly rather than deleting: `/` is in
/// [`token_base_char`](crate::env_generations), so a POSIX path IS that shape.
/// A path that contains a space (which defeats the whole-value
/// `all(token_charset)` test) and also carries one ≥32-byte mixed-case
/// alphanumeric segment reaches the tokenising arm and renders as
/// `<withheld #…>` — e.g. `/home/bob/My Docs/Qontinui2024BuildXyzAbc/logs`.
///
/// Accepted rather than fixed, for three reasons. The three path fields
/// (`panic_log_dir`, `runner_log_dir`, `webview2_user_data_dir`) render as
/// `C:\…` on this platform, where the drive-letter `:` defeats the entropy arm
/// before the separators matter. The cost is one CELL, not a row — the field
/// name, the drift, the row and the fingerprint all still render, so the drift
/// stays diagnosable. And it errs in the direction this module errs everywhere
/// else. Narrowing the classifier to recover a path segment would be the exact
/// reflex that produced five separate credential leaks in this file's history.
fn classify_launch_field(fp: &EnvFingerprinter, field: &str, text: &str) -> EnvValue {
    EnvVarReading::classify_free_text(fp, field, text).value
}

/// The launch snapshot `main()` took versus the same parser re-run now.
///
/// `None` when [`crate::launch_env::first_read`] holds nothing — i.e.
/// `RunnerLaunchEnv::read()` has never run in this process. That is UNKNOWN,
/// and the renderer says so; it is emphatically not "no drift".
///
/// Every field is classified by [`classify_launch_field`] — see there for why
/// the plain classifier was inert on half of them.
fn launch_drift(fp: &EnvFingerprinter) -> Option<LaunchSnapshotDrift> {
    let (at_launch, captured_at) = crate::launch_env::first_read()?;
    let now = RunnerLaunchEnv::read_uncached();
    let before = launch_fields(at_launch);
    let after = launch_fields(&now);
    let differing = before
        .iter()
        .zip(after.iter())
        .filter(|((_, a), (_, b))| a != b)
        .map(|((field, a), (_, b))| LaunchFieldDrift {
            field: (*field).to_string(),
            at_launch: classify_launch_field(fp, field, a),
            now: classify_launch_field(fp, field, b),
        })
        .collect();
    Some(LaunchSnapshotDrift {
        fields_compared: before.len(),
        differing,
        captured_at_launch: *captured_at,
    })
}

/// Read the overrides a `std::process::Command` carries: `(set, cleared)`.
///
/// `get_envs()` yields exactly the seam's DELTA on top of the inherited process
/// env — `Some(v)` for a name the seam sets, `None` for one it clears — which
/// is precisely the question "what does this seam do to a child env?".
fn std_command_env(
    fp: &EnvFingerprinter,
    cmd: &std::process::Command,
) -> (Vec<EnvVarReading>, Vec<String>) {
    let mut sets = Vec::new();
    let mut clears = Vec::new();
    for (k, v) in cmd.get_envs() {
        let name = k.to_string_lossy().to_string();
        match v {
            Some(val) => sets.push(EnvVarReading::classify(fp, &name, &val.to_string_lossy())),
            None => clears.push(name),
        }
    }
    sets.sort_by(|a, b| a.name.cmp(&b.name));
    clears.sort();
    (sets, clears)
}

/// One seam report from a `std::process::Command` built by a production seam.
fn std_seam(
    fp: &EnvFingerprinter,
    seam: &str,
    wrapper: &str,
    cmd: &std::process::Command,
) -> SeamEnvReport {
    let (sets, clears) = std_command_env(fp, cmd);
    SeamEnvReport {
        seam: seam.to_string(),
        command_type: "std::process::Command".to_string(),
        scrub_wrapper: wrapper.to_string(),
        sets,
        clears,
    }
}

/// Twin of [`std_seam`] for the tokio seams — `tokio::process::Command` is a
/// wrapper over the std type and `as_std()` exposes the same override map.
fn tokio_seam(
    fp: &EnvFingerprinter,
    seam: &str,
    wrapper: &str,
    cmd: &tokio::process::Command,
) -> SeamEnvReport {
    SeamEnvReport {
        command_type: "tokio::process::Command".to_string(),
        ..std_seam(fp, seam, wrapper, cmd.as_std())
    }
}

/// What G3 IS. A constant because [`env_generations_section`] renders it and
/// `config_report_g3_names_the_seam_it_omits` asserts against it — a literal
/// duplicated between the two would let the claim and the check drift apart.
const G3_DESCRIBES: &str = "what a PTY child spawned RIGHT NOW inherits: the portable-pty base \
     env + `session::TerminalSession::apply_base_child_env` (the marker strips, TERM, the runner \
     markers/port, the continuation-verdict forward, the briefing, the non-interactive git \
     posture) + the identity-shim PATH prepend + `finalize_child_env` (account pin + credential \
     scrub). NOT included, because replicating them WRITES: the identity seam's per-terminal \
     session/terminal ids and its coord-mcp `--mcp-config` provisioning, the install-interception \
     shim (default dark), and any caller-supplied `extra_env`";

/// G3's freshness line when the identity-shim dir for this build is already
/// materialized — the PATH prepend in this capture is the real one.
const G3_FRESHNESS_WITH_SHIM: &str =
    "fresher than G1 on Windows: `portable-pty` re-reads the HKLM/HKCU `Environment` registry \
     keys OVER the process env, so a user-scope change lands here with no runner restart — but a \
     terminal opened earlier keeps what G3 said at ITS spawn. The PATH here is the SHIM-PREPENDED \
     one: the identity-shim dir for this runner build is already materialized, so a child spawned \
     now resolves `claude`/`gemini` through it";

/// G3's freshness line when it is NOT — the real seam would materialize the dir
/// at spawn and prepend it; this report refuses to materialize, so the PATH
/// shown is the un-shimmed one and says so.
const G3_FRESHNESS_NO_SHIM: &str =
    "fresher than G1 on Windows: `portable-pty` re-reads the HKLM/HKCU `Environment` registry \
     keys OVER the process env, so a user-scope change lands here with no runner restart — but a \
     terminal opened earlier keeps what G3 said at ITS spawn. The PATH here is UN-SHIMMED: the \
     identity-shim dir for this runner build is not materialized yet, and the real spawn seam \
     MATERIALIZES it (this report refuses to), so a real child's PATH would carry one extra \
     leading entry — unless that materialize fails, which the seam handles fail-open";

/// Build the throwaway `CommandBuilder` that G3 reads, by CALLING the
/// production spawn seam's own extracted env functions in the production order.
///
/// Returns the identity-shim dir that was prepended, or `None` when this build's
/// dir is not materialized — which is the one part of the seam the report
/// cannot reproduce without writing, and is therefore reported rather than
/// simulated.
///
/// # Why this is not `new_default_prog()` + `finalize_child_env`
///
/// That was the whole of G3 before, and it made the G1→G3 divergence — headed
/// *"anything listed above is a variable the runner process itself does NOT hold
/// the current value of"* — show **no `PATH` delta at all**. `PATH` is the one
/// variable that decides which binary a child resolves, so a reader asking why
/// `cargo` in a runner pane hits the interception shim got a confidently wrong
/// answer from the row built to answer exactly that. The base seam's `TERM`,
/// `QONTINUI_RUNNER_TERMINAL`, `QONTINUI_RUNNER_API_PORT`, `QONTINUI_RUNNER_CONTEXT`,
/// the continuation-verdict forward and both marker strips were missing too.
fn pty_child_command(
    config_dir: Option<&str>,
) -> (portable_pty::CommandBuilder, Option<std::path::PathBuf>) {
    use crate::install_effects_producer::intercept::shim_materializer;
    use crate::terminal::session::TerminalSession;

    let mut cmd = portable_pty::CommandBuilder::new_default_prog();
    TerminalSession::apply_base_child_env(&mut cmd);

    // The identity seam's PATH prepend, applied through the seam's OWN
    // function — but only when the dir it installs is already there. The
    // resolver is the read-only twin (`identity_dir_if_materialized`): the
    // materializing one writes four scripts, copies two exes, refreshes a
    // liveness mtime and can trigger the orphan sweep, and a diagnostic that
    // materializes the thing it describes changes the answer by asking the
    // question. `None` is the same fail-open case in which the real seam
    // prepends nothing either, and the freshness line says which happened.
    let identity_shim = shim_materializer::identity_dir_if_materialized(&std::env::temp_dir());
    if let Some(dir) = &identity_shim {
        TerminalSession::apply_identity_path_shim(&mut cmd, dir);
    }

    TerminalSession::finalize_child_env(&mut cmd, config_dir, false);
    (cmd, identity_shim)
}

/// The PTY seam.
///
/// `CommandBuilder` keeps base env and overrides in ONE map, so a cleared name
/// is simply ABSENT rather than present-and-cleared — the same asymmetry
/// `terminal::assert_credentials_scrubbed_pty` documents. So the credential
/// names are SEEDED first: only then does "absent afterwards" mean "this seam
/// removed it" rather than "it was never there", and only then would a
/// regressed scrub show up (as a `(probe)` value in `sets`) instead of
/// vanishing into a vacuous pass.
fn pty_seam(fp: &EnvFingerprinter, config_dir: Option<&str>) -> SeamEnvReport {
    let mut cmd = portable_pty::CommandBuilder::new_default_prog();
    for name in crate::terminal::CREDENTIAL_VALUE_ENV_VARS {
        cmd.env(name, "(probe)");
    }
    crate::terminal::session::TerminalSession::finalize_child_env(&mut cmd, config_dir, false);

    let mut sets: Vec<EnvVarReading> = cmd
        .iter_extra_env_as_str()
        .map(|(k, v)| EnvVarReading::classify(fp, k, v))
        .collect();
    sets.sort_by(|a, b| a.name.cmp(&b.name));
    let clears: Vec<String> = crate::terminal::CREDENTIAL_VALUE_ENV_VARS
        .iter()
        .filter(|n| cmd.get_env(n).is_none())
        .map(|n| (*n).to_string())
        .collect();

    SeamEnvReport {
        seam: "session::TerminalSession::finalize_child_env".to_string(),
        command_type: "portable_pty::CommandBuilder".to_string(),
        scrub_wrapper: "scrub_credential_env_pty".to_string(),
        sets,
        clears,
    }
}

/// All eight spawn seams from the table in [`crate::terminal`], each captured by
/// CALLING its own extracted env-construction function on a throwaway
/// `Command` and reading the overrides back.
///
/// Nothing here restates what a seam does — that would be the same second-copy
/// defect the whole report exists to avoid, one level down. The functions are
/// extracted and unit-tested precisely so this is possible; none of them
/// spawns, and the spawn path itself is untouched.
fn seam_reports(fp: &EnvFingerprinter, config_dir: Option<&str>) -> Vec<SeamEnvReport> {
    let mut headless = crate::process_helpers::tokio_no_window("claude");
    // No port argument to supply any more: the seam resolves its own, so the
    // report captures the number production actually ships instead of a second
    // copy of that decision made here. The local this replaced read
    // `mcp::types::get_mcp_api_port()` — the DESIRED port — which would have
    // rendered a passing row for a seam pointing sessions at a dead socket.
    crate::agent_runtime::finalize_headless_child_env(&mut headless);

    let mut claude_session = std::process::Command::new("claude");
    crate::claude_session::session::ClaudeSession::finalize_child_env(
        &mut claude_session,
        config_dir,
        "(config-report probe)",
    );

    let mut ai_child = std::process::Command::new("claude");
    crate::ai_provider::process::prepare_ai_child_env(&mut ai_child);

    vec![
        pty_seam(fp, config_dir),
        tokio_seam(
            fp,
            "agent_runtime::finalize_headless_child_env",
            "scrub_credential_env_tokio",
            &headless,
        ),
        std_seam(
            fp,
            "claude_session::session::finalize_child_env",
            "scrub_credential_env_std",
            &claude_session,
        ),
        std_seam(
            fp,
            "claude_session::runner::build_inline_child_command",
            "scrub_credential_env_std",
            // The program is a placeholder: this seam fingerprints the child's
            // ENVIRONMENT (credential scrubbing), and `build_inline_child_command`
            // touches env identically whatever it is about to exec. The real
            // program is chosen per-platform by
            // `launch_spec::render_program_and_argv`; do not read this literal as
            // the report asserting what gets spawned.
            &crate::claude_session::runner::build_inline_child_command("cmd.exe", &[], "."),
        ),
        std_seam(
            fp,
            "ai_provider::process::prepare_ai_child_env",
            "scrub_credential_env_std",
            &ai_child,
        ),
        std_seam(
            fp,
            "ai_provider::claude_cli::build_scorer_command",
            "scrub_credential_env_std",
            &crate::ai_provider::claude_cli::build_scorer_command("claude", config_dir),
        ),
        tokio_seam(
            fp,
            "orchestration_loop::fix_agent::build_fix_agent_command",
            "scrub_credential_env_tokio",
            &crate::orchestration_loop::fix_agent::build_fix_agent_command(
                "(config-report probe)",
                "(config-report probe)",
            ),
        ),
        tokio_seam(
            fp,
            "commands::command_interpreter::build_interpret_command",
            "scrub_credential_env_tokio",
            &crate::commands::command_interpreter::build_interpret_command("(config-report probe)"),
        ),
    ]
}

/// Capture the whole env-generation section from live runner state.
///
/// # The three generations, and why G3 is not the same list as G1
///
/// - **G1 `runner_process`** — `std::env::vars_os()`, via
///   [`lossy_env_pairs`]. Frozen when this process
///   started; every ad-hoc `std::env::var` call in the runtime reads it. It is
///   `vars_os` rather than `vars` because `vars` PANICS on a name or value that
///   is not valid Unicode, and this function runs inside a `#[tauri::command]`
///   — see that helper for the whole argument.
/// - **G2 `launch_snapshot`** — the typed `RunnerLaunchEnv` read ONCE in
///   `main()`. A parsed subset, so it is displayed but never value-diffed
///   (see [`launch_snapshot_pairs`]); its staleness is measured against a
///   re-read of itself.
/// - **G3 `pty_child`** — what a PTY child spawned RIGHT NOW inherits.
///   `portable_pty::CommandBuilder` seeds from `std::env::vars_os()` **and on
///   Windows re-reads the HKLM/HKCU `Environment` registry keys OVER those
///   entries**, so this generation can be genuinely FRESHER than G1: an
///   operator's user-scope change lands here without ever touching the running
///   process. Then the production spawn seam runs — see [`pty_child_command`],
///   which CALLS `apply_base_child_env`, the identity PATH shim and
///   `finalize_child_env` rather than restating any of them — which is why the
///   credential scrub shows up in the divergence as three removals and the
///   shim-prepended `PATH` as a change.
///
/// # What is honestly NOT here, and why the row says so out loud
///
/// Two things, and the report names both rather than letting an unqualified
/// claim stand:
///
/// 1. **The parts of the spawn seam that WRITE.** The identity seam's
///    per-terminal session/terminal ids and its coord-mcp `--mcp-config`
///    provisioning mint state; the install-interception shim (default dark)
///    materializes a bin dir; `extra_env` is per-caller. Replicating them would
///    mean a diagnostic minting session ids and nonces, so G3 excludes them and
///    [`G3_DESCRIBES`] lists them by name. The one borderline case — the
///    identity PATH shim — is included when its dir is ALREADY materialized (a
///    pure `stat` establishes that) and reported as absent when it is not,
///    because `PATH` decides which binary a child resolves and an unqualified
///    "this is what a child gets" that silently omitted it was worse than a
///    bounded claim.
/// 2. **The historical env of an ALREADY-RUNNING PTY child.** Reading another
///    process's environment block is a debugger-class operation, and recording
///    it at spawn would mean editing the spawn path. G3 is therefore "what a
///    child spawned now receives" and is labelled as such — an already-open
///    terminal holds whatever G3 said at ITS spawn time, which is the very
///    staleness the interpretation line spells out.
pub(crate) fn env_generations_section(
    config_dir: Option<&str>,
    now: DateTime<Utc>,
) -> EnvGenerations {
    let fp = EnvFingerprinter::new();

    let g1 = EnvGeneration::capture(
        &fp,
        EnvGenerationSpec {
            id: "G1",
            name: "runner_process",
            describes: "this runner process's own environment (`std::env::vars_os`)",
            freshness:
                "frozen when the runner process started — every ad-hoc `std::env::var` read in the \
         runtime sees THIS, so it moves only on a runner restart",
            is_full_env: true,
        },
        now,
        // NOT `std::env::vars()`: it panics on non-Unicode, and this runs
        // under a `#[tauri::command]`.
        lossy_env_pairs(std::env::vars_os()),
    );

    let g2 = match crate::launch_env::first_read() {
        Some((snapshot, captured_at)) => EnvGeneration::capture(
            &fp,
            EnvGenerationSpec {
                id: "G2",
                name: "launch_snapshot",
                describes: "`launch_env::RunnerLaunchEnv`, read exactly ONCE in `main()`",
                freshness:
                    "as old as this process; consumers pull the typed value from here rather than \
             re-reading, so it cannot be fresher than G1 and can be staler",
                is_full_env: false,
            },
            *captured_at,
            launch_snapshot_pairs(snapshot),
        ),
        // `read()` has never run here. UNKNOWN, rendered as an empty PARSED
        // generation with a freshness line that says why — not as agreement.
        None => EnvGeneration::capture(
            &fp,
            EnvGenerationSpec {
                id: "G2",
                name: "launch_snapshot",
                describes: "`launch_env::RunnerLaunchEnv`, read exactly ONCE in `main()`",
                freshness:
                    "NOT TAKEN in this process — `RunnerLaunchEnv::read()` has never run here, so \
             there is no launch generation to compare against",
                is_full_env: false,
            },
            now,
            Vec::<(String, String)>::new(),
        ),
    };

    let (pty, identity_shim) = pty_child_command(config_dir);
    let g3 = EnvGeneration::capture(
        &fp,
        EnvGenerationSpec {
            id: "G3",
            name: "pty_child",
            describes: G3_DESCRIBES,
            freshness: if identity_shim.is_some() {
                G3_FRESHNESS_WITH_SHIM
            } else {
                G3_FRESHNESS_NO_SHIM
            },
            is_full_env: true,
        },
        now,
        pty.iter_full_env_as_str()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<Vec<_>>(),
    );

    let divergence = diff_generations(
        &g1,
        &g3,
        "READ THIS AS: anything listed above is a variable whose value a PTY child gets \
         DIFFERENTLY from the runner process itself — either because the runner is stale (a \
         Windows user-scope change lands in G3 without a restart) or because the spawn seam \
         deliberately changes it. A change reaches a Claude tool call only after all three: \
         restart the runner (G1), open a NEW terminal (G3), and only then does a Bash-tool \
         grandchild — which inherits its terminal's frozen copy — see it. Removals of the \
         credential names are the spawn seam's scrub working as designed, not drift; a changed \
         PATH is the identity-shim dir prepended, which is WHY a `claude` in a pane resolves to \
         the shim; TERM / QONTINUI_RUNNER_* / the git-credential vars are the seam setting them. \
         What this comparison does NOT cover is listed on G3's own `describes` line — the \
         seam steps that would have to WRITE to be reproduced here.",
    );

    EnvGenerations {
        generations: vec![g1, g2, g3],
        divergences: vec![divergence],
        launch_drift: launch_drift(&fp),
        seams: seam_reports(&fp, config_dir),
    }
}

/// Layer 6 — the launch snapshot, and whether it still matches a re-read.
///
/// The VALUE is the drift verdict rather than a dump of the struct: the fields
/// are in the env-generation section, and what a reader needs on the layer row
/// is the one bit that decides their next action — "is what this runner is
/// acting on still what the environment says?".
pub(crate) fn launch_env_snapshot_reading(
    section: &EnvGenerations,
    captured_at: DateTime<Utc>,
) -> LayerReading {
    match &section.launch_drift {
        Some(drift) if drift.differing.is_empty() => LayerReading::known(
            format!(
                "{} launch fields, all still equal to a re-read of this process's env",
                drift.fields_compared
            ),
            "launch_env::RunnerLaunchEnv::read (first call — `main`)",
            captured_at,
        ),
        Some(drift) => LayerReading::known(
            format!(
                "{} of {} launch fields DIVERGE from a re-read ({}) — the runner is acting on \
                 the launch value",
                drift.differing.len(),
                drift.fields_compared,
                drift
                    .differing
                    .iter()
                    .map(|f| f.field.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "launch_env::RunnerLaunchEnv::read (first call — `main`)",
            captured_at,
        ),
        // No first read in this process. UNKNOWN — never "no drift".
        None => LayerReading::unknown(
            "`launch_env::RunnerLaunchEnv::read()` has not run in this process, so there is no \
             launch generation to report; this is the absence of a reading, not a finding that \
             the snapshot is current",
            captured_at,
        ),
    }
}

/// Layer 7 — the generation the scattered `std::env::var` call sites see.
///
/// The whole content of this layer is *which* generation that is, plus its
/// size. It is `Known` on the runner app because the answer is never in doubt:
/// an ad-hoc read sees this process's env, and the interesting question — does
/// that agree with what a child gets? — is the divergence section.
pub(crate) fn adhoc_env_reads_reading(
    section: &EnvGenerations,
    captured_at: DateTime<Utc>,
) -> LayerReading {
    let g1 = section
        .generations
        .iter()
        .find(|g| g.name == "runner_process");
    match g1 {
        Some(g) => {
            let divergence = section
                .divergences
                .first()
                .map(|d| d.deltas.len())
                .unwrap_or(0);
            LayerReading::known(
                format!(
                    "{} variables ({} withheld) in this process's env; {} differ from what a PTY \
                     child spawned now would get",
                    g.vars.len(),
                    g.withheld_count(),
                    divergence
                ),
                "std::env::vars (runner process — generation G1)",
                captured_at,
            )
        }
        None => LayerReading::unknown(
            "the env-generation capture produced no `runner_process` generation — this is a \
             wiring bug in the config-report command",
            captured_at,
        ),
    }
}

/// Layer 8 — which supervisor-injected variables this runner actually received.
///
/// # Why this is `Known` on an `ExternalBinary` layer
///
/// The layer is owned by another executable, and this row makes no claim about
/// that executable: it does not read the supervisor's source, its config, or
/// its state. It reports an OBSERVATION of this process's own environment —
/// which of the names the supervisor is known to stamp are present here, and
/// therefore whether this runner was launched by one at all. That is a fact
/// this observer genuinely holds, and it is the fact an operator needs
/// ("is this runner supervised, and with which knobs?").
///
/// Values are deliberately not on this row — they are in the env-generation
/// table, classified, where a credential-shaped one is withheld.
pub(crate) fn supervisor_injected_reading(
    section: &EnvGenerations,
    captured_at: DateTime<Utc>,
) -> LayerReading {
    let Some(g1) = section
        .generations
        .iter()
        .find(|g| g.name == "runner_process")
    else {
        return LayerReading::unknown(
            "the env-generation capture produced no `runner_process` generation — this is a \
             wiring bug in the config-report command",
            captured_at,
        );
    };
    let present: Vec<&str> = SUPERVISOR_INJECTED_ENV_VARS
        .iter()
        .filter(|n| g1.get(n).is_some())
        .copied()
        .collect();
    let value = if present.is_empty() {
        format!(
            "0 of {} supervisor-injected variables present — this runner was not launched by \
             the dev supervisor, or it injected none",
            SUPERVISOR_INJECTED_ENV_VARS.len()
        )
    } else {
        format!(
            "{} of {} present: {}",
            present.len(),
            SUPERVISOR_INJECTED_ENV_VARS.len(),
            present.join(", ")
        )
    };
    LayerReading::known(
        value,
        "observed in this process's env (generation G1) — the supervisor's own state is not read",
        captured_at,
    )
}

// ===========================================================================
// Phase 4 — the two TIME-VARYING coord-served layers (9, 10).
//
// Both are process-global caches a background loop refreshes. Their value can
// change with NO restart of anything, which is why every reading in this report
// carries a `captured_at` — and why the two of them below take care to keep
// "when I read it" and "when the cache last refreshed" as SEPARATE facts.
// ===========================================================================

/// Layer 9 — the coord-served prompt/policy documents this runner currently
/// holds.
///
/// Pure over [`crate::prompt_library::cache_health`]'s snapshot, so every arm
/// is table-testable without a network or a populated cache.
///
/// # The two clocks, and why the row prints both
///
/// The reading's own `captured_at` says WHEN THIS REPORT LOOKED. The cache's
/// age says WHEN THE VALUE IT LOOKED AT WAS LAST CONFIRMED AGAINST COORD.
/// Those are different facts about a layer fetched over the network, and a row
/// carrying only the first would vouch, with a timestamp of seconds ago, for a
/// document last seen before coord went down. So the age is IN the value.
///
/// # Why an empty cache is KNOWN and not UNKNOWN
///
/// Same discipline as layer 11's "no account". The layer WAS read; the answer
/// is that this runner holds nothing from coord. That is a fact about this
/// process, not a limit of the observer, and the value says exactly that — it
/// never renders as "coord's library is empty", which is a claim this runner
/// has no standing to make.
pub(crate) fn coord_prompt_documents_reading(
    health: crate::prompt_library::PromptLibraryCacheHealth,
    captured_at: DateTime<Utc>,
) -> LayerReading {
    let value = match health.age_ms {
        None => format!(
            "0 documents — NOTHING has been fetched from coord in this process. This is what \
             this runner HOLDS, not a finding that coord's library is empty (TTL {} ms)",
            health.ttl_ms
        ),
        Some(age) => format!(
            "{} document(s), last confirmed against coord {} ms ago ({} — TTL {} ms), ETag {}",
            health.documents,
            age,
            if health.fresh {
                "fresh: the next read serves from cache"
            } else {
                "STALE: the next read re-fetches"
            },
            health.ttl_ms,
            if health.has_etag {
                "stored (a refresh costs one conditional 304)"
            } else {
                "absent (a refresh re-hydrates every document)"
            },
        ),
    };
    LayerReading::known(
        value,
        "prompt_library::cache_health (process-global TTL cache — the age is the CACHE's own \
         last-refresh, not this report's read time)",
        captured_at,
    )
}

/// Layer 10 — the fleet-policy dial, as it stands at `captured_at`.
///
/// Pure over [`crate::mcp::fleet_policy_poller::dial_snapshot`]'s value.
///
/// # The freshness asymmetry, reported rather than smoothed over
///
/// Four caches sit behind one poll loop and they do not agree about what they
/// can tell you. The session-briefing cache carries `fetched_at` +
/// `provenance` per document, so this row prints them. The other three are a
/// bare `RwLock<T>` holding a value and nothing else, so their last-refresh
/// time is genuinely unavailable — and the row says `UNKNOWN` for it rather
/// than substituting `captured_at`. Substituting would make every dial look
/// freshly polled at the instant of the report, which is the single most
/// convincing wrong answer this layer could give: it is exactly what a runner
/// that lost coord an hour ago would print.
///
/// # Why a value equal to the resting default is called out
///
/// `off` means EITHER "the fleet says off" OR "no poll has ever succeeded", and
/// caches 1 and 3 cannot distinguish them. Rendering a bare `off` would let a
/// reader conclude the first when the truth is the second, so the row names the
/// ambiguity in place.
pub(crate) fn fleet_policy_dial_reading(
    dial: &crate::mcp::fleet_policy_poller::FleetPolicyDial,
    captured_at: DateTime<Utc>,
) -> LayerReading {
    fn floor(v: Option<u64>) -> String {
        // `None` is "the fleet has NO OPINION" and must never render as 0 — a
        // zero floor disables the guard it names, so printing one would turn a
        // missing fleet term into a silently disabled protection.
        v.map(|b| b.to_string())
            .unwrap_or_else(|| "(no fleet opinion)".to_string())
    }
    let ambiguous = |value: &str, default: &str| {
        if value == default {
            " [= resting default: EITHER the fleet says so OR no poll has ever succeeded — this \
             cache cannot tell them apart]"
        } else {
            ""
        }
        .to_string()
    };

    let briefings = dial
        .briefings
        .iter()
        .map(|b| {
            if !b.present {
                format!(
                    "{}=absent (renderer falls back to the compiled-in builtin)",
                    b.name
                )
            } else {
                format!(
                    "{}=v{} fetched_at {} ({})",
                    b.name,
                    b.version
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "?".into()),
                    b.fetched_at
                        .clone()
                        .unwrap_or_else(|| "UNKNOWN".to_string()),
                    b.provenance.unwrap_or("UNKNOWN"),
                )
            }
        })
        .collect::<Vec<_>>()
        .join("; ");

    let value = format!(
        "install_interception={}{} | session floors host warn={} crit={}, wsl warn={} crit={} | \
         plan_capture={}{} (armed at {:?}) | briefings: {} | poll interval {} ms | last refresh of \
         the first three caches: {}",
        dial.install_intercept_mode,
        ambiguous(&dial.install_intercept_mode, dial.install_intercept_default),
        floor(dial.host_warn_free_bytes),
        floor(dial.host_critical_free_bytes),
        floor(dial.wsl_warn_free_bytes),
        floor(dial.wsl_critical_free_bytes),
        dial.plan_capture_level,
        ambiguous(&dial.plan_capture_level, dial.plan_capture_default),
        dial.plan_capture_record_level,
        briefings,
        dial.poll_interval_ms,
        if dial.caches_expose_refresh_time {
            "see above"
        } else {
            "UNKNOWN — those caches hold a value and no stamp; the report REFUSES to substitute \
             its own read time"
        },
    );
    LayerReading::known(
        value,
        "mcp::fleet_policy_poller::dial_snapshot (four process-global caches, one poll loop — \
         TIME-VARYING with no restart)",
        captured_at,
    )
}

// ===========================================================================
// Phase 4 — the CARRIERS (12, 13, 14).
//
// Configuration that reaches a session by argv, or by a file the runner writes,
// rather than by the environment or by the session's own discovery. All three
// are credential-adjacent, so all three report IDENTITY — path, existence,
// ownership, verdict — and never content. Every value that could carry one goes
// through `EnvVarReading::classify` first.
// ===========================================================================

/// Layer 12 — the `--settings` carrier the runner materializes and the identity
/// shim appends to a `claude` invocation.
///
/// Pure over its arguments so every arm — both registration variants, present
/// and absent on disk, injected and not injected into this process's env — is
/// table-testable.
///
/// # Why "does the file exist" is the load-bearing fact and not a detail
///
/// The shim's gate (`bin/qontinui_shim::identity_settings_args`) appends
/// `--settings <path>` only when the path is non-empty **and**
/// `Path::is_file()`. It is fail-OPEN: a missing file means the session simply
/// launches without the hook, silently, with identity still pinned by
/// `--session-id`. So "the runner resolved a path" and "the session got the
/// hook" are different claims, and only the second one matters — which makes
/// the existence bit, not the path, the answer to "why did my SessionStart hook
/// not run?".
///
/// # Why this does not call `materialize`
///
/// `materialize` WRITES — four scripts, a settings file and up to three
/// `chmod`s. A diagnostic that materializes the thing it is describing changes
/// the answer by asking the question, and would report `exists: true` on a
/// machine where the spawn path had never succeeded. So the path comes from
/// [`crate::session::claude_hook::settings_path`], which is the ONE definition
/// the writer itself uses, and the existence bit comes from a plain `stat`.
pub(crate) fn claude_settings_carrier_reading(
    reg: crate::session::claude_hook::StopHookRegistration,
    path: &std::path::Path,
    exists: bool,
    env_injected: Option<&str>,
    captured_at: DateTime<Utc>,
) -> LayerReading {
    let fp = EnvFingerprinter::new();
    let injected = match env_injected {
        None => format!(
            "not set in THIS process's env (expected — the runner exports {} onto a spawned \
             CHILD, not onto itself)",
            crate::session::claude_hook::CLAUDE_SETTINGS_ENV
        ),
        Some(v) => format!(
            "{}={} in this process's env — this runner is itself running inside a shimmed session",
            crate::session::claude_hook::CLAUDE_SETTINGS_ENV,
            EnvVarReading::classify(&fp, crate::session::claude_hook::CLAUDE_SETTINGS_ENV, v)
                .value
                .detail()
        ),
    };
    LayerReading::known(
        format!(
            "{} — on disk: {}; variant `{}`. Two appenders read this ONE file: the identity \
             shim, for a PATH-resolved `claude`, and \
             `claude_hook::direct_spawn_settings_args`, for an autonomous spawn that execs \
             `claude` directly with no shim in the chain. BOTH append `--settings` ONLY when \
             that file exists (fail-open), so a `false` here means spawned sessions get NO \
             hook: no SessionStart confirmation, no SessionStart policy injection, no \
             PreCompact, and no Stop. {}",
            path.display(),
            exists,
            reg.as_str(),
            injected,
        ),
        format!(
            "session::claude_hook::settings_path(session_restore_dir(), \
             StopHookRegistration::from_env()) — variant read live from env {}",
            crate::mcp::continuation_verdict::FLAG_ENV
        ),
        captured_at,
    )
}

/// Layer 13 — the `--mcp-config` carrier, built by the SEPARATE `qontinui-shim`
/// binary.
///
/// # Why this is `Known` on an `ExternalBinary` layer
///
/// Exactly layer 8's argument. This row makes no claim about the shim binary:
/// it does not read its source, its argv, or its state, and it does not restate
/// `identity_mcp_config_args`'s rule as a second copy — that function is in
/// another binary's address space and calling it from here is not possible by
/// construction. What this observer genuinely holds is the shim's INPUT: the
/// environment variable the shim reads, as it exists here, and whether the path
/// it names is a file. That is the fact an operator needs ("did this session
/// get coord-mcp, and if not, at which end did it break?").
///
/// The runner is not usually its own shimmed child, so "absent" is the expected
/// reading on a hand-launched runner and is stated as such rather than as a
/// fault — the same posture layer 8 takes toward an unsupervised runner.
///
/// The value goes through [`EnvVarReading::classify`] before it is rendered.
/// The path itself is not a credential, but the file it names carries a bearer
/// and a proxy nonce, and this layer is close enough to that material that
/// classifying first costs nothing and removes the question.
pub(crate) fn mcp_config_carrier_reading(
    env_value: Option<&str>,
    path_is_file: bool,
    captured_at: DateTime<Utc>,
) -> LayerReading {
    let fp = EnvFingerprinter::new();
    let value = match env_value {
        None => format!(
            "{} is not set in this process's env — this runner is not itself running inside a \
             runner-spawned session, which is the expected reading for a hand-launched runner. \
             The argv is assembled by the `qontinui-shim` binary, which appends \
             `--mcp-config <path>` only when that variable names an existing file (fail-open)",
            crate::coord_mcp::MCP_CONFIG_ENV
        ),
        Some(v) => format!(
            "{}={} — names an existing file: {}. The `qontinui-shim` binary appends \
             `--mcp-config <path>` only when that is true (fail-open)",
            crate::coord_mcp::MCP_CONFIG_ENV,
            EnvVarReading::classify(&fp, crate::coord_mcp::MCP_CONFIG_ENV, v)
                .value
                .detail(),
            path_is_file,
        ),
    };
    LayerReading::known(
        value,
        "observed in this process's env (generation G1) — the `qontinui-shim` binary's argv \
         construction is not in this address space and is not restated here",
        captured_at,
    )
}

/// Layer 14 — the shared-root `.mcp.json`, including the write guard's verdict.
///
/// Pure over [`crate::coord_mcp::McpJsonReport`], which is itself a read-only
/// shape classification: no bearer, no proxy nonce, and no field able to carry
/// one.
///
/// # The verdict is the point of this row (the plan's D2)
///
/// `coord_mcp::coord_mcp_safe_to_write` decides whether this runner may rewrite
/// the umbrella-root config every session at the root reads. Its refusal path
/// emits a `warn!` and returns `false` — so an ephemeral or secondary runner
/// CORRECTLY declining to strand every root-opened session on a dying port was,
/// until this row existed, indistinguishable from a runner that never tried.
/// One is the guard working; the other is a bug. The report now says which.
///
/// The verdict is taken from that guard, not re-derived: a second copy of
/// the primary/secondary test would compile, agree today, and start lying the
/// first time the guard moved — which is the defect class this whole report
/// exists to expose.
///
/// Specifically it is taken from `coord_mcp::coord_mcp_write_verdict`, the pure
/// core `coord_mcp_safe_to_write` decides on and every writer reaches through.
/// Asking the WRAPPER meant that opening this report on a secondary emitted
/// `coord_mcp: REFUSING to write …` into the runner log the operator was about
/// to read — a log line describing a write nobody attempted, manufactured by the
/// act of reporting. One implementation of the rule, two doors: the warning
/// belongs to the write attempt, not to the observation.
///
/// # The port comparison is against the BOUND port, and refuses to guess
///
/// `McpJsonReport::this_runner_port` is an `Option<u16>` filled by
/// `coord_mcp::resolve_bound_api_port` — the same resolver `coord_doctor`
/// injects. It is deliberately not `mcp::types::get_mcp_api_port()`, which
/// returns the port this runner *wanted*: `mcp_api`'s bind loop falls back
/// across `[port, port+1, port+2]` when a port is blocked (the Windows
/// zombie-socket path this layer exists to diagnose), and comparing a
/// correctly-rewritten `.mcp.json` against the desired port produces a false
/// alarm on a healthy runner while a stale file naming the desired port
/// produces a false ALL-CLEAR on the stranded state.
///
/// `None` renders as an explicit UNKNOWN. A row that cannot establish the
/// bound port must say the comparison is unavailable — substituting the env
/// value to keep the sentence grammatical is precisely how the inverted verdict
/// gets printed.
pub(crate) fn mcp_json_reading(
    report: &crate::coord_mcp::McpJsonReport,
    captured_at: DateTime<Utc>,
) -> LayerReading {
    use crate::coord_mcp::McpJsonShape;

    // No umbrella root resolved. That is the OBSERVER failing to establish
    // where the shared config would be — a production install, or a checkout
    // the resolver could not anchor — not a finding that the file is absent.
    if report.shape == McpJsonShape::NoRoot {
        return LayerReading::unknown(
            "`workspace_paths::workspace_root()` resolved no umbrella root, so this runner \
             cannot say where the shared `.mcp.json` would be. This is the absence of a \
             reading, NOT a finding that there is no shared root config",
            captured_at,
        );
    }

    // Two of the insertions below carry text this function did not choose:
    // `instance_name` is the RAW `QONTINUI_INSTANCE_NAME` environment value
    // (`instance::instance_name()`), and `read_error` is an OS or `serde`
    // message. Both are bounded at their source — the `serde` arm emits
    // `JSON <category> error at line L column C` and never the offending value,
    // and `.mcp.json` is the file carrying the bearer and the proxy nonce this
    // row promises are structurally unreportable — and both are classified here
    // anyway, as PROSE. That is the same two-independent-controls discipline
    // `settings_struct_reading` documents, and `classify_free_text` is the
    // right door for it: `classify` examines the value as a whole, so it is
    // inert on a sentence.
    let fp = EnvFingerprinter::new();
    let classified = |name: &str, text: &str| EnvVarReading::classify_free_text(&fp, name, text);
    let instance = |n: &str| match &classified("QONTINUI_INSTANCE_NAME", n).value {
        // Quoted, so an ordinary name still reads `SECONDARY "temp-abc"`.
        EnvValue::Shown { .. } => format!("{n:?}"),
        withheld => withheld.detail(),
    };
    let owner = match (&report.instance_name, report.owns_shared_root_state) {
        (None, true) => "PRIMARY (owns shared root state)".to_string(),
        (Some(n), true) => format!(
            "named instance {} that still owns shared root state",
            instance(n)
        ),
        (None, false) => {
            "a NAMELESS SECONDARY (no QONTINUI_INSTANCE_NAME, detected by port) — fails closed"
                .to_string()
        }
        (Some(n), false) => format!("SECONDARY {} — does NOT own shared root state", instance(n)),
    };
    let port_note = match (report.proxy_port, report.this_runner_port) {
        (None, _) => "the file names no loopback proxy port (absent, unparseable, or the \
                      static-bearer agent shape)"
            .to_string(),
        // The comparison this row exists to make cannot be made. Say so —
        // NEVER substitute the DESIRED port (`QONTINUI_PORT` / `MCP_API_PORT`)
        // to manufacture a verdict: on a runner that fell back off a blocked
        // port that substitution inverts the answer in both directions, which
        // is the one failure this layer must not produce.
        (Some(p), None) => format!(
            "names port {p}. THIS RUNNER'S BOUND PORT IS UNKNOWN — no Tauri runtime / managed \
             AppState is reachable from here, so `coord_mcp::resolve_bound_api_port` fails \
             closed and the comparison is UNAVAILABLE. This is the absence of a check, not a \
             finding that the port matches"
        ),
        (Some(p), Some(bound)) if p == bound => {
            format!("names port {p}, which IS this runner's BOUND API port")
        }
        (Some(p), Some(bound)) => format!(
            "names port {p}, which is NOT this runner's BOUND API port ({bound}) — root-opened \
             sessions are pointed at a different runner"
        ),
    };
    let read_note = match &report.read_error {
        None => String::new(),
        Some(e) => format!(
            " The file IS on disk and could not be read or parsed ({}) — `unparseable` here \
             means present-and-unusable, never missing.",
            classified("mcp_json_read_error", e).value.detail(),
        ),
    };

    LayerReading::known(
        format!(
            "{} — on disk: {}; shape: {};{} {}. This runner is {}. coord_mcp_safe_to_write: {}",
            report.path.as_deref().unwrap_or("(unresolved)"),
            report.exists,
            report.shape.as_str(),
            read_note,
            port_note,
            owner,
            if report.safe_to_write {
                "ALLOWED — this runner may rewrite the shared root config"
            } else {
                "REFUSED — this runner will NOT write it (a secondary protecting the primary's \
                 shared state, or a foreign/agent config it must not clobber). That refusal was \
                 previously visible only as a runner-log warning"
            },
        ),
        "coord_mcp::mcp_json_report — the verdict is coord_mcp_write_verdict's OWN return value \
         (coord_mcp_safe_to_write's pure core, asked WITHOUT its write-attempt warn!) and the \
         port comparison is against coord_mcp::resolve_bound_api_port (the ACTUALLY-BOUND port, \
         not the configured one); the bearer and the proxy nonce the file carries are \
         structurally unreportable",
        captured_at,
    )
}

/// Everything the report derives from the `settings.json` DOCUMENT, resolved
/// from ONE non-mutating read.
///
/// Three layers need that document (1, 5 and 11) and every one of them used to
/// reach it through a different door. Two of those doors were writers, which is
/// the whole reason this type exists — see [`settings_derived_inputs`].
pub(crate) struct SettingsDerivedInputs {
    /// Layer 1 — the whole-file provenance verdict.
    pub provenance: crate::settings::SettingsProvenance,
    /// Layer 1 — the bounded load error, present only for `unreadable`.
    pub error: Option<String>,
    /// Layer 5 — the four rungs of the backend-URL resolution.
    pub api_base_url: ApiBaseUrlInputs,
    /// Layer 11 — the resolved `CLAUDE_CONFIG_DIR` and its selection arm.
    pub claude_config_dir: (Option<String>, ClaudeConfigDirSource),
    /// Layer 14 — the umbrella root, resolved ONCE off the non-mutating door.
    ///
    /// `workspace_paths::workspace_root()` reads `paths.workspace_root` through
    /// `config_facade::get_setting` → `settings::load_settings_full`, so it is
    /// a WRITER like the other two doors above. Layer 14 needed it twice —
    /// `coord_mcp::mcp_json_report` for the path and the write guard for its
    /// verdict — which is how one config report entered the settings writer
    /// twice after layer 5 had already been moved off it. Resolved here from the
    /// document this function already read, and injected.
    pub workspace_root: Option<std::path::PathBuf>,
}

/// Read `settings.json` ONCE, non-mutatingly, and derive every layer that needs
/// it.
///
/// # Why this is one function and not three call sites
///
/// `read_settings_from_disk` returns the same `LoadedSettings` shape as
/// `load_settings_full` and is the base that reader itself starts from, but it
/// writes nothing: no `claude-accounts.json` migration, no `local_user_id` mint,
/// no `save_settings` of the operator's real file, no keyring read.
/// `load_settings` cannot resolve layer 1 at all — it discards provenance by
/// design and is documented as such at its definition.
///
/// Moving layer 1 onto the non-mutating reader was not enough, and that is the
/// lesson this function encodes. Layer 5 sat one line below it calling
/// `api_config::gather_api_base_url_inputs()`, whose first statement is
/// `settings::load_settings()` → `load_settings_full()` — the reader layer 1 had
/// just been moved OFF, reached through a second door. On a machine with a
/// present, parsing `settings.json` whose `local_user_id` is empty (settings
/// hand-copied from another machine with the identity key stripped, or a
/// boot-time persist that failed against a read-only config dir), opening the
/// config report to find out why the runner cannot find its settings MINTED A
/// UUID AND REWROTE `settings.json` — while layer 1's `source` line went on
/// saying `settings::read_settings_from_disk`, so the report concealed the write
/// it had just performed. So the rule is structural now: **the report reads the
/// settings document exactly once, here, and no layer resolver is handed a
/// function that can reach `load_settings_full`.**
///
/// That rule immediately found a THIRD and FOURTH entry nobody had named. Layer
/// 14's `coord_mcp::mcp_json_report` resolved the umbrella root through
/// `workspace_paths::workspace_root()`, which reads `paths.workspace_root` via
/// `config_facade::get_setting` → `load_settings_full` — and it needed the root
/// twice, once for the `.mcp.json` path and once for the write guard. Nothing in
/// "resolve the workspace root" says it is a settings write, which is exactly
/// why enumerating the doors by hand does not work and the counter does. The
/// root is therefore resolved HERE, once, off `workspace_root_from`, and
/// injected.
///
/// # The two overlays, and why each is the READ variant
///
/// - **web-integration** (layer 5): applied through
///   `settings::apply_web_integration_env_overlay`, the same function
///   `load_settings_full` calls. Without it a whitespace-only
///   `QONTINUI_WEB_BACKEND_URL` would make this door resolve the DISK url while
///   the runner resolved the build default. It is CALLED, never restated.
/// - **the machine-global Claude roster** (layer 11): applied from
///   `claude_accounts::load()` — the plain READ — rather than
///   `load_with_migration()`, which runs the one-shot seed migration and WRITES
///   `claude-accounts.json`. The overlay itself is
///   `claude_accounts::apply_roster_overlay`, unchanged, because it overwrites
///   `ai.claude_cli.{account_selection_mode, config_dir}` UNCONDITIONALLY when
///   the roster exists and the per-instance copies in settings.json are stale
///   shadows.
///
/// Layer 11's cost of taking the read variant is stated in
/// [`claude_config_dir_reading`]: on a machine where the seed has NOT run, this
/// door and the runner's own can resolve different accounts, and the arm cannot
/// say so.
///
/// # What does not escape
///
/// The ~90-field `Settings` is dropped at the end of this function. It carries
/// `web_integration.runner_token`, `qontinui_user_id` and the sync toggles, and
/// none of those crosses into the report: layer 1 exports only the provenance
/// verdict and the bounded error, layer 5 exports two `web_integration` fields
/// through `api_config::api_base_url_inputs_from`, and layer 11 exports the
/// resolver's `(dir, arm)` pair. `config_report_live_render_carries_no_settings_field_value`
/// is the assertion that catches a later edit that widens this.
pub(crate) fn settings_derived_inputs() -> SettingsDerivedInputs {
    let crate::settings::LoadedSettings {
        mut settings,
        provenance,
        error,
    } = crate::settings::read_settings_from_disk();

    crate::settings::apply_web_integration_env_overlay(&mut settings);
    let api_base_url = crate::api_config::api_base_url_inputs_from(&settings);
    let workspace_root =
        crate::workspace_paths::workspace_root_from(settings.paths.workspace_root.as_deref());

    if let Some(roster) = crate::claude_accounts::load() {
        crate::claude_accounts::apply_roster_overlay(&mut settings, roster);
    }
    // Note what is NOT re-implemented: the selection rule itself still comes
    // from `get_effective_config_dir`, which returns the `(value, source)` pair
    // in one traversal.
    let claude_config_dir = crate::ai_provider::get_effective_config_dir(&settings.ai.claude_cli);
    drop(settings);

    SettingsDerivedInputs {
        provenance,
        error,
        api_base_url,
        claude_config_dir,
        workspace_root,
    }
}

/// Build the injection payload from live runner-binary state. Every bin-only
/// layer this binary can resolve goes in here; anything it cannot resolve stays
/// `None` and the lib driver reports it as `Unknown` with a reason, never as a
/// silently-dropped row.
pub(crate) fn config_report_inputs() -> ConfigReportInputs {
    let now = Utc::now();

    // Layers 1, 5 and 11, from ONE non-mutating read of settings.json. Nothing
    // below may call a settings door of its own — see the function's docs for
    // the write this collapse removed.
    let SettingsDerivedInputs {
        provenance,
        error,
        api_base_url,
        claude_config_dir,
        workspace_root,
    } = settings_derived_inputs();

    // Captured ONCE and shared by the three env layers and the section, so the
    // rows and the tables can never describe different captures of a
    // fast-moving thing.
    let env = env_generations_section(claude_config_dir.0.as_deref(), now);

    // Layer 12 — resolved through the writer's OWN path helper, and STATTED
    // rather than materialized: see `claude_settings_carrier_reading` on why a
    // diagnostic must not write the file it is describing.
    let hook_reg = crate::session::claude_hook::StopHookRegistration::from_env();
    let hook_path = crate::session::claude_hook::settings_path(
        &crate::session::claude_hook::session_restore_dir(),
        hook_reg,
    );
    let hook_exists = hook_path.is_file();
    let hook_env = std::env::var(crate::session::claude_hook::CLAUDE_SETTINGS_ENV).ok();

    // Layer 13 — the shim's INPUT, which is all this process can honestly hold
    // about a value assembled in another binary.
    let mcp_cfg_env = std::env::var(crate::coord_mcp::MCP_CONFIG_ENV).ok();
    let mcp_cfg_is_file = mcp_cfg_env
        .as_deref()
        .map(|p| std::path::Path::new(p).is_file())
        .unwrap_or(false);

    ConfigReportInputs {
        observer: Observer::RunnerApp,
        settings_struct: Some(settings_struct_reading(
            provenance,
            error.as_deref(),
            crate::settings::resolve_settings_path(),
            now,
        )),
        config_dir: Some(config_dir_reading(
            crate::settings::resolve_config_dir(),
            now,
        )),
        api_endpoint_registry: Some(api_base_url_reading(&api_base_url, now)),
        claude_config_dir: Some(claude_config_dir_reading(claude_config_dir, now)),
        launch_env_snapshot: Some(launch_env_snapshot_reading(&env, now)),
        adhoc_env_reads: Some(adhoc_env_reads_reading(&env, now)),
        supervisor_injected_env: Some(supervisor_injected_reading(&env, now)),
        coord_prompt_documents: Some(coord_prompt_documents_reading(
            crate::prompt_library::cache_health(),
            now,
        )),
        fleet_policy_dial: Some(fleet_policy_dial_reading(
            &crate::mcp::fleet_policy_poller::dial_snapshot(),
            now,
        )),
        claude_settings_carrier: Some(claude_settings_carrier_reading(
            hook_reg,
            &hook_path,
            hook_exists,
            hook_env.as_deref(),
            now,
        )),
        mcp_config_carrier: Some(mcp_config_carrier_reading(
            mcp_cfg_env.as_deref(),
            mcp_cfg_is_file,
            now,
        )),
        mcp_json: Some(mcp_json_reading(
            &crate::coord_mcp::mcp_json_report(workspace_root),
            now,
        )),
        env_generations: Some(env),
    }
}

/// Run the layered configuration report and return the structured result.
/// The frontend renders `ConfigReport`; the `render()`-formatted text is
/// available via [`config_report_text`].
#[tauri::command]
pub fn config_report_run() -> ConfigReport {
    build_report(&config_report_inputs())
}

/// Same report, as the copy-pasteable text form (identical in shape to the
/// standalone bin's stdout, differing only where the bin genuinely cannot
/// observe a layer). Convenient for a "copy report" button.
#[tauri::command]
pub fn config_report_text() -> String {
    config_report_run().render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_runner_lib::config_report::LayerSpec;

    fn fixed_stamp() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-22T12:34:56.789Z")
            .expect("literal is valid RFC 3339")
            .with_timezone(&Utc)
    }

    fn inputs(
        env_web: Option<&str>,
        env_api: Option<&str>,
        persisted: Option<&str>,
        is_debug: bool,
    ) -> ApiBaseUrlInputs {
        ApiBaseUrlInputs {
            env_web: env_web.map(str::to_string),
            env_api: env_api.map(str::to_string),
            persisted: persisted.map(str::to_string),
            is_debug,
        }
    }

    /// Each of the four documented rungs produces the arm string AND the value
    /// that rung's input implies — both asserted as LITERALS, so neither the
    /// precedence order nor the arm vocabulary can drift silently.
    #[test]
    fn config_report_api_arm_agrees_with_the_resolved_value() {
        let cases: Vec<(ApiBaseUrlInputs, &str, &str)> = vec![
            (
                inputs(
                    Some("https://web.example/"),
                    Some("https://api.example"),
                    Some("https://persisted.example"),
                    true,
                ),
                "env:QONTINUI_WEB_BACKEND_URL",
                "https://web.example",
            ),
            (
                inputs(
                    None,
                    Some("https://api.example/"),
                    Some("https://persisted.example"),
                    true,
                ),
                "env:QONTINUI_API_URL",
                "https://api.example",
            ),
            (
                inputs(Some("   "), None, Some("https://persisted.example/"), true),
                "persisted:web_integration.backend_url",
                "https://persisted.example",
            ),
            (
                inputs(None, None, None, true),
                "build_default:debug",
                "http://127.0.0.1:8000",
            ),
            (
                inputs(None, None, Some(""), false),
                "build_default:release",
                "https://api.qontinui.io",
            ),
        ];

        for (i, (input, expected_arm, expected_value)) in cases.into_iter().enumerate() {
            let reading = api_base_url_reading(&input, fixed_stamp());
            assert_eq!(
                reading,
                LayerReading::known(expected_value, expected_arm, fixed_stamp()),
                "case {i}: {input:?}"
            );
        }
    }

    /// **F-second-pass-1 regression.** The report must never reach
    /// `settings::load_settings_full` — the runner's one settings
    /// writer-by-side-effect.
    ///
    /// Layer 1 was moved onto the non-mutating `read_settings_from_disk`, and
    /// then layer 5 undid it ONE LINE BELOW by calling
    /// `api_config::gather_api_base_url_inputs()`, whose first statement is
    /// `load_settings()` → `load_settings_full()`. On a machine with a present,
    /// parsing `settings.json` whose `local_user_id` is empty, opening the config
    /// report minted a UUID and rewrote the operator's real file — while layer
    /// 1's `source` still said `read_settings_from_disk`, so the report concealed
    /// the write.
    ///
    /// # Why this is a call counter and not a byte fingerprint
    ///
    /// `config_report_live_command_writes_nothing_it_reports_on` watches the
    /// right files but CANNOT FAIL on a dev box: once boot has run the one-shot
    /// migration, `needs_persist` is false and `MIGRATE_ONCE` has fired, so the
    /// writer writes nothing and the fingerprints match whether or not the report
    /// called it. Driving the write path instead needs a process-global
    /// `set_var("QONTINUI_CONFIG_DIR")`, which races every sibling test that
    /// reads real settings — the documented cause of an existing flake. A
    /// per-thread entry counter has neither problem and asserts the stronger
    /// thing: not "the write did not happen this time", but "the code that can
    /// write was never entered".
    ///
    /// Three assertions, and the middle one is the non-vacuity control.
    #[test]
    fn config_report_never_reaches_the_settings_writer() {
        use crate::settings::settings_full_load_count;

        // (1) CONTROL — the instrument fires. Without this the whole test could
        // pass against a counter that never moves.
        let before = settings_full_load_count();
        let _ = crate::settings::load_settings_full();
        assert_eq!(
            settings_full_load_count(),
            before + 1,
            "the entry counter must move for a direct call, or this test proves nothing"
        );

        // (2) CONTROL — the two doors the report USED TO take are genuinely that
        // writer. These are the defects themselves, asserted rather than
        // narrated: if either stops reaching `load_settings_full`, the finding's
        // premise changed and this test must be re-derived.
        //
        // `gather_api_base_url_inputs` was layer 5's door. The second is the
        // subtler one and was found by this very counter: layer 14's
        // `coord_mcp::mcp_json_report` resolved the umbrella root through
        // `workspace_paths::workspace_root()`, which reads `paths.workspace_root`
        // via `config_facade::get_setting` → `load_settings_full` — TWICE per
        // report, once for the path and once for the write guard. Nothing in the
        // name "resolve the workspace root" says it is a settings write.
        let before = settings_full_load_count();
        let _ = crate::api_config::gather_api_base_url_inputs();
        assert_eq!(
            settings_full_load_count(),
            before + 1,
            "gather_api_base_url_inputs() is the writer door — that is why the report may not \
             call it"
        );
        let before = settings_full_load_count();
        let _ = crate::workspace_paths::workspace_root();
        assert_eq!(
            settings_full_load_count(),
            before + 1,
            "workspace_paths::workspace_root() is a writer door too — that is why layer 14 takes \
             an injected root"
        );

        // …and the read-only twins of both are NOT.
        let before = settings_full_load_count();
        let disk = crate::settings::read_settings_from_disk().settings;
        let _ = crate::api_config::api_base_url_inputs_from(&disk);
        let root =
            crate::workspace_paths::workspace_root_from(disk.paths.workspace_root.as_deref());
        let _ = crate::coord_mcp::mcp_json_report(root);
        assert_eq!(
            settings_full_load_count(),
            before,
            "the read-only twins must reach no writer at all"
        );

        // (3) THE ASSERTION. Neither the settings-derived half nor the whole
        // injection payload may enter it, not once.
        let before = settings_full_load_count();
        let derived = settings_derived_inputs();
        assert_eq!(
            settings_full_load_count(),
            before,
            "settings_derived_inputs() entered the settings writer"
        );

        let before = settings_full_load_count();
        let inputs = config_report_inputs();
        assert_eq!(
            settings_full_load_count(),
            before,
            "config_report_inputs() entered the settings writer — a diagnostic that mints a \
             local_user_id into the operator's settings.json has changed the answer by asking \
             the question"
        );

        // …and the layers it was supposed to resolve DID resolve, so this is not
        // a vacuous pass over an input builder that bailed out early.
        assert!(
            inputs.api_endpoint_registry.is_some(),
            "layer 5 must still be injected"
        );
        assert!(
            inputs.settings_struct.is_some(),
            "layer 1 must still be injected"
        );
        assert!(
            inputs.claude_config_dir.is_some(),
            "layer 11 must still be injected"
        );
        // Layer 5's inputs come from the read-only twin over the SAME document
        // layer 1 reported on — one read, three layers.
        assert_eq!(derived.api_base_url.is_debug, cfg!(debug_assertions));

        println!(
            "[config-report evidence] load_settings_full entries on this thread after the full \
             input build: {}",
            settings_full_load_count()
        );
    }

    /// Every rung's `ApiBaseUrlArm::value_origin_name` is a connection-string
    /// NAME, asserted as LITERALS.
    ///
    /// This is what makes `classify_env_var`'s JOINT arm — a `*_URL`/`*_URI`/
    /// `*_DSN` name whose value carries userinfo at all — reachable for layer 5.
    /// Without it, `https://ops@qontinui.internal` (an account name, no password)
    /// would print verbatim: the value arm only fires on
    /// `UrlUserinfo::WithPassword`.
    #[test]
    fn config_report_api_arm_origin_names_are_url_named() {
        use crate::api_config::ApiBaseUrlArm;

        for (arm, expected) in [
            (ApiBaseUrlArm::EnvWebBackendUrl, "QONTINUI_WEB_BACKEND_URL"),
            (ApiBaseUrlArm::EnvApiUrl, "QONTINUI_API_URL"),
            (
                ApiBaseUrlArm::PersistedBackendUrl,
                "web_integration.backend_url",
            ),
            (
                ApiBaseUrlArm::BuildDefaultDebug,
                "build_default.backend_url",
            ),
            (
                ApiBaseUrlArm::BuildDefaultRelease,
                "build_default.backend_url",
            ),
            (
                ApiBaseUrlArm::BuildDefaultReleaseLoopbackRejected,
                "build_default.backend_url",
            ),
        ] {
            assert_eq!(arm.value_origin_name(), expected);
            assert!(
                expected.to_ascii_uppercase().ends_with("_URL"),
                "{expected} must be judged under the connection-string name rule"
            );
        }
    }

    /// **F-second-pass-2 regression.** A credential-bearing URL reaching layer 5
    /// is WITHHELD, not printed.
    ///
    /// The classifier was wired into `EnvGeneration::capture` only, so in ONE
    /// rendered report the G1/G3 rows for `QONTINUI_WEB_BACKEND_URL` showed
    /// `<withheld #…: value is a URL with a password in its userinfo>` while this
    /// row printed the password in full. Every rung is driven, including the
    /// PERSISTED one, which reached the row without passing any env classifier at
    /// all.
    ///
    /// The arm is asserted to survive in full: withholding the value must not
    /// cost the reader the "which rung produced this?" half.
    #[test]
    fn config_report_api_layer_withholds_a_credential_bearing_url() {
        // (a) A password in the userinfo, on each of the three configurable
        // rungs.
        let pw = "S3cretPw";
        let url = "https://ops:S3cretPw@qontinui.internal";
        assert!(url.contains(pw), "the fixture must carry the password");
        for (i, (input, expected_arm)) in [
            (
                inputs(Some(url), None, None, true),
                "env:QONTINUI_WEB_BACKEND_URL",
            ),
            (inputs(None, Some(url), None, true), "env:QONTINUI_API_URL"),
            (
                inputs(None, None, Some(url), true),
                "persisted:web_integration.backend_url",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let reading = api_base_url_reading(&input, fixed_stamp());
            let LayerReading::Known { value, source, .. } = &reading else {
                panic!("case {i}: a resolved URL is always Known, got {reading:?}");
            };
            assert!(
                !value.contains(pw),
                "case {i}: the password reached the layer row: {value}"
            );
            assert!(
                !value.contains("qontinui.internal"),
                "case {i}: a withheld value must carry NO part of the URL: {value}"
            );
            assert!(
                value.starts_with("<withheld #")
                    && value.contains("value is a URL with a password in its userinfo"),
                "case {i}: the row must say WHY it withheld: {value}"
            );
            assert_eq!(
                source, expected_arm,
                "case {i}: the ARM is the half that stays useful when the value is withheld"
            );
        }

        // (b) The joint NAME arm: an account with NO password. The value arm
        // cannot catch this one — `value_is_credential` fires only on
        // `WithPassword` — so it is the control for `value_origin_name` being a
        // `*_URL` name.
        let reading = api_base_url_reading(
            &inputs(
                Some("https://serviceacct@qontinui.internal"),
                None,
                None,
                true,
            ),
            fixed_stamp(),
        );
        let LayerReading::Known { value, .. } = &reading else {
            panic!("got {reading:?}");
        };
        assert!(
            !value.contains("serviceacct"),
            "an account name next to a host is withheld under the joint arm: {value}"
        );
        assert!(
            value.contains("the value carries URL userinfo"),
            "…and the row says which arm did it: {value}"
        );

        // (c) THE OTHER DIRECTION. A userinfo-free backend URL is the single
        // most-read line of this report and must stay printable, or the fix
        // would have broken the diagnostic instead of the leak. LITERALS.
        for (input, expected_value) in [
            (
                inputs(Some("https://api.qontinui.io"), None, None, true),
                "https://api.qontinui.io",
            ),
            (inputs(None, None, None, true), "http://127.0.0.1:8000"),
            (
                inputs(None, None, Some("http://192.168.1.10:8000/"), true),
                "http://192.168.1.10:8000",
            ),
        ] {
            let reading = api_base_url_reading(&input, fixed_stamp());
            let LayerReading::Known { value, .. } = &reading else {
                panic!("got {reading:?}");
            };
            assert_eq!(value, expected_value);
        }
    }

    /// THE FALSIFICATION CHECK, bin side: a reading resolved from a BIN-only
    /// module reaches the LIB driver and renders as a real value with its arm.
    /// Nothing here restructures the crate boundary — the whole crossing is one
    /// `Option<LayerReading>` field.
    #[test]
    fn config_report_bin_injection_reaches_the_lib_driver() {
        let report = build_report(&ConfigReportInputs {
            observer: Observer::RunnerApp,
            settings_struct: None,
            config_dir: None,
            api_endpoint_registry: Some(api_base_url_reading(
                &inputs(Some("https://web.example"), None, None, true),
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
        });

        let row = report
            .row("api_endpoint_registry")
            .expect("the injected layer has a row");
        assert_eq!(
            row.reading,
            LayerReading::known(
                "https://web.example",
                "env:QONTINUI_WEB_BACKEND_URL",
                fixed_stamp()
            )
        );

        let text = report.render();
        assert!(
            text.contains("      value:       https://web.example\n"),
            "injected value must reach the rendered report:\n{text}"
        );
        assert!(
            text.contains("      source:      env:QONTINUI_WEB_BACKEND_URL\n"),
            "injected arm must reach the rendered report:\n{text}"
        );
    }

    /// The same driver, asked from the runner app but WITHOUT the injection,
    /// says so — and blames the wiring, not the machine. The two `Unknown`
    /// reasons must not be interchangeable: one is a structural limit of the
    /// headless bin, the other is a bug in this module.
    #[test]
    fn config_report_uninjected_bin_layer_blames_the_wiring_not_the_machine() {
        let report = build_report(&ConfigReportInputs {
            observer: Observer::RunnerApp,
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
        });
        match &report
            .row("api_endpoint_registry")
            .expect("row present")
            .reading
        {
            LayerReading::Unknown { reason, .. } => {
                assert!(
                    reason.contains("was not injected by the runner binary"),
                    "reason must name the wiring bug: {reason}"
                );
                assert!(
                    !reason.contains("headless"),
                    "the runner app is not the headless bin: {reason}"
                );
            }
            other => panic!("an uninjected layer must be Unknown, got {other:?}"),
        }
    }

    /// Every arm of layer 11, including both ways of resolving to no account.
    /// The rendered `value` and the arm's wire string are both LITERALS here —
    /// asserting `source.as_str()` against the enum would pin nothing, and the
    /// `(none)` sentinel is the one piece of vocabulary this module invents, so
    /// it is the piece most in need of pinning.
    ///
    /// The two `None` rows are the point of the test: they are the same
    /// `Option::None` and they mean different things.
    #[test]
    fn config_report_claude_config_dir_reading_covers_every_arm() {
        let cases: Vec<((Option<String>, ClaudeConfigDirSource), &str, &str)> = vec![
            (
                (
                    Some("C:/claude/acct-a".to_string()),
                    ClaudeConfigDirSource::LeastUsageResolved,
                ),
                "C:/claude/acct-a",
                "least_usage_resolved",
            ),
            (
                (
                    Some("C:/claude/acct-b".to_string()),
                    ClaudeConfigDirSource::LeastUsageConfigDirFallback,
                ),
                "C:/claude/acct-b",
                "least_usage_config_dir_fallback",
            ),
            (
                (
                    Some("C:/claude/acct-c".to_string()),
                    ClaudeConfigDirSource::Manual,
                ),
                "C:/claude/acct-c",
                "manual_config_dir",
            ),
            (
                (
                    Some("C:/claude/acct-d".to_string()),
                    ClaudeConfigDirSource::RequestOverride,
                ),
                "C:/claude/acct-d",
                "request_override",
            ),
            // An account IS configured and its credentials are dead …
            (
                (None, ClaudeConfigDirSource::RejectedNoCredentials),
                "(none)",
                "rejected_no_credentials",
            ),
            // … versus no account was ever selected. Same `None`, different fix.
            (
                (None, ClaudeConfigDirSource::Unconfigured),
                "(none)",
                "unconfigured",
            ),
        ];

        for (i, (resolved, expected_value, expected_arm)) in cases.into_iter().enumerate() {
            assert_eq!(
                claude_config_dir_reading(resolved, fixed_stamp()),
                LayerReading::known(expected_value, expected_arm, fixed_stamp()),
                "case {i}"
            );
        }
    }

    /// The live command path resolves against this machine's real env +
    /// settings and produces a KNOWN reading for every bin-only layer this
    /// phase wired. Guards the wiring that the pure tests above deliberately
    /// bypass — a field left `None` in `config_report_inputs` compiles fine and
    /// only shows up here.
    #[test]
    fn config_report_live_command_injects_every_bin_layer() {
        let report = config_report_run();
        let specs: Vec<&LayerSpec> = report.rows.iter().map(|r| r.spec).collect();
        assert_eq!(specs.len(), 15, "every layer gets a row");

        match &report
            .row("api_endpoint_registry")
            .expect("row present")
            .reading
        {
            LayerReading::Known { value, source, .. } => {
                assert!(!value.is_empty(), "a known backend URL must have a value");
                assert!(
                    source.starts_with("env:")
                        || source.starts_with("persisted:")
                        || source.starts_with("build_default:"),
                    "unexpected api base arm: {source}"
                );
            }
            other => panic!("the runner app can always resolve this layer, got {other:?}"),
        }

        // Layer 2 — the config dir. `Unknown` is legitimate only when the
        // resolver itself failed, and it must then quote that failure rather
        // than blaming the injection.
        match &report.row("config_dir").expect("row present").reading {
            LayerReading::Known { value, source, .. } => {
                assert!(!value.is_empty(), "a known config dir must have a value");
                assert!(
                    ["env:QONTINUI_CONFIG_DIR", "platform_config_dir"].contains(&source.as_str()),
                    "unexpected config dir arm: {source}"
                );
            }
            LayerReading::Unknown { reason, .. } => {
                assert!(
                    reason.contains("settings::resolve_config_dir() failed"),
                    "an unresolvable config dir must quote the resolver: {reason}"
                );
            }
            other => panic!("layer 2 is never withheld, got {other:?}"),
        }

        // Layer 11 — always KNOWN from the runner app: "no account" is a
        // reading, not an unreadable layer.
        match &report
            .row("claude_config_dir")
            .expect("row present")
            .reading
        {
            LayerReading::Known { value, source, .. } => {
                assert!(!value.is_empty(), "even the no-account row has a value");
                assert!(
                    [
                        "request_override",
                        "least_usage_resolved",
                        "least_usage_config_dir_fallback",
                        "manual_config_dir",
                        "rejected_no_credentials",
                        "unconfigured",
                    ]
                    .contains(&source.as_str()),
                    "unexpected claude config dir arm: {source}"
                );
            }
            other => panic!("the runner app always resolves layer 11, got {other:?}"),
        }

        // Phase 3 — the three env layers are KNOWN from the runner app, and
        // each names the generation it read rather than a bare value.
        match &report
            .row("launch_env_snapshot")
            .expect("row present")
            .reading
        {
            LayerReading::Known { source, .. } => assert_eq!(
                source, "launch_env::RunnerLaunchEnv::read (first call — `main`)",
                "layer 6 must attribute the LAUNCH read"
            ),
            // Legitimate only in a process where `read()` never ran.
            LayerReading::Unknown { reason, .. } => assert!(
                reason.contains("has not run in this process"),
                "layer 6's only honest Unknown is a missing first read: {reason}"
            ),
            other => panic!("layer 6 is never withheld, got {other:?}"),
        }
        match &report.row("adhoc_env_reads").expect("row present").reading {
            LayerReading::Known { value, source, .. } => {
                assert_eq!(source, "std::env::vars (runner process — generation G1)");
                assert!(value.contains("variables"), "got {value}");
            }
            other => panic!("the runner app always resolves layer 7, got {other:?}"),
        }
        match &report
            .row("supervisor_injected_env")
            .expect("row present")
            .reading
        {
            LayerReading::Known { value, source, .. } => {
                assert!(
                    source.starts_with("observed in this process's env"),
                    "layer 8 must say it OBSERVED rather than asked the supervisor: {source}"
                );
                assert!(value.contains(" of 7"), "got {value}");
            }
            other => panic!("the runner app always resolves layer 8, got {other:?}"),
        }
    }

    // =======================================================================
    // Phase 3 — env generations.
    // =======================================================================

    /// **THE REDACTION NEGATIVE CONTROL.**
    ///
    /// This report renders a pipe table and aligned columns. `SECRET_RE` in
    /// `session/redact.rs` fires only on `key[=:]value` ADJACENCY, so it
    /// provably does **not** see a secret sitting in a table cell — and a
    /// redaction test fed only `key: value` fixtures would pass vacuously
    /// against exactly the shape this report emits.
    ///
    /// So the test is deliberately two-sided:
    ///
    /// 1. the regex is shown to MISS the table shape (that failure is the
    ///    control — if it ever starts matching, the premise changed and this
    ///    test must be re-derived, not deleted);
    /// 2. the key-level classifier catches the same secret anyway, because the
    ///    value was withheld at ingestion and never reached a cell.
    ///
    /// That is the whole argument for withholding at the model layer:
    /// `redact.rs` says of itself it is "defense in depth, NOT a security
    /// boundary … a courtesy backstop", and a deliberate credential-adjacent
    /// dump cannot be gated on a courtesy backstop that structurally cannot see
    /// its output format.
    #[test]
    fn config_report_env_table_defeats_redact_secrets_but_not_the_classifier() {
        use crate::session::redact::redact_secrets;
        use qontinui_runner_lib::env_generations::{
            EnvFingerprinter, EnvGeneration, EnvGenerationSpec, EnvGenerations,
        };

        let secret = "hunter2";

        // (1) The control. A pipe-table row and an aligned-column row, both
        //     carrying the secret in exactly the shape `render_table` emits,
        //     survive the sweep completely untouched.
        let table_row = format!("  POSTGRES_PASSWORD    | {secret}   | (absent)");
        let swept = String::from_utf8(redact_secrets(table_row.as_bytes())).expect("utf8");
        assert_eq!(
            swept, table_row,
            "premise broken: SECRET_RE now catches a pipe-table cell — re-derive this control"
        );
        assert!(
            swept.contains(secret),
            "the control must still hold {secret}"
        );

        let aligned = format!("  POSTGRES_PASSWORD      {secret}");
        let swept_aligned = String::from_utf8(redact_secrets(aligned.as_bytes())).expect("utf8");
        assert_eq!(
            swept_aligned, aligned,
            "premise broken: SECRET_RE now catches an aligned column"
        );

        // …and the sweep DOES catch the adjacency form, so the control above is
        // about the SHAPE and not about a broken regex.
        let adjacent = format!("POSTGRES_PASSWORD={secret}");
        let swept_adjacent = String::from_utf8(redact_secrets(adjacent.as_bytes())).expect("utf8");
        assert!(
            !swept_adjacent.contains(secret),
            "the regex must still catch key=value: {swept_adjacent}"
        );

        // (2) The real control: withheld at ingestion, so the renderer never
        //     had the value to put in a cell.
        let fp = EnvFingerprinter::new();
        let section = EnvGenerations {
            generations: vec![EnvGeneration::capture(
                &fp,
                EnvGenerationSpec {
                    id: "G1",
                    name: "runner_process",
                    describes: "this process's env",
                    freshness: "frozen at start",
                    is_full_env: true,
                },
                fixed_stamp(),
                [
                    ("QONTINUI_CONFIG_DIR", "C:/cfg"),
                    ("QONTINUI_POSTGRES_PASSWORD", secret),
                ],
            )],
            divergences: vec![],
            launch_drift: None,
            seams: vec![],
        }
        .render();

        assert!(
            section.contains("QONTINUI_POSTGRES_PASSWORD | <withheld #"),
            "the rendered section must use the pipe-table shape the regex cannot see:\n{section}"
        );
        assert!(
            !section.contains(secret),
            "the secret reached the rendered table:\n{section}"
        );
        assert!(section.contains("<withheld #"), "{section}");

        // (3) The belt-and-braces second pass over the rendered text is a
        //     no-op, because withholding already left nothing to find. If this
        //     report ever depended on that pass, (1) proves it would leak.
        let braced = String::from_utf8(redact_secrets(section.as_bytes())).expect("utf8");
        assert_eq!(braced, section);
    }

    /// The live capture on THIS machine: three generations, a divergence
    /// between the two full ones, and all eight spawn seams.
    ///
    /// Run with `--nocapture` to read the section — it is the report an
    /// operator sees from the in-app command.
    #[test]
    fn config_report_live_env_generations_cover_three_ages_and_eight_seams() {
        let section = env_generations_section(None, fixed_stamp());

        let names: Vec<&str> = section
            .generations
            .iter()
            .map(|g| g.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["runner_process", "launch_snapshot", "pty_child"]
        );
        assert!(
            !section.generations[0].vars.is_empty(),
            "this process has an environment"
        );
        assert!(
            !section.generations[2].vars.is_empty(),
            "a PTY child inherits an environment"
        );
        assert_eq!(section.divergences.len(), 1, "G1 → G3 is the diagnostic");

        let seams: Vec<&str> = section.seams.iter().map(|s| s.seam.as_str()).collect();
        assert_eq!(
            seams,
            vec![
                "session::TerminalSession::finalize_child_env",
                "agent_runtime::finalize_headless_child_env",
                "claude_session::session::finalize_child_env",
                "claude_session::runner::build_inline_child_command",
                "ai_provider::process::prepare_ai_child_env",
                "ai_provider::claude_cli::build_scorer_command",
                "orchestration_loop::fix_agent::build_fix_agent_command",
                "commands::command_interpreter::build_interpret_command",
            ],
            "all eight seams from the `crate::terminal` table must be reported"
        );

        // Every seam clears every credential name — observed by CALLING the
        // seam, not by restating its source.
        for seam in &section.seams {
            for name in crate::terminal::CREDENTIAL_VALUE_ENV_VARS {
                assert!(
                    seam.clears.iter().any(|c| c == name),
                    "{}: {name} is not cleared — the credential scrub is missing from this \
                     seam's env construction",
                    seam.seam
                );
            }
        }

        println!(
            "\n[config-report evidence] withheld readings in this section: {}\n{}",
            section.total_withheld(),
            section.render()
        );
    }

    /// Shannon entropy of `value`, in bits per character.
    ///
    /// A discriminator the production classifier does not have: it measures
    /// SYMBOL VARIETY, where `value_is_credential`'s entropy arm measures
    /// charset membership plus the presence of a lower-case, an upper-case and
    /// a digit. A 40-character all-lower-case random token scores ~5 bits here
    /// and is invisible to the production arm — which is exactly the kind of
    /// independence [`looks_credential_bearing_independently`] needs to be
    /// worth anything.
    fn shannon_bits_per_char(value: &str) -> f64 {
        let mut counts: std::collections::BTreeMap<char, usize> = std::collections::BTreeMap::new();
        for c in value.chars() {
            *counts.entry(c).or_default() += 1;
        }
        let n = value.chars().count() as f64;
        if n == 0.0 {
            return 0.0;
        }
        -counts
            .values()
            .map(|&k| {
                let p = k as f64 / n;
                p * p.log2()
            })
            .sum::<f64>()
    }

    /// **The independent credential heuristic** — deliberately NOT
    /// `classify_env_var`, and it must never call it.
    ///
    /// This is F2's whole point. The leak tests below used to read
    ///
    /// ```ignore
    /// if value.len() < 8 || classify_env_var(&name, &value).is_none() { continue; }
    /// ```
    ///
    /// which scoped the assertion to the values the classifier already catches
    /// — so every value it MISSED was skipped before the assertion ran. That
    /// proves only what `EnvValue::Withheld` guarantees structurally (a withheld
    /// reading has no field able to hold a value), and it could never have
    /// caught F1: a `postgresql://user:pw@host/db` the classifier did not
    /// recognise was excluded from its own leak check. It is the same vacuity
    /// class this module's header criticises `redact_secrets` for, one level up.
    ///
    /// Three OR-ed arms, each spelled differently from the production one:
    ///
    /// 1. **URL userinfo**, found by splitting rather than by the production
    ///    parser — an `@` inside the authority of a `scheme://…` value.
    /// 2. **Shannon entropy** above 4.0 bits/char on a separator-free token.
    ///    The bar is above the 4.0 ceiling of hex on purpose: a 40-character
    ///    git SHA is one of this report's most useful lines and the report
    ///    prints it deliberately, so a heuristic that flagged it would be
    ///    asserting the opposite of the intended behaviour.
    ///
    ///    **That reasoning holds only where LENGTH is not the binding
    ///    constraint, and this arm's 16-character floor means it often is.**
    ///    Shannon entropy over *n* symbols is capped at log₂(*n*) — at exactly
    ///    16 characters the ceiling IS 4.0, so the strict `> 4.0` test can never
    ///    fire there no matter how random the value; a 64-symbol base64 alphabet
    ///    with no repeats needs 17 characters to exceed it and, with the repeats
    ///    a real token has, closer to 28. So this arm is a check on *long*
    ///    tokens, and short-but-secret values are covered by arms 1 and 3 rather
    ///    than by this one. The floor is deliberate all the same: below it the
    ///    per-character estimate is noise, and the arms that matter for the
    ///    values this report actually carries are the URL one and the name list.
    /// 3. **An independently written name list** — same idea as
    ///    `CREDENTIAL_NAME_TOKENS`, retyped, so deleting an entry from that
    ///    constant does not silently narrow this check too.
    ///
    /// A value this returns `true` for and the render still contains is a
    /// FAILURE, whatever the classifier thinks of it.
    fn looks_credential_bearing_independently(name: &str, value: &str) -> bool {
        if value.chars().count() < 16 {
            return false;
        }

        // (1) `scheme://…@…` where the `@` is in the authority.
        let url_userinfo = value
            .split_once("://")
            .map(|(_, rest)| {
                rest.split(['/', '?', '#'])
                    .next()
                    .unwrap_or("")
                    .contains('@')
            })
            .unwrap_or(false);

        // (2) High symbol variety in a separator-free token.
        let tokenish = !value.chars().any(|c| {
            c.is_whitespace() || matches!(c, '/' | '\\' | ';' | ':' | ',' | '"' | '@' | '%')
        });
        let high_entropy = tokenish && shannon_bits_per_char(value) > 4.0;

        // (3) A retyped name list.
        let upper = name.to_ascii_uppercase();
        let name_hit = [
            "PASSWORD",
            "PASSWD",
            "PASSPHRASE",
            "SECRET",
            "APIKEY",
            "API_KEY",
            "ACCESS_KEY",
            "PRIVATE_KEY",
            "CLIENT_SECRET",
            "REFRESH_TOKEN",
            "ACCESS_TOKEN",
            "BEARER",
            "CREDENTIAL",
        ]
        .iter()
        .any(|t| upper.contains(t));

        url_userinfo || high_entropy || name_hit
    }

    /// The heuristic above is meaningful: it fires on the shapes that matter
    /// (including the exact value F1 was about) and stays silent on the values
    /// this report exists to print.
    ///
    /// Without this, a heuristic that returned `false` for everything would
    /// make both leak tests below pass vacuously — the failure mode the tests
    /// they replace actually had.
    #[test]
    fn config_report_independent_heuristic_fires_on_secrets_and_not_on_diagnostics() {
        for (name, value) in [
            // The F1 value, verbatim.
            (
                "QONTINUI_DATABASE_URL",
                "postgresql://qontinui:hunter2@localhost:5432/qontinui",
            ),
            ("HTTPS_PROXY", "http://user:pass@proxy:8080"),
            // All-lower-case high-variety token: caught here, MISSED by the
            // production entropy arm, which requires an upper-case character.
            ("BLOB", "q7zx4m9k2wvb8ndp1jrl5tyf3hgc6soaeui0"),
            ("SOME_PASSWORD", "correct horse battery staple"),
        ] {
            assert!(
                looks_credential_bearing_independently(name, value),
                "{name} must be flagged by the independent heuristic"
            );
        }

        for (name, value) in [
            (
                "PATH",
                "C:/bin;C:/Windows/system32;C:/Program Files/Git/cmd",
            ),
            ("QONTINUI_API_URL", "http://127.0.0.1:8000"),
            ("QONTINUI_WEB_BACKEND_URL", "https://api.qontinui.io"),
            // The documented carve-out: a git SHA is printed on purpose.
            (
                "QONTINUI_GIT_SHA",
                "7bb1ed7b0c9a1f2e3d4c5b6a7988990011223344",
            ),
            ("QONTINUI_CONFIG_DIR", "C:/Users/x/AppData/Roaming/qontinui"),
            ("TERM", "xterm-256color"),
        ] {
            assert!(
                !looks_credential_bearing_independently(name, value),
                "{name} is a value the report must be able to print"
            );
        }
    }

    /// **The end-to-end leak check, against this machine's REAL secrets.**
    ///
    /// For every variable in this process's environment that an INDEPENDENT
    /// heuristic — [`looks_credential_bearing_independently`], which never
    /// consults `classify_env_var` — calls credential-bearing, assert its
    /// actual value appears nowhere in the rendered report. Nothing is planted:
    /// these are the values the runner is genuinely carrying (CLAUDE.md
    /// documents three plaintext passwords among them), which is the only
    /// fixture that can prove the classifier's REACH on a real machine rather
    /// than against a value chosen to be catchable.
    ///
    /// Scoping this loop with the classifier itself — as it did before — made
    /// it structurally unable to fail for anything the classifier missed, which
    /// is the only failure worth testing for.
    #[test]
    fn config_report_live_render_contains_no_credential_value_from_this_process() {
        let rendered = env_generations_section(None, fixed_stamp()).render();

        let mut checked = 0usize;
        // `vars_os` + lossy, not `vars()`: the leak check must not itself
        // panic on the non-Unicode machine it is most needed on.
        for (name, value) in lossy_env_pairs(std::env::vars_os()) {
            if !looks_credential_bearing_independently(&name, &value) {
                continue;
            }
            checked += 1;
            assert!(
                !rendered.contains(&value),
                "the value of {name} reached the rendered report — it looks credential-bearing \
                 to a heuristic the classifier does not share, so the classifier must withhold it"
            );
        }

        // The NON-VACUITY control. Two planted connection strings, in the shape
        // F1 was about, must be flagged by the independent heuristic AND
        // withheld by the classifier — so this assertion is one that can fail.
        //
        // The second is the sharper of the two: its NAME carries no credential
        // token and no `_URL`/`_URI`/`_DSN` suffix, so the ONLY thing that can
        // withhold it is the value-SHAPE arm. Delete that arm and this test
        // fails; the `QONTINUI_DATABASE_URL` case alone would still be caught
        // by the name+userinfo arm and would never notice.
        let planted = "postgresql://qontinui:hunter2@localhost:5432/qontinui";
        let planted_proxy = "http://proxyuser:s3cr3tpw@proxy.internal:8080";
        assert!(looks_credential_bearing_independently(
            "QONTINUI_DATABASE_URL",
            planted
        ));
        assert!(looks_credential_bearing_independently(
            "QONTINUI_PROXY_PROBE",
            planted_proxy
        ));
        let fp = qontinui_runner_lib::env_generations::EnvFingerprinter::new();
        let seeded = qontinui_runner_lib::env_generations::EnvGenerations {
            generations: vec![
                qontinui_runner_lib::env_generations::EnvGeneration::capture(
                    &fp,
                    qontinui_runner_lib::env_generations::EnvGenerationSpec {
                        id: "G1",
                        name: "runner_process",
                        describes: "seeded",
                        freshness: "seeded",
                        is_full_env: true,
                    },
                    fixed_stamp(),
                    [
                        ("QONTINUI_DATABASE_URL", planted),
                        ("QONTINUI_PROXY_PROBE", planted_proxy),
                    ],
                ),
            ],
            divergences: vec![],
            launch_drift: None,
            seams: vec![],
        }
        .render();
        assert!(
            !seeded.contains("hunter2"),
            "a planted connection string reached the render:\n{seeded}"
        );
        assert!(
            !seeded.contains("s3cr3tpw"),
            "a planted proxy password reached the render — the VALUE-shape arm is the only \
             thing that can catch this one:\n{seeded}"
        );
        // Both must appear as WITHHELD cells in the pipe-table shape. The name
        // column is padded to one width, so match on the row rather than on a
        // fixed `name | ` string.
        for planted_name in ["QONTINUI_DATABASE_URL", "QONTINUI_PROXY_PROBE"] {
            assert!(
                seeded.lines().any(|l| {
                    l.trim_start().starts_with(planted_name) && l.contains("| <withheld #")
                }),
                "{planted_name} must be withheld in the table shape:\n{seeded}"
            );
        }

        println!(
            "[config-report evidence] env values flagged by the INDEPENDENT heuristic and \
             checked against the render: {checked}"
        );
    }

    /// Layer 8 reports an OBSERVATION, and both arms of it are honest: some of
    /// the supervisor's names present, or a stated zero. A zero must never
    /// render as an absent row or as UNKNOWN — "this runner is unsupervised" is
    /// a reading, not a failure to read.
    #[test]
    fn config_report_supervisor_layer_states_zero_as_a_reading() {
        use qontinui_runner_lib::env_generations::{
            EnvFingerprinter, EnvGeneration, EnvGenerationSpec, EnvGenerations,
        };

        let fp = EnvFingerprinter::new();
        let empty = EnvGenerations {
            generations: vec![EnvGeneration::capture(
                &fp,
                EnvGenerationSpec {
                    id: "G1",
                    name: "runner_process",
                    describes: "d",
                    freshness: "f",
                    is_full_env: true,
                },
                fixed_stamp(),
                [("PATH", "C:/bin")],
            )],
            divergences: vec![],
            launch_drift: None,
            seams: vec![],
        };
        match supervisor_injected_reading(&empty, fixed_stamp()) {
            LayerReading::Known { value, .. } => assert!(
                value.starts_with("0 of 7 supervisor-injected variables present"),
                "got {value}"
            ),
            other => panic!("a zero must be a KNOWN reading, got {other:?}"),
        }

        let supervised = EnvGenerations {
            generations: vec![EnvGeneration::capture(
                &fp,
                EnvGenerationSpec {
                    id: "G1",
                    name: "runner_process",
                    describes: "d",
                    freshness: "f",
                    is_full_env: true,
                },
                fixed_stamp(),
                [
                    ("QONTINUI_INSTANCE_NAME", "temp-abc"),
                    ("QONTINUI_PORT", "9901"),
                ],
            )],
            divergences: vec![],
            launch_drift: None,
            seams: vec![],
        };
        match supervisor_injected_reading(&supervised, fixed_stamp()) {
            LayerReading::Known { value, .. } => assert_eq!(
                value, "2 of 7 present: QONTINUI_INSTANCE_NAME, QONTINUI_PORT",
                "the row must name WHICH ones, in table order"
            ),
            other => panic!("got {other:?}"),
        }
    }

    /// Layer 6's `Unknown` arm is reachable only when no launch read happened,
    /// and it must refuse the "no drift" reading in words — an operator who
    /// reads a blank as agreement will stop looking.
    #[test]
    fn config_report_launch_layer_unknown_refuses_the_no_drift_reading() {
        use qontinui_runner_lib::env_generations::EnvGenerations;

        let none = EnvGenerations {
            generations: vec![],
            divergences: vec![],
            launch_drift: None,
            seams: vec![],
        };
        match launch_env_snapshot_reading(&none, fixed_stamp()) {
            LayerReading::Unknown { reason, .. } => {
                assert!(reason.contains("has not run in this process"), "{reason}");
                assert!(
                    reason.contains("not a finding that the snapshot is current"),
                    "the reason must refuse the wrong inference: {reason}"
                );
            }
            other => panic!("a missing launch read is UNKNOWN, got {other:?}"),
        }
    }

    // =======================================================================
    // Phase 4 — the two TIME-VARYING layers (9, 10).
    // =======================================================================

    /// Layer 9 reports the CACHE's own age, and an empty cache says so without
    /// claiming coord's library is empty.
    ///
    /// The strings are LITERALS. Asserting the rendered value against
    /// `health.age_ms` would pin nothing — the reading is built from that
    /// field, so such a test passes for any wording at all, including a wording
    /// that silently dropped the age.
    #[test]
    fn config_report_prompt_library_row_states_the_caches_own_age() {
        use crate::prompt_library::PromptLibraryCacheHealth;

        let empty = PromptLibraryCacheHealth {
            populated: false,
            age_ms: None,
            fresh: false,
            ttl_ms: 45_000,
            documents: 0,
            has_etag: false,
        };
        match coord_prompt_documents_reading(empty, fixed_stamp()) {
            LayerReading::Known { value, source, .. } => {
                assert!(
                    value.contains("0 documents — NOTHING has been fetched from coord"),
                    "got {value}"
                );
                assert!(
                    value.contains("not a finding that coord's library is empty"),
                    "an empty cache must refuse the wrong inference: {value}"
                );
                assert!(
                    source.contains("the CACHE's own last-refresh, not this report's read time"),
                    "the source must separate the two clocks: {source}"
                );
            }
            other => panic!("an empty cache is a READING, not an unreadable layer: {other:?}"),
        }

        let fresh = PromptLibraryCacheHealth {
            populated: true,
            age_ms: Some(1_200),
            fresh: true,
            ttl_ms: 45_000,
            documents: 7,
            has_etag: true,
        };
        match coord_prompt_documents_reading(fresh, fixed_stamp()) {
            LayerReading::Known { value, .. } => {
                assert!(
                    value.contains("7 document(s), last confirmed against coord 1200 ms ago"),
                    "got {value}"
                );
                assert!(
                    value.contains("fresh: the next read serves from cache"),
                    "got {value}"
                );
                assert!(value.contains("ETag stored"), "got {value}");
            }
            other => panic!("got {other:?}"),
        }

        let stale = PromptLibraryCacheHealth {
            populated: true,
            age_ms: Some(90_000),
            fresh: false,
            ttl_ms: 45_000,
            documents: 7,
            has_etag: false,
        };
        match coord_prompt_documents_reading(stale, fixed_stamp()) {
            LayerReading::Known { value, .. } => {
                assert!(value.contains("90000 ms ago"), "got {value}");
                assert!(
                    value.contains("STALE: the next read re-fetches"),
                    "got {value}"
                );
                assert!(value.contains("ETag absent"), "got {value}");
            }
            other => panic!("got {other:?}"),
        }
    }

    fn dial(
        mode: &str,
        capture: &str,
        floors: (Option<u64>, Option<u64>, Option<u64>, Option<u64>),
        briefings: Vec<crate::mcp::fleet_policy_poller::BriefingDial>,
    ) -> crate::mcp::fleet_policy_poller::FleetPolicyDial {
        crate::mcp::fleet_policy_poller::FleetPolicyDial {
            poll_interval_ms: 45_000,
            install_intercept_mode: mode.to_string(),
            install_intercept_default: "off",
            host_warn_free_bytes: floors.0,
            host_critical_free_bytes: floors.1,
            wsl_warn_free_bytes: floors.2,
            wsl_critical_free_bytes: floors.3,
            plan_capture_level: capture.to_string(),
            plan_capture_default: "off",
            plan_capture_record_level: "record",
            briefings,
            caches_expose_refresh_time: false,
        }
    }

    /// **The freshness commitment, layer 10.**
    ///
    /// Three caches carry a value and no stamp, so the row must say UNKNOWN for
    /// their last-refresh time — never `captured_at`. Substituting would make a
    /// runner that lost coord an hour ago render as freshly polled, which is
    /// the most convincing wrong answer this layer could give.
    ///
    /// The fourth cache DOES carry `fetched_at`, and that value must reach the
    /// row: the whole point of splitting the two facts is that the report can
    /// state the one it actually has.
    #[test]
    fn config_report_fleet_dial_row_refuses_to_invent_a_refresh_time() {
        use crate::mcp::fleet_policy_poller::BriefingDial;

        let d = dial(
            "off",
            "record",
            (Some(3_221_225_472), None, None, None),
            vec![
                BriefingDial {
                    name: "runner-session",
                    present: true,
                    version: Some(4),
                    fetched_at: Some("2026-08-22T09:00:00Z".to_string()),
                    provenance: Some("coord"),
                },
                BriefingDial {
                    name: "ai-session-rules",
                    present: false,
                    version: None,
                    fetched_at: None,
                    provenance: None,
                },
            ],
        );
        let reading = fleet_policy_dial_reading(&d, fixed_stamp());
        let LayerReading::Known { value, source, .. } = reading else {
            panic!("the runner app always resolves layer 10");
        };

        assert!(
            value.contains(
                "last refresh of the first three caches: UNKNOWN — those caches hold a value \
                 and no stamp; the report REFUSES to substitute its own read time"
            ),
            "the row must refuse to invent a refresh time: {value}"
        );
        // …and it must not have leaked the reading's own stamp in as one.
        assert!(
            !value.contains("2026-08-22T12:34:56"),
            "the read time must never appear as a refresh time: {value}"
        );

        // The cache that DOES know its refresh time reports it.
        assert!(
            value.contains("runner-session=v4 fetched_at 2026-08-22T09:00:00Z (coord)"),
            "the briefing cache's own stamp must reach the row: {value}"
        );
        assert!(
            value.contains(
                "ai-session-rules=absent (renderer falls back to the compiled-in builtin)"
            ),
            "an absent briefing is a reading about the CACHE: {value}"
        );

        // A `None` floor is "the fleet has no opinion", NEVER `0` — a zero
        // floor disables the guard it names.
        assert!(
            value.contains("host warn=3221225472 crit=(no fleet opinion)"),
            "got {value}"
        );
        assert!(
            value.contains("wsl warn=(no fleet opinion) crit=(no fleet opinion)"),
            "got {value}"
        );
        assert!(
            !value.contains("crit=0"),
            "a missing fleet term must never render as zero: {value}"
        );

        // A value equal to the resting default is AMBIGUOUS and says so;
        // one that is not, is not annotated.
        assert!(
            value.contains(
                "install_interception=off [= resting default: EITHER the fleet says so OR no \
                 poll has ever succeeded"
            ),
            "got {value}"
        );
        assert!(
            value.contains("plan_capture=record (armed at"),
            "a non-default level must not be annotated as ambiguous: {value}"
        );
        assert!(
            source.contains("TIME-VARYING with no restart"),
            "got {source}"
        );
    }

    // =======================================================================
    // Phase 4 — the CARRIERS (12, 13, 14).
    // =======================================================================

    /// Layer 12 states the file's EXISTENCE, which is the shim's actual gate,
    /// and names the variant that produced the file name.
    #[test]
    fn config_report_settings_carrier_row_states_existence_and_variant() {
        use crate::session::claude_hook::StopHookRegistration;
        use std::path::Path;

        let armed = claude_settings_carrier_reading(
            StopHookRegistration::Registered,
            Path::new("C:/hooks/claude_hook_settings.json"),
            true,
            None,
            fixed_stamp(),
        );
        let LayerReading::Known { value, source, .. } = armed else {
            panic!("the runner app always resolves layer 12");
        };
        assert!(
            value.contains("C:/hooks/claude_hook_settings.json"),
            "got {value}"
        );
        assert!(value.contains("on disk: true"), "got {value}");
        assert!(value.contains("variant `registered`"), "got {value}");
        assert!(
            value.contains("BOTH append `--settings` ONLY when that file exists (fail-open)"),
            "the row must state WHY existence is the load-bearing fact: {value}"
        );
        assert!(
            value.contains("direct_spawn_settings_args"),
            "the row must name BOTH appenders: an operator debugging a hookless autonomous              session is looking for the one that is not the shim: {value}"
        );
        assert!(
            value.contains("QONTINUI_CLAUDE_HOOK_SETTINGS onto a spawned CHILD"),
            "got {value}"
        );
        assert!(
            source.contains("QONTINUI_STOP_HOOK_CONTINUATION"),
            "the source must name the env var the variant is read from: {source}"
        );

        // The dark variant writes a DIFFERENT file, and a missing one means
        // spawned sessions get no hook at all.
        let dark = claude_settings_carrier_reading(
            StopHookRegistration::Omitted,
            Path::new("C:/hooks/claude_hook_settings-nostop.json"),
            false,
            None,
            fixed_stamp(),
        );
        let LayerReading::Known { value, .. } = dark else {
            panic!("layer 12 is never withheld");
        };
        assert!(
            value.contains("claude_hook_settings-nostop.json"),
            "got {value}"
        );
        assert!(value.contains("on disk: false"), "got {value}");
        assert!(value.contains("variant `omitted`"), "got {value}");
        assert!(
            value.contains("a `false` here means spawned sessions get NO hook"),
            "got {value}"
        );
    }

    /// Layer 13 is an OBSERVATION of the shim's input, and says so — it never
    /// claims to have read the other binary.
    #[test]
    fn config_report_mcp_config_carrier_row_is_an_observation_not_a_shim_read() {
        let absent = mcp_config_carrier_reading(None, false, fixed_stamp());
        let LayerReading::Known { value, source, .. } = absent else {
            panic!("the runner app always resolves layer 13");
        };
        assert!(
            value.contains("QONTINUI_MCP_CONFIG is not set in this process's env"),
            "got {value}"
        );
        assert!(
            value.contains("expected reading for a hand-launched runner"),
            "an absent carrier must not read as a fault: {value}"
        );
        assert!(
            source.contains("not in this address space and is not restated here"),
            "the source must refuse to claim a read of the shim binary: {source}"
        );

        let present = mcp_config_carrier_reading(
            Some("C:/AppData/qontinui/mcp-config-abc.json"),
            true,
            fixed_stamp(),
        );
        let LayerReading::Known { value, .. } = present else {
            panic!("layer 13 is never withheld");
        };
        assert!(
            value.contains("C:/AppData/qontinui/mcp-config-abc.json"),
            "got {value}"
        );
        assert!(
            value.contains("names an existing file: true"),
            "got {value}"
        );
    }

    /// **The carrier leak check.** Both carrier rows run their env value
    /// through `EnvVarReading::classify`, so a credential-shaped value is
    /// WITHHELD at ingestion and never reaches the rendered row.
    ///
    /// The fixture is a JWT-prefixed value, which the classifier catches by
    /// VALUE shape rather than by name — neither `QONTINUI_MCP_CONFIG` nor
    /// `QONTINUI_CLAUDE_HOOK_SETTINGS` contains a credential-class name token,
    /// so a name-only classifier would let this straight through. That is the
    /// arm this test exists to pin.
    #[test]
    fn config_report_carrier_rows_withhold_a_credential_shaped_value() {
        use crate::session::claude_hook::StopHookRegistration;
        use std::path::Path;

        let jwt = "eyJhbGciOiJFZERTQSJ9.cGF5bG9hZA.c2ln";

        let LayerReading::Known { value, .. } =
            mcp_config_carrier_reading(Some(jwt), false, fixed_stamp())
        else {
            panic!("layer 13 is never withheld as a LAYER — the VALUE is");
        };
        assert!(
            !value.contains(jwt),
            "a credential-shaped value leaked: {value}"
        );
        assert!(value.contains("<withheld #"), "got {value}");
        assert!(
            value.contains("value prefix \"eyJ\""),
            "the reason must be stated: {value}"
        );

        let LayerReading::Known { value, .. } = claude_settings_carrier_reading(
            StopHookRegistration::Registered,
            Path::new("C:/hooks/claude_hook_settings.json"),
            true,
            Some(jwt),
            fixed_stamp(),
        ) else {
            panic!("layer 12 is never withheld");
        };
        assert!(
            !value.contains(jwt),
            "a credential-shaped value leaked: {value}"
        );
        assert!(value.contains("<withheld #"), "got {value}");
    }

    fn mcp_json(
        shape: crate::coord_mcp::McpJsonShape,
        safe: bool,
        instance: Option<&str>,
        owns: bool,
        proxy_port: Option<u16>,
    ) -> crate::coord_mcp::McpJsonReport {
        mcp_json_bound(shape, safe, instance, owns, proxy_port, Some(9876))
    }

    /// [`mcp_json`] with the BOUND port under test — `None` models a runner
    /// with no reachable Tauri state, where the comparison is UNKNOWN.
    fn mcp_json_bound(
        shape: crate::coord_mcp::McpJsonShape,
        safe: bool,
        instance: Option<&str>,
        owns: bool,
        proxy_port: Option<u16>,
        bound_port: Option<u16>,
    ) -> crate::coord_mcp::McpJsonReport {
        crate::coord_mcp::McpJsonReport {
            root: Some("D:/qontinui-root".to_string()),
            path: Some("D:/qontinui-root/.mcp.json".to_string()),
            exists: true,
            instance_name: instance.map(str::to_string),
            owns_shared_root_state: owns,
            this_runner_port: bound_port,
            proxy_port,
            shape,
            read_error: None,
            safe_to_write: safe,
        }
    }

    /// **The plan's D2.** The `coord_mcp_safe_to_write` decision was previously
    /// visible only as a runner-log `warn!`, so a secondary correctly refusing
    /// to hijack the shared root config looked exactly like one that never
    /// tried. Both verdicts must be readable in the row.
    #[test]
    fn config_report_mcp_json_row_surfaces_the_write_verdict() {
        use crate::coord_mcp::McpJsonShape;

        // A secondary REFUSED at the shared root — the case the guard exists
        // for, and the one that was invisible.
        let LayerReading::Known { value, source, .. } = mcp_json_reading(
            &mcp_json(
                McpJsonShape::OursProxy,
                false,
                Some("temp-abc"),
                false,
                Some(9901),
            ),
            fixed_stamp(),
        ) else {
            panic!("the runner app always resolves layer 14");
        };
        assert!(
            value.contains("coord_mcp_safe_to_write: REFUSED"),
            "the verdict must be in the row: {value}"
        );
        assert!(
            value.contains("previously visible only as a runner-log warning"),
            "got {value}"
        );
        assert!(
            value.contains("SECONDARY \"temp-abc\" — does NOT own shared root state"),
            "got {value}"
        );
        assert!(
            value.contains("names port 9901, which is NOT this runner's BOUND API port (9876)"),
            "a root file naming another runner's port is the diagnosable state: {value}"
        );
        assert!(
            source.contains("coord_mcp_write_verdict's OWN return value"),
            "the verdict must not be re-derived: {source}"
        );
        assert!(
            source.contains("asked WITHOUT its write-attempt warn!"),
            "the row must say it read the guard's core and not the warn!-emitting wrapper — \
             reporting on a refusal must not manufacture the log line recording one: {source}"
        );
        assert!(
            source.contains(
                "bearer and the proxy nonce the file carries are structurally unreportable"
            ),
            "got {source}"
        );

        // The primary, ALLOWED, on its own port.
        let LayerReading::Known { value, .. } = mcp_json_reading(
            &mcp_json(McpJsonShape::OursProxy, true, None, true, Some(9876)),
            fixed_stamp(),
        ) else {
            panic!("layer 14 is never withheld when a root resolves");
        };
        assert!(
            value.contains("coord_mcp_safe_to_write: ALLOWED"),
            "got {value}"
        );
        assert!(
            value.contains("PRIMARY (owns shared root state)"),
            "got {value}"
        );
        assert!(
            value.contains("names port 9876, which IS this runner's BOUND API port"),
            "got {value}"
        );
        assert!(value.contains("shape: ours_proxy"), "got {value}");

        // A NAMELESS secondary — detected by port, fails closed. The row must
        // not call it the primary just because it has no instance name.
        let LayerReading::Known { value, .. } = mcp_json_reading(
            &mcp_json(McpJsonShape::Foreign, false, None, false, None),
            fixed_stamp(),
        ) else {
            panic!("layer 14 is never withheld when a root resolves");
        };
        assert!(value.contains("NAMELESS SECONDARY"), "got {value}");
        assert!(value.contains("fails closed"), "got {value}");
        assert!(value.contains("shape: foreign"), "got {value}");
    }

    /// An unresolvable workspace root is UNKNOWN, and it refuses the "there is
    /// no shared config" reading in words — the same discipline layer 6's
    /// missing launch read follows.
    #[test]
    fn config_report_mcp_json_row_unknown_refuses_the_no_config_reading() {
        use crate::coord_mcp::{McpJsonReport, McpJsonShape};

        let no_root = McpJsonReport {
            root: None,
            path: None,
            exists: false,
            instance_name: None,
            owns_shared_root_state: true,
            this_runner_port: Some(9876),
            proxy_port: None,
            shape: McpJsonShape::NoRoot,
            read_error: None,
            safe_to_write: true,
        };
        match mcp_json_reading(&no_root, fixed_stamp()) {
            LayerReading::Unknown { reason, .. } => {
                assert!(reason.contains("resolved no umbrella root"), "{reason}");
                assert!(
                    reason.contains(
                        "absence of a reading, NOT a finding that there is no shared root config"
                    ),
                    "the reason must refuse the wrong inference: {reason}"
                );
            }
            other => panic!("an unresolvable root is UNKNOWN, got {other:?}"),
        }
    }

    /// The live command resolves all FIVE Phase-4 layers from this machine's
    /// real state. Guards the wiring the pure tests above deliberately bypass:
    /// a field left `None` in `config_report_inputs` compiles fine and shows up
    /// only here.
    #[test]
    fn config_report_live_command_injects_every_phase_4_layer() {
        let report = config_report_run();

        for (name, expected_source_fragment) in [
            ("coord_prompt_documents", "prompt_library::cache_health"),
            (
                "fleet_policy_dial",
                "mcp::fleet_policy_poller::dial_snapshot",
            ),
            (
                "claude_settings_carrier",
                "session::claude_hook::settings_path",
            ),
            (
                "mcp_config_carrier",
                "observed in this process's env (generation G1)",
            ),
            ("mcp_json", "coord_mcp::mcp_json_report"),
        ] {
            match &report.row(name).expect("row present").reading {
                LayerReading::Known { source, value, .. } => {
                    assert!(!value.is_empty(), "{name}: a known row must have a value");
                    assert!(
                        source.contains(expected_source_fragment),
                        "{name}: source must attribute {expected_source_fragment}, got {source}"
                    );
                }
                // Layer 14's only honest Unknown on this machine.
                LayerReading::Unknown { reason, .. } if name == "mcp_json" => assert!(
                    reason.contains("resolved no umbrella root"),
                    "{name}: unexpected Unknown: {reason}"
                ),
                other => panic!("{name}: the runner app resolves this layer, got {other:?}"),
            }
        }

        // …and after Phase 5 NOTHING in the LIVE report says "not yet
        // implemented". The lib-side twin of this assertion runs against a
        // headless payload; this one runs against the real in-app report, which
        // is the artifact an operator reads.
        let pending: Vec<&str> = report
            .rows
            .iter()
            .filter(|r| match &r.reading {
                LayerReading::Unknown { reason, .. } => reason.contains("not yet implemented"),
                _ => false,
            })
            .map(|r| r.spec.name)
            .collect();
        assert_eq!(
            pending,
            Vec::<&str>::new(),
            "every layer of the live report has a resolver"
        );

        // Run with `--nocapture` to read the five rows this phase landed, as
        // they resolve against THIS machine — the report an operator sees from
        // the in-app command.
        println!("\n[config-report evidence] Phase 4 rows on this machine:");
        for name in [
            "coord_prompt_documents",
            "fleet_policy_dial",
            "claude_settings_carrier",
            "mcp_config_carrier",
            "mcp_json",
        ] {
            println!("  {name}: {:?}", report.row(name).unwrap().reading);
        }
    }

    // =======================================================================
    // Phase 5 — layer 1 (whole-file settings provenance).
    // =======================================================================

    /// Every provenance arm, with the vocabulary and the load-bearing sentences
    /// asserted as LITERALS.
    ///
    /// The wire strings (`loaded` / `fresh_install` / `unreadable`) are a
    /// contract a reader compares across machines, so asserting them against
    /// `SettingsProvenance::as_str()` would pin nothing — the same reasoning
    /// layer 3's arm test spells out.
    ///
    /// The three arms are not interchangeable and the test says why for each:
    /// `fresh_install` and `unreadable` BOTH ship a `Settings::default()`, and
    /// the only thing separating "defaults are correct, there was nothing to
    /// load" from "defaults are a placeholder standing in for the user's real
    /// identity fields" is this row.
    #[test]
    fn config_report_settings_struct_reading_covers_every_provenance_arm() {
        use crate::settings::SettingsProvenance;

        let path = || Ok(std::path::PathBuf::from("C:/cfg/settings.json"));

        // `loaded` — the user's real state.
        let loaded =
            settings_struct_reading(SettingsProvenance::Loaded, None, path(), fixed_stamp());
        let LayerReading::Known { value, source, .. } = &loaded else {
            panic!("every provenance arm is Known, got {loaded:?}");
        };
        assert!(
            value.starts_with("C:/cfg/settings.json — provenance `loaded`: "),
            "the path and the arm lead the value: {value}"
        );
        assert!(
            value.contains("IS the user's real persisted state"),
            "the loaded arm must say the values are the user's: {value}"
        );
        // The source names the NON-MUTATING reader, as a LITERAL: this string
        // is what tells a reader the row did not run the migration/persist path
        // that `load_settings_full` does, and asserting it against the constant
        // that produced it would pin nothing.
        assert_eq!(
            source,
            "settings::read_settings_from_disk → settings::SettingsProvenance (WHOLE-FILE \
             provenance of settings.json, read WITHOUT the roster migration, the \
             local_user_id/tier persist and the keyring read that `load_settings_full` performs; \
             NOT per-field attribution)",
        );

        // `fresh_install` — authoritative, and NOTHING came from disk.
        let fresh = settings_struct_reading(
            SettingsProvenance::FreshInstall,
            None,
            path(),
            fixed_stamp(),
        );
        let LayerReading::Known { value, .. } = &fresh else {
            panic!("fresh_install is Known, got {fresh:?}");
        };
        assert!(
            value.contains("provenance `fresh_install`: NO settings.json exists"),
            "the fresh arm must name the arm and the absence: {value}"
        );
        assert!(
            value.contains("nothing in the struct came from disk"),
            "a first run must not read as 'these are the user's values': {value}"
        );

        // `unreadable` — the arm the row exists for.
        let unreadable = settings_struct_reading(
            SettingsProvenance::Unreadable,
            Some("parse failed: expected value at line 1 column 1"),
            path(),
            fixed_stamp(),
        );
        let LayerReading::Known { value, .. } = &unreadable else {
            panic!(
                "unreadable is Known, not Unknown — it is a fact about the machine, not a \
                    limit of the observer; got {unreadable:?}"
            );
        };
        assert!(
            value.contains("provenance `unreadable`:"),
            "the unreadable arm must name the arm: {value}"
        );
        assert!(
            value.contains("parse failed: expected value at line 1 column 1"),
            "the REASON is the actionable half of this arm: {value}"
        );
        assert!(
            value.contains("DEFAULT PLACEHOLDER"),
            "the row must refuse to let a placeholder read as the user's state: {value}"
        );
        assert!(
            value.contains("web_integration.runner_token"),
            "the row must name which fields are not the user's: {value}"
        );

        // Every arm carries the not-per-field caveat, in the OUTPUT — a reader
        // who took `loaded` for per-field attribution is worse off than one
        // with no row.
        for (arm, reading) in [
            ("loaded", &loaded),
            ("fresh_install", &fresh),
            ("unreadable", &unreadable),
        ] {
            let LayerReading::Known { value, .. } = reading else {
                unreachable!()
            };
            assert!(
                value.contains("WHOLE-FILE only")
                    && value.contains(
                        "Per-field attribution was considered and deliberately not \
                                       built"
                    ),
                "{arm}: the row must state its grain and that the finer one was a decision: \
                 {value}"
            );
        }
    }

    /// An unresolvable settings path is NAMED as unresolvable rather than
    /// rendered as some plausible file. Same refusal layer 2 makes for a failed
    /// `get_config_dir`.
    #[test]
    fn config_report_settings_struct_reading_refuses_to_invent_a_path() {
        let reading = settings_struct_reading(
            crate::settings::SettingsProvenance::Unreadable,
            Some("cannot resolve settings path: Failed to get config directory"),
            Err("Failed to get config directory".to_string()),
            fixed_stamp(),
        );
        let LayerReading::Known { value, .. } = &reading else {
            panic!("got {reading:?}");
        };
        assert!(
            value.starts_with("(path unresolvable: Failed to get config directory) — "),
            "an unresolved path must say so: {value}"
        );
        assert!(
            !value.contains("settings.json —"),
            "the row must not name a settings.json it never resolved: {value}"
        );
    }

    /// **The layer-1 leak check.** The settings load error is free text
    /// assembled from an OS or serde message — the shape nobody audits — so it
    /// goes through `EnvVarReading::classify` before it is rendered.
    ///
    /// The fixture is a token-shaped blob, which the classifier catches by VALUE
    /// entropy: the name it is classified under (`settings_load_error`) carries
    /// no credential-class token, so a name-only classifier would pass this
    /// straight through. That is the arm this test pins.
    #[test]
    fn config_report_settings_struct_row_withholds_a_credential_shaped_error() {
        // Split so the source carries no contiguous high-entropy literal next to a
        // credential keyword — gitleaks' `generic-api-key` fires on that shape.
        // `concat!` is compile-time, so the value and type are unchanged.
        let secret = concat!("AbCdEf0123456789", "AbCdEf0123456789xyz");
        let reading = settings_struct_reading(
            crate::settings::SettingsProvenance::Unreadable,
            Some(secret),
            Ok(std::path::PathBuf::from("C:/cfg/settings.json")),
            fixed_stamp(),
        );
        let LayerReading::Known { value, .. } = &reading else {
            panic!("got {reading:?}");
        };
        assert!(!value.contains(secret), "the error value leaked: {value}");
        assert!(
            value.contains("<withheld #"),
            "a withheld error must render as its fingerprint: {value}"
        );
    }

    /// The live command resolves layer 1 from this machine's real state, and the
    /// arm it reports is one of the three documented wire strings (LITERAL
    /// list). Guards the wiring the pure tests above bypass.
    #[test]
    fn config_report_live_command_injects_the_settings_struct_layer() {
        let report = config_report_run();
        match &report.row("settings_struct").expect("row present").reading {
            LayerReading::Known { value, source, .. } => {
                assert!(
                    ["`loaded`", "`fresh_install`", "`unreadable`"]
                        .iter()
                        .any(|arm| value.contains(arm)),
                    "layer 1 must report a documented provenance arm: {value}"
                );
                assert!(
                    source.contains("settings::read_settings_from_disk"),
                    "layer 1 must attribute the NON-MUTATING provenance reader: {source}"
                );
                // `load_settings` discards provenance by design, so a row
                // attributing it would be describing a call that cannot answer
                // this layer.
                assert!(
                    !source.contains("load_settings ("),
                    "layer 1 must not attribute the provenance-discarding reader: {source}"
                );
                println!("\n[config-report evidence] layer 1 on this machine:\n  {value}");
            }
            other => panic!("the runner app resolves layer 1, got {other:?}"),
        }
    }

    /// **No `Settings` FIELD VALUE reaches the render.** Checked against this
    /// machine's REAL settings document rather than a fixture: the identity
    /// fields are read here, in the test, and then asserted absent from the full
    /// rendered report.
    ///
    /// This is the guarantee the layer-1 doc comment claims, and it is the one a
    /// future edit is most likely to break — adding "just the tier" or "just the
    /// backend URL" to the row is a one-line change that this test, and only
    /// this test, would catch.
    #[test]
    fn config_report_live_render_carries_no_settings_field_value() {
        // The NON-MUTATING reader, deliberately: this test used to call
        // `load_settings_full`, which mints a `local_user_id` and persists it —
        // a test that writes the operator's settings.json to check that the
        // report does not.
        let loaded = crate::settings::read_settings_from_disk();
        let rendered = config_report_run().render();

        let mut checked = 0usize;
        let mut identity: Vec<(&str, String)> = vec![
            (
                "web_integration.runner_token",
                loaded.settings.web_integration.runner_token.clone(),
            ),
            ("local_user_id", loaded.settings.local_user_id.clone()),
        ];
        if let Some(uid) = loaded.settings.qontinui_user_id.clone() {
            identity.push(("qontinui_user_id", uid));
        }
        for (field, value) in identity {
            // Short/empty values are not evidence of anything — a 4-character
            // string can appear inside a path by coincidence.
            if value.trim().len() < 8 {
                continue;
            }
            checked += 1;
            assert!(
                !rendered.contains(value.trim()),
                "the value of Settings::{field} reached the rendered report"
            );
        }
        println!(
            "[config-report evidence] identity-bearing Settings fields checked against the full \
             render: {checked}"
        );
    }

    /// **F3 regression.** The G1→G3 divergence includes `PATH`, and G3's own
    /// `describes` line names what it does NOT include.
    ///
    /// G3's heading tells the reader that *"anything listed above is a variable
    /// the runner process itself does NOT hold the current value of"*. Built
    /// from `new_default_prog()` + `finalize_child_env` alone, it showed **no
    /// `PATH` delta at all** — so an operator asking why `cargo` in a runner
    /// pane resolves to the interception shim got a confidently wrong answer
    /// about the one mechanism that decides which binary a child resolves.
    ///
    /// Two halves, because the seam splits in two: the parts that can be
    /// replicated without writing are CAPTURED (asserted here as real deltas),
    /// and the parts that cannot are NAMED (asserted here as text).
    #[test]
    fn config_report_g3_captures_the_spawn_seam_and_names_what_it_omits() {
        let section = env_generations_section(None, fixed_stamp());
        let g3 = section
            .generations
            .iter()
            .find(|g| g.name == "pty_child")
            .expect("G3 is captured");

        // The base seam's own variables reach G3 — LITERAL names, because these
        // are the ones a reader compares against a live `env` in a pane.
        for name in [
            "TERM",
            "QONTINUI_RUNNER_TERMINAL",
            "QONTINUI_RUNNER_API_PORT",
            "QONTINUI_RUNNER_CONTEXT",
        ] {
            assert!(
                g3.get(name).is_some(),
                "{name} is set by the spawn seam and must be in G3"
            );
        }
        assert_eq!(
            g3.get("TERM").map(|v| v.cell()),
            Some("xterm-256color".to_string()),
            "G3 must carry the value the seam actually sets"
        );
        assert_eq!(
            g3.get("QONTINUI_RUNNER_TERMINAL").map(|v| v.cell()),
            Some("1".to_string())
        );

        // The divergence must NAME the path key. Windows uses `Path`, every
        // other platform `PATH`, and `set_child_path` deliberately collapses
        // the case-variants — so accept the one this platform uses.
        let divergence = section.divergences.first().expect("G1 → G3 is captured");
        let names: Vec<&str> = divergence.deltas.iter().map(|d| d.name()).collect();
        let path_key = if cfg!(windows) { "Path" } else { "PATH" };

        // NOTE the asymmetry with the block above: a seam variable is a DELTA
        // only when this process does not already hold the same value, and a
        // test process spawned from a runner pane inherits TERM and the
        // QONTINUI_RUNNER_* markers already. So PRESENCE in G3 is asserted
        // above, and the divergence is asserted only where it is unconditional.

        // `PATH` is a delta exactly when the identity-shim dir for this build
        // is materialized — and G3's freshness line says which case this is,
        // so the two can never disagree.
        let shim_dir = crate::install_effects_producer::intercept::shim_materializer::identity_dir_if_materialized(
            &std::env::temp_dir(),
        );
        if shim_dir.is_some() {
            assert!(
                names.contains(&path_key),
                "the identity shim IS materialized, so {path_key} must appear in the divergence — \
                 it is the whole answer to \"why does a pane resolve `claude` to the shim?\": \
                 {names:?}"
            );
            assert_eq!(g3.freshness, G3_FRESHNESS_WITH_SHIM);
            assert!(g3.freshness.contains("SHIM-PREPENDED"), "{}", g3.freshness);
        } else {
            assert_eq!(g3.freshness, G3_FRESHNESS_NO_SHIM);
            assert!(
                g3.freshness.contains("UN-SHIMMED"),
                "an un-shimmed capture must say so rather than letting the reader assume the \
                 PATH shown is what a child gets: {}",
                g3.freshness
            );
        }

        // …and the bounded claim. Each of these is a seam step this report
        // refuses to reproduce because reproducing it WRITES.
        for omitted in [
            "identity seam",
            "coord-mcp",
            "install-interception",
            "extra_env",
        ] {
            assert!(
                g3.describes.contains(omitted),
                "G3 must name {omitted} as excluded rather than leaving an unqualified claim: {}",
                g3.describes
            );
        }
        assert!(
            g3.describes.contains("apply_base_child_env")
                && g3.describes.contains("identity-shim PATH prepend")
                && g3.describes.contains("finalize_child_env"),
            "G3 must name what it DOES include too: {}",
            g3.describes
        );

        println!(
            "\n[config-report evidence] G1 → G3 deltas on this machine ({} total): {:?}",
            names.len(),
            names
        );
    }

    /// **F4 regression.** Layer 14 compares the file's port against the port
    /// this runner is BOUND to, and says UNKNOWN when it cannot establish one.
    ///
    /// The inversion this pins: on a primary that fell back off a blocked 9876
    /// to 9877 (`mcp_api`'s `[port, port+1, port+2]` loop — the Windows
    /// zombie-socket path this layer exists to diagnose), comparing against the
    /// CONFIGURED port both cries wolf on a correct file and clears a stale
    /// one. Ports are LITERALS here for that reason.
    #[test]
    fn config_report_mcp_json_row_compares_the_bound_port_not_the_configured_one() {
        use crate::coord_mcp::McpJsonShape;

        // A runner that fell back to 9877, with a CORRECTLY rewritten file.
        // Against the configured 9876 this used to read as a mismatch.
        let LayerReading::Known { value, .. } = mcp_json_reading(
            &mcp_json_bound(
                McpJsonShape::OursProxy,
                true,
                None,
                true,
                Some(9877),
                Some(9877),
            ),
            fixed_stamp(),
        ) else {
            panic!("layer 14 resolves when a root resolves");
        };
        assert!(
            value.contains("names port 9877, which IS this runner's BOUND API port"),
            "a healthy fallback runner must not be reported as broken: {value}"
        );

        // The same runner with a STALE file naming the configured port. Against
        // the configured port this used to read as an all-clear.
        let LayerReading::Known { value, .. } = mcp_json_reading(
            &mcp_json_bound(
                McpJsonShape::OursProxy,
                true,
                None,
                true,
                Some(9876),
                Some(9877),
            ),
            fixed_stamp(),
        ) else {
            panic!("layer 14 resolves when a root resolves");
        };
        assert!(
            value.contains("names port 9876, which is NOT this runner's BOUND API port (9877)"),
            "a stranded root config is exactly what this row exists to catch: {value}"
        );

        // No reachable Tauri state: UNKNOWN, never a substituted env value.
        let LayerReading::Known { value, .. } = mcp_json_reading(
            &mcp_json_bound(McpJsonShape::OursProxy, true, None, true, Some(9876), None),
            fixed_stamp(),
        ) else {
            panic!("layer 14 resolves when a root resolves");
        };
        assert!(
            value.contains("THIS RUNNER'S BOUND PORT IS UNKNOWN"),
            "an unresolvable bound port must be stated: {value}"
        );
        assert!(
            value.contains("absence of a check, not a finding that the port matches"),
            "the row must refuse the wrong inference: {value}"
        );
        assert!(
            !value.contains("IS this runner's BOUND API port"),
            "the row must not assert a match it could not establish: {value}"
        );
    }

    /// **F7 regression, row side.** A present-but-unreadable file renders as
    /// `unparseable` WITH its reason, never as `absent` next to `on disk:
    /// true`. (The mapping itself is pinned in
    /// `coord_mcp::tests::mcp_json_read_error_is_unparseable_not_absent_unless_it_is_notfound`.)
    #[test]
    fn config_report_mcp_json_row_distinguishes_unreadable_from_missing() {
        use crate::coord_mcp::{McpJsonReport, McpJsonShape};

        let locked = McpJsonReport {
            root: Some("D:/qontinui-root".to_string()),
            path: Some("D:/qontinui-root/.mcp.json".to_string()),
            exists: true,
            instance_name: None,
            owns_shared_root_state: true,
            this_runner_port: Some(9876),
            proxy_port: None,
            shape: McpJsonShape::Unparseable,
            read_error: Some("The process cannot access the file (os error 32)".to_string()),
            safe_to_write: true,
        };
        let LayerReading::Known { value, .. } = mcp_json_reading(&locked, fixed_stamp()) else {
            panic!("layer 14 resolves when a root resolves");
        };
        assert!(value.contains("on disk: true"), "got {value}");
        assert!(
            value.contains("shape: unparseable_or_no_mcp_servers"),
            "a locked file is present-and-unusable, not missing: {value}"
        );
        assert!(
            value.contains("The process cannot access the file (os error 32)"),
            "the reason is the whole difference from `absent`: {value}"
        );
        assert!(
            value.contains("present-and-unusable, never missing"),
            "the row must say so in words: {value}"
        );
    }

    /// **F5 regression, row side.** Layer 2 STATS the directory it reports and
    /// cannot create it, whatever it is handed.
    ///
    /// This is the half a live-report fingerprint check cannot reach: on a
    /// machine whose config dir already exists, calling the CREATING resolver
    /// writes nothing and looks identical. Here the path is one the test owns
    /// and deliberately does not create, so the assertion has something to see.
    #[test]
    fn config_report_config_dir_row_stats_and_never_creates() {
        use crate::settings::ConfigDirSource;

        let tmp = tempfile::tempdir().expect("tempdir");

        // A configured directory that does NOT exist — the typo'd
        // `QONTINUI_CONFIG_DIR` case, which is the whole point of the row.
        let absent = tmp.path().join("qonitnui-typo");
        let reading = config_dir_reading(
            Ok((absent.clone(), ConfigDirSource::EnvConfigDir)),
            fixed_stamp(),
        );
        let LayerReading::Known { value, source, .. } = &reading else {
            panic!("a resolved config dir is always Known, got {reading:?}");
        };
        assert!(value.contains("on disk: false"), "got {value}");
        assert!(
            value.contains("STATTED, never created"),
            "the row must say which it did: {value}"
        );
        assert_eq!(source, "env:QONTINUI_CONFIG_DIR");
        assert!(
            !absent.exists(),
            "reading the layer materialized the directory it was describing"
        );

        // The same row for a directory that DOES exist.
        let present = tmp.path().join("real");
        std::fs::create_dir_all(&present).expect("mkdir");
        let reading = config_dir_reading(
            Ok((present, ConfigDirSource::PlatformConfigDir)),
            fixed_stamp(),
        );
        let LayerReading::Known { value, source, .. } = &reading else {
            panic!("got {reading:?}");
        };
        assert!(value.contains("on disk: true"), "got {value}");
        assert_eq!(source, "platform_config_dir");

        // An unresolvable directory is UNKNOWN and quotes the NON-CREATING
        // resolver by name — a reader sent to `get_config_dir` would be sent to
        // the function this row must never call.
        let reading = config_dir_reading(
            Err("Failed to get config directory".to_string()),
            fixed_stamp(),
        );
        let LayerReading::Unknown { reason, .. } = &reading else {
            panic!("an unresolvable dir is UNKNOWN, got {reading:?}");
        };
        assert_eq!(
            reason,
            "settings::resolve_config_dir() failed: Failed to get config directory"
        );
    }

    /// **F5 regression.** Running the live report writes none of the files it
    /// reports on — **with one bounded, named exception, below.**
    ///
    /// Layer 1 used to call `settings::load_settings_full`, which is not a read:
    /// it runs `claude_accounts::load_with_migration` (writing
    /// `claude-accounts.json`), can mint a `local_user_id` UUID plus a tier
    /// migration and call `save_settings` — **overwriting the operator's real
    /// settings.json** — and reaches the OS keyring via
    /// `AuthManager::new().get_access_token()`. Layer 2 called
    /// `settings::get_config_dir`, which `create_dir_all`s, so the report
    /// MATERIALIZED a typo'd `QONTINUI_CONFIG_DIR` and then reported it as
    /// present. Two tests in this file drive the live command, so `cargo test`
    /// on a dev box did all of it.
    ///
    /// # The exception the old absolute claim omitted
    ///
    /// Layer 11 goes `get_effective_config_dir` → `oauth_refresh::has_valid_credentials`
    /// → `creds_path_is_valid`, which calls `request_background_refresh` when the
    /// selected account's token is within `REFRESH_LEAD_MS` of expiry. **In a
    /// release build that POSTs the token endpoint off-thread and rewrites
    /// `<effective_config_dir>/.credentials.json`.** So "the live report writes
    /// nothing" was false as stated, and a comment asserting a protection the
    /// code does not provide is the defect class this whole report exists to
    /// expose.
    ///
    /// It fires whenever the SELECTED account is within `REFRESH_LEAD_MS`
    /// (10 minutes) of expiry — a routine condition on a working box, not an
    /// edge case, and the comment should not read as though it were one.
    ///
    /// It is genuinely bounded in one respect: in-process it shares
    /// `REFRESH_STATE` with the runner, so it cannot double-refresh what the
    /// runner is already refreshing.
    ///
    /// # Two claims this comment used to make that are FALSE
    ///
    /// 1. *"the one write the report cannot decline without re-implementing
    ///    `get_effective_config_dir`"*. Not so, and this module is its own
    ///    counter-example: it performs the identical injection FOUR times
    ///    already — `api_config::api_base_url_inputs_from`,
    ///    `workspace_paths::workspace_root_from`, `coord_mcp_write_verdict_at`
    ///    and `settings::resolve_config_dir` are each called with the pure
    ///    inputs rather than re-deriving a rule. A
    ///    `get_effective_config_dir_with_validator` twin taking the credential
    ///    predicate as a parameter would re-implement NO selection rule: the
    ///    mode walk, the `LeastUsage` resolved-dir precedence and the
    ///    `Rejected`/`Unconfigured` arms would all still live in
    ///    `ai_provider::config`, exactly once. The write is therefore declinable
    ///    and simply has NOT been declined — the cost is a new public seam with
    ///    one caller, and the write is bounded and shared-state-deduped, so the
    ///    trade was judged not worth taking. That is a choice, not an
    ///    impossibility, and saying otherwise is the same defect class this
    ///    report exists to expose.
    /// 2. *the `cfg(test)` stub makes `.credentials.json` watchable*. Backwards.
    ///    Under `cfg(test)` `request_background_refresh` RECORDS the request and
    ///    performs no network call, so the ONE path this test names is the one
    ///    path it structurally cannot observe: the refresh write never happens
    ///    in a test binary, whether or not the production code would have made
    ///    it. What the watch actually covers is every OTHER writer of that file
    ///    reachable from a report run — which is worth having, and is what the
    ///    row below is honestly asserting — while the disclosed refresh write
    ///    stays out of reach of this suite entirely.
    ///
    /// This fingerprints every file the readers could write, runs the full
    /// report, and re-fingerprints. Two halves are pinned elsewhere because they
    /// cannot fail here: non-creation of the config dir by
    /// `settings::…::resolve_config_dir_creates_nothing`, which owns the path it
    /// checks, and non-entry into the settings writer by
    /// [`config_report_never_reaches_the_settings_writer`] — this test's
    /// fingerprints match on a dev box whether or not the writer ran, because
    /// boot has already consumed the one-shot migration.
    #[test]
    fn config_report_live_command_writes_nothing_it_reports_on() {
        fn fingerprint(
            path: &std::path::Path,
        ) -> (bool, Option<u64>, Option<std::time::SystemTime>) {
            match std::fs::metadata(path) {
                Ok(md) => (true, Some(md.len()), md.modified().ok()),
                Err(_) => (false, None, None),
            }
        }

        let watched: Vec<std::path::PathBuf> = [
            crate::settings::resolve_settings_path().ok(),
            crate::claude_accounts::claude_accounts_file_path(),
            crate::settings::resolve_config_dir().ok().map(|(d, _)| d),
            // The credentials file of the account layer 11 resolves to — the
            // one file the report's own call graph can reach a writer for. The
            // dir comes from `settings_derived_inputs()`, i.e. from
            // `get_effective_config_dir` itself; re-deriving the selection rule
            // here to name the path would be the defect the module forbids.
            settings_derived_inputs()
                .claude_config_dir
                .0
                .map(|d| std::path::PathBuf::from(d).join(".credentials.json")),
        ]
        .into_iter()
        .flatten()
        .collect();
        assert!(
            !watched.is_empty(),
            "the test must actually be watching something"
        );

        let before: Vec<_> = watched.iter().map(|p| fingerprint(p)).collect();
        let report = config_report_run();
        let after: Vec<_> = watched.iter().map(|p| fingerprint(p)).collect();

        for (i, path) in watched.iter().enumerate() {
            assert_eq!(
                before[i],
                after[i],
                "running the config report changed {} — a diagnostic that materializes the thing \
                 it is describing changes the answer by asking the question",
                path.display()
            );
        }

        // …and the rows still resolved, so this is not a vacuous pass over a
        // report that failed to run.
        assert_eq!(report.rows.len(), 15);
        match &report.row("settings_struct").expect("row present").reading {
            LayerReading::Known { source, .. } => assert!(
                source.contains("read_settings_from_disk"),
                "layer 1 must name the NON-MUTATING reader: {source}"
            ),
            other => panic!("got {other:?}"),
        }
        match &report.row("config_dir").expect("row present").reading {
            LayerReading::Known { value, .. } => assert!(
                value.contains("STATTED, never created"),
                "layer 2 must report existence rather than ensure it: {value}"
            ),
            LayerReading::Unknown { reason, .. } => assert!(
                reason.contains("settings::resolve_config_dir() failed"),
                "layer 2's only honest Unknown quotes the NON-CREATING resolver: {reason}"
            ),
            other => panic!("got {other:?}"),
        }

        println!(
            "[config-report evidence] files fingerprinted across a live report run: {}",
            watched.len()
        );
    }

    /// **The end-to-end leak check for the carriers**, against this machine's
    /// REAL environment. Nothing is planted: for every variable the INDEPENDENT
    /// heuristic calls credential-bearing, its actual value must appear nowhere
    /// in the full rendered report — which includes three credential-adjacent
    /// carrier rows.
    ///
    /// Scoped by [`looks_credential_bearing_independently`] for the same reason
    /// as its sibling above: gating a leak check on the classifier under test
    /// makes it blind to precisely the values that leak.
    ///
    /// # The seeded control, and why the loop alone was worthless here
    ///
    /// The loop iterates the test machine's own environment and asserts
    /// nothing about `checked`. On a clean box — CI, a fresh shell, a container —
    /// it runs ZERO iterations and passes having examined nothing, and it never
    /// covered a LAYER row in the first place: a layer value is not an
    /// environment variable, so the only rows the loop can catch are ones that
    /// happen to echo a variable this process is carrying.
    ///
    /// So a credential-bearing URL is PLANTED into the layer-5 inputs and the
    /// whole report — every row, rendered — is asserted not to contain it. That
    /// is the arm capable of catching a leaking layer row, and before the fix it
    /// failed.
    #[test]
    fn config_report_live_full_render_leaks_no_credential_value() {
        let rendered = config_report_run().render();
        let mut checked = 0usize;
        // `vars_os` + lossy, not `vars()`: the leak check must not itself
        // panic on the non-Unicode machine it is most needed on.
        for (name, value) in lossy_env_pairs(std::env::vars_os()) {
            if !looks_credential_bearing_independently(&name, &value) {
                continue;
            }
            checked += 1;
            assert!(
                !rendered.contains(&value),
                "the value of {name} reached the FULL rendered report"
            );
        }

        // THE SEEDED CONTROL — an assertion that can fail on any machine.
        let planted = "https://ops:S3cretPw@qontinui.internal";
        assert!(
            looks_credential_bearing_independently("QONTINUI_WEB_BACKEND_URL", planted),
            "the plant must be credential-bearing to the INDEPENDENT heuristic, or the control \
             proves nothing"
        );
        let mut seeded = config_report_inputs();
        seeded.api_endpoint_registry = Some(api_base_url_reading(
            &inputs(Some(planted), None, None, true),
            fixed_stamp(),
        ));
        let seeded_render = build_report(&seeded).render();
        assert!(
            !seeded_render.contains("S3cretPw"),
            "a credential planted in a LAYER row reached the full render:\n{seeded_render}"
        );
        assert!(
            !seeded_render.contains("qontinui.internal"),
            "a withheld layer value must carry no part of the URL:\n{seeded_render}"
        );
        // …and the row is still THERE, saying it withheld. A row that vanished
        // would also satisfy the two assertions above.
        let row_value = match &build_report(&seeded)
            .row("api_endpoint_registry")
            .expect("layer 5 has a row")
            .reading
        {
            LayerReading::Known { value, source, .. } => {
                assert_eq!(source, "env:QONTINUI_WEB_BACKEND_URL");
                value.clone()
            }
            other => panic!("layer 5 must stay Known when its value is withheld, got {other:?}"),
        };
        assert!(
            row_value.starts_with("<withheld #"),
            "the row must render the withholding, not disappear: {row_value}"
        );

        println!(
            "[config-report evidence] env values flagged by the INDEPENDENT heuristic and \
             checked against the full render: {checked}; plus 1 seeded layer-5 credential"
        );
    }

    /// **The systemic gap the two live leak checks above cannot close** — nine
    /// confirmed `url_userinfo` false negatives, PLANTED, so coverage does not
    /// depend on what this box happens to export.
    ///
    /// Both live checks iterate `std::env::vars_os()`, and their own evidence
    /// lines admit `checked` can be 0. So on CI, in a container, or in any
    /// shell that carries none of these shapes, they examine none of them —
    /// which is exactly how this family survived three rewrites of the parser
    /// while the suite stayed green. Every value below was returning `None`
    /// from `url_userinfo` and printing VERBATIM:
    ///
    /// - an **empty username** — `redis://:password@host` is the canonical
    ///   pre-ACL Redis form, and this crate builds `redis://` URLs and exports
    ///   `REDIS_URL` itself (`ci_node::services`, `ci_node::executor`,
    ///   `bin/qontinui_profile`; `ci_node::manifest` names it alongside
    ///   `DATABASE_URL` as a credential-bearing family);
    /// - an **`@` inside the username** — mandated by Azure Database for
    ///   PostgreSQL/MySQL Single Server (`user@servername`), and normal
    ///   wherever an email is the account name;
    /// - the **RFC 3986 sub-delims**, which are legal unencoded in userinfo;
    /// - a **non-ASCII (IDN) host**.
    ///
    /// Three assertions per row, each able to fail on its own:
    ///
    /// 1. [`looks_credential_bearing_independently`] — which never consults the
    ///    classifier — calls it credential-bearing. Without this the rest is
    ///    vacuous, which is the failure mode these tests exist to refuse.
    /// 2. `classify_env_var` withholds it under the VALUE-shape arm. The names
    ///    are chosen so nothing else can rescue them: none matches a
    ///    `CREDENTIAL_NAME_TOKENS` entry, none ends in `_URL`/`_URI`/`_DSN`, and
    ///    the entropy charset admits neither `:` nor `@`.
    /// 3. The rendered section carries the secret nowhere AND still shows the
    ///    variable as a withheld row — a row that vanished would satisfy the
    ///    absence assertion trivially.
    #[test]
    fn config_report_planted_credential_urls_never_reach_the_render() {
        use qontinui_runner_lib::env_generations::{classify_env_var, WithholdReason};

        // (name, value, the substring that must not survive anywhere)
        let planted: &[(&str, &str, &str)] = &[
            (
                "QONTINUI_REDIS_PROBE",
                "redis://:s3cretpw@127.0.0.1:6379/0",
                "s3cretpw",
            ),
            (
                "QONTINUI_REDISS_PROBE",
                "rediss://:s3cretpw@cache.internal:6380",
                "s3cretpw",
            ),
            (
                "QONTINUI_AMQP_PROBE",
                "amqp://:guestpw@rabbit.internal:5672/%2f",
                "guestpw",
            ),
            (
                "QONTINUI_GIT_REMOTE_PROBE",
                "https://:ghp_A1b2C3d4E5f6G7h8@github.com/o/r.git",
                "ghp_A1b2C3d4E5f6G7h8",
            ),
            (
                "QONTINUI_AZURE_PG_PROBE",
                "postgres://myadmin@mydemoserver:mypassword@srv.postgres.database.azure.com:5432/db",
                "mypassword",
            ),
            (
                "QONTINUI_ATLAS_PROBE",
                "mongodb+srv://ops@corp.com:hunter2@cluster0.mongodb.net/test",
                "hunter2",
            ),
            (
                "QONTINUI_APOSTROPHE_PROBE",
                "postgres://o'brien:hunter2@db.internal:5432/app",
                "hunter2",
            ),
            (
                "QONTINUI_SUBDELIM_PROBE",
                "postgres://svc!x:hunter2@db.internal:5432/app",
                "hunter2",
            ),
            (
                "QONTINUI_IDN_PROBE",
                "https://u:idnpassw0rd@münchen.example.com/x",
                "idnpassw0rd",
            ),
        ];

        for (name, value, secret) in planted {
            assert!(
                looks_credential_bearing_independently(name, value),
                "{name} must be credential-bearing to the INDEPENDENT heuristic, or the row \
                 below proves nothing"
            );
            assert_eq!(
                classify_env_var(name, value),
                Some(WithholdReason::ValueUrlPassword),
                "{name}={value} must be withheld by the VALUE-shape arm"
            );
            assert!(
                value.contains(secret),
                "{name}: the fixture's own secret must be IN the value, or assertion (3) is \
                 vacuous"
            );
        }

        let fp = EnvFingerprinter::new();
        let rendered = EnvGenerations {
            generations: vec![EnvGeneration::capture(
                &fp,
                EnvGenerationSpec {
                    id: "G1",
                    name: "runner_process",
                    describes: "seeded",
                    freshness: "seeded",
                    is_full_env: true,
                },
                fixed_stamp(),
                planted.iter().map(|(n, v, _)| (*n, *v)),
            )],
            divergences: vec![],
            launch_drift: None,
            seams: vec![],
        }
        .render();

        for (name, value, secret) in planted {
            assert!(
                !rendered.contains(secret),
                "{name}: the password reached the render:\n{rendered}"
            );
            assert!(
                !rendered.contains(value),
                "{name}: the whole connection string reached the render:\n{rendered}"
            );
            assert!(
                rendered
                    .lines()
                    .any(|l| l.trim_start().starts_with(name) && l.contains("| <withheld #")),
                "{name} must be present AS A WITHHELD ROW, not merely absent:\n{rendered}"
            );
        }
        assert!(
            rendered.contains(&format!(
                "variables:   {} ({} withheld)",
                planted.len(),
                planted.len()
            )),
            "all {} planted variables must be counted as withheld:\n{rendered}",
            planted.len()
        );

        // …and the same family through the LAYER path, which the env-generation
        // section does not cover: layer 5 renders a resolved URL through
        // `EnvVarReading::classify` under the arm's own origin name. The
        // empty-username row is the canonical one, so it is the one planted
        // here.
        let mut seeded = config_report_inputs();
        seeded.api_endpoint_registry = Some(api_base_url_reading(
            &inputs(Some("redis://:s3cretpw@127.0.0.1:6379/0"), None, None, true),
            fixed_stamp(),
        ));
        let seeded_render = build_report(&seeded).render();
        assert!(
            !seeded_render.contains("s3cretpw"),
            "an empty-username credential planted in a LAYER row reached the full render:\n\
             {seeded_render}"
        );
        // …and the row is still THERE, saying it withheld. A vanished row would
        // satisfy the assertion above without withholding anything.
        let row_value = match &build_report(&seeded)
            .row("api_endpoint_registry")
            .expect("layer 5 has a row")
            .reading
        {
            LayerReading::Known { value, .. } => value.clone(),
            other => panic!("layer 5 must stay Known when its value is withheld, got {other:?}"),
        };
        assert!(
            row_value.starts_with("<withheld #"),
            "the row must render the withholding, not disappear: {row_value}"
        );
    }

    /// **F2 regression.** A `Debug`-rendered launch field is classified as
    /// PROSE, because the plain classifier is INERT on that shape.
    ///
    /// `RestateEnvHints` holds two RAW environment strings
    /// (`QONTINUI_RESTATE_EXTERNAL_ADMIN_URL`, `…_INGRESS_URL`), and
    /// [`launch_fields`] renders the whole struct through `Debug`. The `Debug`
    /// wrapper puts `")` and `,` inside `url_userinfo`'s host span, so the host
    /// charset rejects the candidate; and the field LABEL is `restate`, which
    /// carries no credential token and does not end `_URL`, so neither the name
    /// arm nor the joint arm can fire either. `api_url` was protected only by
    /// the accident of its own label ending `_URL`.
    ///
    /// The three assertions are the three cells of the executed table, and the
    /// middle one is what stops this test being vacuous: the same URL, bare, IS
    /// a credential to the very classifier that returns `None` on the `Debug`
    /// form — so the miss is about the SHAPE, not about the fixture.
    #[test]
    fn config_report_debug_rendered_launch_fields_are_classified_as_prose() {
        use qontinui_runner_lib::env_generations::{classify_env_var, WithholdReason};

        let bare_admin = "http://admin:hunter2@restate.internal:9070";
        let bare_ingress = "https://svc:s3cretpw@restate.internal:8080";
        let e = RunnerLaunchEnv {
            kind: qontinui_types::wire::runner_kind::RunnerKind::Named {
                name: "temp-abc".to_string(),
            },
            restate: crate::launch_env::RestateEnvHints {
                external_admin_url: Some(bare_admin.to_string()),
                external_ingress_url: Some(bare_ingress.to_string()),
            },
            ..RunnerLaunchEnv::default()
        };

        let fields = launch_fields(&e);
        let (_, restate_debug) = fields
            .iter()
            .find(|(n, _)| *n == "restate")
            .expect("launch_fields renders a `restate` row");

        // PRECONDITION — the fixture's own secrets must really be in the
        // rendered field, or every assertion below is vacuous.
        assert!(
            restate_debug.contains("hunter2") && restate_debug.contains("s3cretpw"),
            "got {restate_debug}"
        );

        // (1) The CONTROL that makes this a defect and not a preference: the
        //     plain classifier — the call that used to be here — returns None.
        assert_eq!(
            classify_env_var("restate", restate_debug),
            None,
            "premise broken: the whole-value classifier now sees the Debug shape — re-derive \
             this control rather than deleting it"
        );

        // (2) …while the same URL, BARE, is a credential to that same
        //     classifier. So the value is one; only the wrapper hid it.
        assert_eq!(
            classify_env_var("restate", bare_admin),
            Some(WithholdReason::ValueUrlPassword),
        );
        assert_eq!(
            classify_env_var("restate", bare_ingress),
            Some(WithholdReason::ValueUrlPassword),
        );

        // (3) The classifier the code now uses withholds the whole field.
        let fp = EnvFingerprinter::new();
        let classified = classify_launch_field(&fp, "restate", restate_debug);
        assert_eq!(
            classified,
            EnvValue::Withheld {
                reason: WithholdReason::ValueUrlPassword,
                fingerprint: fp.fingerprint(restate_debug),
            },
            "the Debug-wrapped restate field must be withheld"
        );
        assert!(!classified.detail().contains("hunter2"));
        assert!(!classified.detail().contains("s3cretpw"));
        assert!(!classified.cell().contains("hunter2"));

        // NO field of this snapshot renders either secret, whichever row it
        // landed in — the classifier is applied to the whole vector, so a field
        // added later is covered without anyone remembering.
        for (field, text) in &fields {
            let v = classify_launch_field(&fp, field, text);
            assert!(
                !v.detail().contains("hunter2") && !v.detail().contains("s3cretpw"),
                "field {field} leaked a planted secret: {}",
                v.detail()
            );
        }

        // …and an ORDINARY launch field stays readable. A classifier that
        // withheld everything would pass every assertion above and destroy the
        // report.
        let plain = RunnerLaunchEnv {
            api_url: Some("http://127.0.0.1:8000".to_string()),
            port: Some(9876),
            ..RunnerLaunchEnv::default()
        };
        for (field, text) in launch_fields(&plain) {
            let v = classify_launch_field(&fp, field, &text);
            assert_eq!(
                v.detail(),
                text,
                "ordinary launch field {field} must stay printable"
            );
        }
    }

    /// **F2 regression, second site.** The `.mcp.json` row inserts two pieces
    /// of text it did not author — the RAW `QONTINUI_INSTANCE_NAME` and an OS /
    /// `serde` read error — and both are classified before they reach the row.
    ///
    /// Both are bounded at their source, which is exactly why the classify pass
    /// is a SECOND, independent control: `shape_from_read` emits
    /// `JSON <category> error at line L column C` and never the offending
    /// value, and the file it is reading is the one holding the bearer and the
    /// proxy nonce this row promises are structurally unreportable. The
    /// bounding is the control that can be edited away by someone restoring
    /// `e.to_string()`; the classifier is the one that cannot.
    #[test]
    fn config_report_mcp_json_row_classifies_the_text_it_did_not_author() {
        use crate::coord_mcp::{McpJsonReport, McpJsonShape};

        let report = |instance: Option<&str>, err: Option<&str>| McpJsonReport {
            root: Some("D:/qontinui-root".to_string()),
            path: Some("D:/qontinui-root/.mcp.json".to_string()),
            exists: true,
            instance_name: instance.map(str::to_string),
            owns_shared_root_state: false,
            this_runner_port: Some(9876),
            proxy_port: None,
            shape: McpJsonShape::Unparseable,
            read_error: err.map(str::to_string),
            safe_to_write: false,
        };

        // CONTROL: an ordinary instance name still renders QUOTED and verbatim.
        let LayerReading::Known { value, .. } =
            mcp_json_reading(&report(Some("temp-abc"), None), fixed_stamp())
        else {
            panic!("layer 14 is Known when a root resolves");
        };
        assert!(
            value.contains("SECONDARY \"temp-abc\" — does NOT own shared root state"),
            "got {value}"
        );

        // A credential-shaped instance name is withheld rather than printed.
        //
        // The fixture is a full-length PAT shape, so it is assembled from two
        // halves for the same reason `env_generations`'s `PREFIX_SAMPLES` are:
        // GitHub push protection scans SOURCE text, not runtime values, and a
        // 40-character `ghp_` literal written whole is what it looks for. The
        // concatenation is byte-identical, and binding it ONCE means the
        // negative assertion below still checks the exact string that went in
        // — the two can no longer drift apart.
        let (pat_prefix, pat_suffix) = ("ghp_", "16C7e42F292c6912E7710c838347Ae178B4a");
        let pat = format!("{pat_prefix}{pat_suffix}");
        let LayerReading::Known { value, .. } =
            mcp_json_reading(&report(Some(pat.as_str()), None), fixed_stamp())
        else {
            panic!("layer 14 is Known when a root resolves");
        };
        assert!(
            !value.contains(pat.as_str()),
            "the instance name reached the row verbatim: {value}"
        );
        assert!(
            value.contains("<withheld #"),
            "the row must SAY it withheld, not drop the clause: {value}"
        );

        // A read error carrying a secret — the `serde` shape the bounding at
        // the source exists to prevent — is withheld too.
        let leaky = "invalid type: string \"eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9\", expected u16";
        let LayerReading::Known { value, .. } =
            mcp_json_reading(&report(None, Some(leaky)), fixed_stamp())
        else {
            panic!("layer 14 is Known when a root resolves");
        };
        assert!(
            !value.contains("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"),
            "the read error leaked a token into the row: {value}"
        );
        assert!(
            value.contains("could not be read or parsed (<withheld #"),
            "the clause must still be there, saying it withheld: {value}"
        );

        // CONTROL: the BOUNDED message the source actually emits stays fully
        // readable — it is the whole diagnostic value of the row.
        let bounded = "JSON Data error at line 4 column 12";
        let LayerReading::Known { value, .. } =
            mcp_json_reading(&report(None, Some(bounded)), fixed_stamp())
        else {
            panic!("layer 14 is Known when a root resolves");
        };
        assert!(
            value.contains("could not be read or parsed (JSON Data error at line 4 column 12)"),
            "an ordinary read error must stay printable: {value}"
        );
    }
}
