//! Machine-side dev-environment capture agent.
//!
//! Runs on a developer's machine, captures that machine's real dev-environment
//! configuration (SECRET-FREE), and POSTs it to the qontinui-web backend so the
//! server computes drift vs a canonical machine. Auth is a per-machine API key
//! (`X-Machine-Key: mk_<token>`), NOT a user JWT.
//!
//! ## Lifecycle
//!
//! 1. `qontinui_profile env enroll --code <enrollment_code>` →
//!    `POST {backend}/api/v1/devenv/agent/enroll`. The server returns
//!    `{ machine_id, machine_key: "mk_…", environment_id }` ONCE. We store the
//!    key in `SecureStorage` and the rest in `~/.qontinui/env-agent.json`.
//! 2. The runner's background task ([`spawn_env_capture`]) — or the one-shot
//!    `qontinui_profile env capture` — runs the four collectors, builds the
//!    envelope, writes a last-envelope cache, and PUTs the envelope to
//!    `{backend}/api/v1/devenv/agent/environments/{environment_id}/config`.
//!
//! ## Envelope contract (conforms EXACTLY to the backend)
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "captured_at": "<rfc3339>",
//!   "sections": {
//!     "services":     { "<key>": "<string>" },
//!     "db_schema":    { "<key>": "<string>" },
//!     "versions":     { "<key>": "<string>" },
//!     "env_contract": { "<VAR_NAME>": "present" },
//!     "claude_accounts": { "<key>": "<string>" },
//!     "repos":        { "repo_<owner>_<name>": "<canonical clone url>" }
//!   },
//!   "unknown_keys": { "versions": ["rustc"] }
//! }
//! ```
//!
//! Every section value MUST be a string. A failing/empty section is OMITTED.
//!
//! `unknown_keys` is an ADDITIVE sibling of `sections`: section name → the keys
//! this capture could not MEASURE (a probe that blew its budget), as distinct
//! from keys this box genuinely lacks. It is never a change to a section's value
//! shape — see [`ConfigEnvelope::unknown_keys`].
//!
//! ## Fail-open posture (mirrors `fleet.rs`)
//!
//! Network pushes use an exponential-backoff retry ([2,4,8,16,32,60]s, 10s
//! timeout) cloned from `fleet::post_budget_with_retry`. A terminal failure
//! returns `Err`; the periodic loop logs+swallows so the runner never blocks.
//! The last envelope is cached to `~/.qontinui/last_env_capture.json` BEFORE the
//! POST so an operator can inspect "what would be pushed" even when the backend
//! is unreachable.

pub mod apply;
pub mod apply_repos;
pub mod apply_services;
pub mod apply_versions;
pub mod collectors;
pub mod config;
pub mod directive;
pub mod enroll;
pub mod pull;

use std::sync::OnceLock;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Value};
use tracing::{debug, info, warn};

use self::collectors::Section;
use self::config::EnvAgentConfig;

/// Current envelope schema version. Must match the backend's expectation.
///
/// **P4 deliberately does NOT bump this**, and the reason is stronger than
/// "removing a key is backward-compatible". qontinui-web's drift service
/// (`app/services/devenv_drift.py`) treats ANY difference between the two
/// envelopes' `schema_version` as `schema_version_mismatch`, which forces the
/// whole report to `critical` and clears `in_sync` regardless of the per-key
/// deltas. A fleet upgrades one box at a time, so a bump would mark every
/// machine that has not upgraded yet — and every machine that has, against a
/// canonical that has not — critically drifted on every section, for the whole
/// rollout window. That is a fleet-wide false alarm in exchange for a signal
/// nothing consumes: the backend stores and echoes this number but validates no
/// required field set against it (`sections` is a free-form
/// `dict[str, dict[str, str]]`, `app/schemas/devenv.py`).
///
/// What retiring `database_url` does cost is one transient per-key delta: a
/// canonical capture taken before P4 still carries `services.database_url`, so
/// a post-P4 box reads `removed` there until the canonical box re-captures.
/// That is the rollout step §8 of the plan already prescribes, and it clears
/// itself — unlike a version mismatch, which nothing but a fleet-wide upgrade
/// can clear. The key is NOT named in `unknown_keys` to soften it: `unknown`
/// means "this box could not measure it", and a retired key was not unmeasured.
const SCHEMA_VERSION: u32 = 1;

/// Wire shape of the capture envelope (`PUT .../config` body).
#[derive(Debug, Serialize)]
pub struct ConfigEnvelope {
    /// Schema version — always [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// RFC3339 capture timestamp.
    pub captured_at: String,
    /// Section name → section map (each value a string). Failing/empty sections
    /// are omitted by the isolation driver.
    pub sections: Map<String, Value>,
    /// Section name → the keys in that section this capture could not MEASURE,
    /// as opposed to keys this box genuinely lacks. Today only `versions`
    /// contributes (a toolchain probe that blew the capture probe budget — see
    /// [`collectors::VersionsCapture`]).
    ///
    /// An ADDITIVE sibling of `sections`, never a change to a section's value
    /// shape: every section value must stay a `Value::String`, and the pull's
    /// diff silently drops non-string values, so a `{value, status}` value would
    /// make the key disappear from the diff altogether.
    ///
    /// Always emitted, including as `{}` — and that emptiness is now load-bearing
    /// rather than aspirational.
    ///
    /// The web-side half has LANDED (qontinui-web PR #992, plan
    /// `2026-08-14-devenv-capture-probe-budget-makes-actionable-nondeterministic`
    /// Phase 4, commit `2d11fdda`). `ConfigEnvelope.unknown_keys` in `qontinui-web`
    /// `backend/app/schemas/devenv.py` is `dict[str, list[str]] | None`, and
    /// `to_stored_config()` persists it as a sibling of `sections` whenever it is
    /// not `None` — so what this runner PUTs is stored, and the drift view reads
    /// an unmeasured key as `DeltaStatusT::unknown` instead of `removed`.
    ///
    /// That is why `{}` is emitted rather than skipped: the backend deliberately
    /// keeps `None` (field never arrived — an older runner, nothing can be
    /// concluded) distinct from `{}` (an explicit "every probe completed"), and a
    /// skipped-when-empty field would collapse the two and turn "we were never
    /// told" into a positive claim that everything was measured.
    ///
    /// **Still one-way.** `CanonicalConfigResponse` (same file) does NOT serve
    /// the field back, so [`pull::CanonicalConfig`] has no counterpart and a
    /// pulling box cannot tell a key CANONICAL failed to measure from one
    /// canonical genuinely lacks. If the pulling box measured the key it diffs
    /// as [`pull::Change::Extra`]; if it lacks it too there is no row at all.
    /// Either way it is never an apply action — which is what keeps this a
    /// reporting gap rather than a mutation hazard. Closing it needs the serving
    /// half first; adding a reader for a field nothing serves would just be dead
    /// code.
    pub unknown_keys: Map<String, Value>,
}

// ============================================================================
// PG pool bridge (lib ↔ binary)
// ============================================================================
//
// The live deadpool PG pool is owned by the binary crate's
// `crate::database::pg::GLOBAL_PG_DB` OnceLock, which the lib crate cannot
// reach. The binary publishes a clone of the pool here at boot
// (`publish_pg_pool`) so the lib-side `db_schema` collector can use it. Until
// published, `collect_db_schema` returns `None` (section omitted).

static PG_POOL: OnceLock<deadpool_postgres::Pool> = OnceLock::new();

/// Publish the live PG pool for the `db_schema` collector. Called once by the
/// binary at boot (`main.rs` fleet-publishers block). Idempotent — a second
/// call is ignored.
pub fn publish_pg_pool(pool: deadpool_postgres::Pool) {
    let _ = PG_POOL.set(pool);
}

/// Build a LAZY deadpool PG pool from a connection string and publish it for
/// the `db_schema` collector.
///
/// The full runner hands `publish_pg_pool` a clone of its already-built boot
/// pool (`main.rs`), but the standalone `qontinui_profile env capture` CLI has
/// no such pool — without this it can never populate the high-value `db_schema`
/// section (alembic_head + schema/table census), so cross-machine schema drift
/// is invisible from the CLI. This builds a small pool (mirroring
/// `database::pg::build_pool`, minus the search_path hook — `alembic_version`
/// lives in `public`, already covered by the default search_path — and minus
/// the timeout setters, which would panic without an active tokio runtime).
///
/// Lazy by design: the pool is built but NOT connected here, so it is safe to
/// call from a synchronous CLI context with no runtime entered. The collector
/// connects on first `get().await` inside the capture runtime; any connect
/// failure there simply omits the section. Idempotent: a second call no-ops
/// because the underlying `OnceLock` is already set.
pub fn publish_pg_pool_from_url(database_url: &str) -> Result<(), String> {
    let pg_config: tokio_postgres::Config = database_url
        .parse()
        .map_err(|e| format!("invalid PG connection string: {e}"))?;
    let mgr_config = deadpool_postgres::ManagerConfig {
        recycling_method: deadpool_postgres::RecyclingMethod::Fast,
    };
    let mgr = deadpool_postgres::Manager::from_config(pg_config, tokio_postgres::NoTls, mgr_config);
    let pool = deadpool_postgres::Pool::builder(mgr)
        .max_size(2)
        .build()
        .map_err(|e| format!("failed to build PG pool: {e}"))?;
    publish_pg_pool(pool);
    Ok(())
}

/// Publish a lazy pool pointed at THIS machine's bundled PostgreSQL.
///
/// The `db_schema` collector needs a pool to census `alembic_head` and the
/// schema/table list. Before P4 the CLI got that DSN from the active profile's
/// `database_url`; there is no such key any more, and the schema it wants to
/// census is the bundled cluster's.
///
/// Returns `Err` when the cluster is not running — the CLI can be invoked with
/// no runner up. Every caller treats that as "omit the `db_schema` section",
/// which is the honest outcome: the alternative, guessing `localhost:5432`, is
/// what let a box census an unrelated project's database and report no drift.
pub fn publish_pg_pool_from_local_cluster() -> Result<(), String> {
    let dsn = crate::embedded_pg::local_dsn("qontinui_db")?;
    publish_pg_pool_from_url(&dsn)
}

/// Get a clone of the published PG pool, if any. `deadpool_postgres::Pool` is an
/// `Arc` internally, so cloning is cheap and shares the live pool.
pub(crate) fn pg_pool() -> Option<deadpool_postgres::Pool> {
    PG_POOL.get().cloned()
}

// ============================================================================
// Cache path
// ============================================================================

/// Path of the last-envelope cache (`~/.qontinui/last_env_capture.json`).
fn last_capture_cache_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("last_env_capture.json"))
}

/// Atomically write the last-envelope cache (tmp + rename). Best-effort: a
/// failure is logged at debug and swallowed. Mirrors
/// `fleet::write_last_budget_cache`.
fn write_last_capture_cache(envelope: &ConfigEnvelope, environment_id: &str) {
    let Some(path) = last_capture_cache_path() else {
        return;
    };
    let payload = serde_json::json!({
        "environment_id": environment_id,
        "envelope": envelope,
    });
    let pretty = match serde_json::to_vec_pretty(&payload) {
        Ok(b) => b,
        Err(e) => {
            debug!("env_agent: cache serialize failed: {e}");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            debug!("env_agent: cache mkdir failed: {e}");
            return;
        }
    }
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, &pretty) {
        debug!("env_agent: cache write failed: {e}");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        debug!("env_agent: cache rename failed: {e}");
    }
}

// ============================================================================
// Envelope assembly
// ============================================================================

/// Insert a section under `name` IFF it is `Some` and non-empty. The isolation
/// driver: a collector that fails or yields nothing contributes no key, so the
/// envelope never carries a broken/empty section.
fn add_section(sections: &mut Map<String, Value>, name: &str, section: Option<Section>) {
    if let Some(s) = section {
        if !s.is_empty() {
            sections.insert(name.to_string(), Value::Object(s));
        }
    }
}

/// Run all four collectors and assemble the envelope. Each collector is
/// best-effort; a failing/empty one is omitted (never aborts the others).
pub async fn build_envelope() -> ConfigEnvelope {
    let mut sections = Map::new();
    let mut unknown_keys = Map::new();

    add_section(
        &mut sections,
        "services",
        collectors::collect_services().await,
    );
    add_section(
        &mut sections,
        "db_schema",
        collectors::collect_db_schema().await,
    );
    // Synchronous collectors — wrap in Some so the isolation driver still drops
    // an (unlikely) empty result.
    //
    // `collect_versions` is BLOCKING (it shells out to three tools and sleeps in
    // its own poll loop), and its pinned worst case is 39s — see
    // `collectors::CAPTURE_PROBE_RETRY_BUDGET`. This runs on the runner's shared
    // multi-threaded runtime, which also serves the UI Bridge and the coord
    // door, so calling it inline parked one of those workers for the duration.
    // `spawn_blocking` puts it on the blocking pool where a long synchronous
    // call belongs.
    let versions = tokio::task::spawn_blocking(collectors::collect_versions)
        .await
        .unwrap_or_else(|e| {
            // A panicked/cancelled collector degrades to "we have no local data
            // for this section", which the pull reports as un-comparable — never
            // as a section full of missing keys.
            tracing::warn!("env_agent: versions collector did not complete: {e}");
            collectors::VersionsCapture::empty()
        });
    add_section(
        &mut sections,
        apply_versions::VERSIONS_SECTION,
        Some(versions.section),
    );
    // Keyed by SECTION so the field generalizes; gated on the section having
    // actually survived the isolation driver, so the envelope can never carry an
    // unknown-key list for a section that isn't there.
    if sections.contains_key(apply_versions::VERSIONS_SECTION) && !versions.unknown_keys.is_empty()
    {
        unknown_keys.insert(
            apply_versions::VERSIONS_SECTION.to_string(),
            Value::Array(
                versions
                    .unknown_keys
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    add_section(
        &mut sections,
        "env_contract",
        Some(collectors::collect_env_contract()),
    );
    // Claude account roster — SECRET-FREE (names/topology/presence only). Returns
    // None (section omitted) when no roster or config dir is present.
    add_section(
        &mut sections,
        "claude_accounts",
        collectors::collect_claude_accounts(),
    );
    // Which repositories this box has cloned. Returns None (section omitted)
    // only when the workspace root does not resolve — a resolved root under
    // which nothing matched still returns Some, carrying its provenance key, so
    // "this box has none of them" stays a stated observation rather than a
    // dropped section.
    add_section(&mut sections, "repos", collectors::collect_repos());

    ConfigEnvelope {
        schema_version: SCHEMA_VERSION,
        captured_at: chrono::Utc::now().to_rfc3339(),
        sections,
        unknown_keys,
    }
}

// ============================================================================
// HTTP push (cloned from fleet::post_budget_with_retry)
// ============================================================================

/// Render an error with its full `source()` chain (cloned from
/// `fleet::error_chain`) so failure WARNs carry the root cause (dns/tls/os).
fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(cause) = src {
        s.push_str(": ");
        s.push_str(&cause.to_string());
        src = cause.source();
    }
    s
}

/// PUT the envelope with exponential backoff (2s, 4s, 8s, 16s, 32s, 60s) +
/// `X-Machine-Key` auth. Returns Ok on first success; Err with the last error if
/// every attempt fails. Cloned from `fleet::post_budget_with_retry` — the caller
/// logs+swallows so the runner never blocks.
async fn put_envelope_with_retry(
    backend_url: &str,
    environment_id: &str,
    machine_key: &str,
    envelope: &ConfigEnvelope,
) -> Result<(), String> {
    let url = format!(
        "{}/api/v1/devenv/agent/environments/{}/config",
        backend_url.trim_end_matches('/'),
        environment_id
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("reqwest client build: {e}"))?;
    let mut last_err = String::new();
    let backoff_ms: [u64; 6] = [2_000, 4_000, 8_000, 16_000, 32_000, 60_000];
    for (attempt, delay_ms) in backoff_ms.iter().enumerate() {
        match client
            .put(&url)
            .header("X-Machine-Key", machine_key)
            .json(envelope)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(());
                }
                let body_text = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "<unable to read response body>".to_string());
                last_err = format!("PUT {url} -> HTTP {status}: {body_text}");
            }
            Err(e) => {
                last_err = format!("PUT {url} failed: {}", error_chain(&e));
            }
        }
        if attempt + 1 < backoff_ms.len() {
            warn!(
                "env_agent::push: attempt {} failed ({}); retrying in {}s",
                attempt + 1,
                last_err,
                delay_ms / 1000
            );
            tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
        }
    }
    Err(last_err)
}

// ============================================================================
// Capture-and-push driver
// ============================================================================

/// Load enrollment state, build the envelope, cache it, and PUT it to the
/// backend. No-op (returns `Ok(())`) when the agent is not enrolled, so the
/// periodic loop is safe to call unconditionally.
///
/// The envelope is cached to `~/.qontinui/last_env_capture.json` BEFORE the POST
/// — the operator's lifeline when the backend is unreachable.
pub async fn capture_and_push() -> Result<(), String> {
    let cfg = match EnvAgentConfig::load() {
        Some(c) if c.is_enrolled() => c,
        _ => {
            debug!("env_agent::capture_and_push: not enrolled — skipping");
            return Ok(());
        }
    };

    // The machine key is the auth credential — without it we cannot push.
    let machine_key = match crate::secure_storage::SecureStorage::new()
        .ok()
        .and_then(|s| s.get_agent_machine_key().ok().flatten())
    {
        Some(k) if !k.is_empty() => k,
        _ => {
            warn!(
                "env_agent::capture_and_push: enrolled but no machine key in secure storage \
                 — re-run `qontinui_profile env enroll --code <code>`"
            );
            return Ok(());
        }
    };

    let envelope = build_envelope().await;

    // Cache BEFORE the POST.
    write_last_capture_cache(&envelope, &cfg.environment_id);

    match put_envelope_with_retry(
        &cfg.backend_url,
        &cfg.environment_id,
        &machine_key,
        &envelope,
    )
    .await
    {
        Ok(()) => {
            info!(
                "env_agent::capture_and_push: pushed {} section(s) to environment {} ({})",
                envelope.sections.len(),
                cfg.environment_id,
                cfg.backend_url
            );
            Ok(())
        }
        Err(e) => Err(e),
    }
}

// ============================================================================
// Sync helper for the CLI one-shot (`qontinui_profile env capture`)
// ============================================================================

/// Build a current-thread tokio runtime and run [`capture_and_push`]. Used by
/// the `qontinui_profile env capture` CLI (a sync binary). Errors are returned
/// so the CLI can print them + set a nonzero exit.
pub fn capture_and_push_blocking() -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime build failed: {e}"))?;
    rt.block_on(capture_and_push())
}

/// Build the envelope synchronously (current-thread runtime) WITHOUT pushing.
/// Used by `qontinui_profile env capture --dry-run` to pretty-print the
/// envelope the agent would send.
pub fn build_envelope_blocking() -> Result<ConfigEnvelope, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime build failed: {e}"))?;
    Ok(rt.block_on(build_envelope()))
}

// ============================================================================
// Periodic capture task (clone of fleet::spawn_heartbeat)
// ============================================================================

/// The capture loop's interval, in seconds: `QONTINUI_ENV_CAPTURE_INTERVAL_SECS`
/// if it parses, else 900, and never below 60.
///
/// **The single reader of that variable**, so nothing downstream can describe
/// the loop with a hardcoded number. It used to be read inline in
/// [`spawn_env_capture`] while `collectors` documented — and one WARN
/// arithmetically claimed — a fixed 15 minutes; at the 60s floor that warn told
/// an operator "~60 minutes" about three minutes of elapsed time.
pub fn capture_interval_secs() -> u64 {
    std::env::var("QONTINUI_ENV_CAPTURE_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(900)
        .max(60)
}

/// Spawn the periodic env-capture task on the ambient tokio runtime.
///
/// Interval from `QONTINUI_ENV_CAPTURE_INTERVAL_SECS` (default 900s, floored at
/// 60s). `MissedTickBehavior::Skip` mirrors `fleet::spawn_heartbeat` — a missed
/// tick (system suspend) doesn't blast catch-up. The enrollment gate is checked
/// INSIDE the loop so a mid-session enroll is picked up without a runner
/// restart. Runs once shortly after start, then on the interval. Failures
/// `warn!` and retry on the next tick; the loop never panics.
pub fn spawn_env_capture() {
    let secs: u64 = capture_interval_secs();

    info!(
        "env_agent::capture: starting periodic capture task, interval={}s",
        secs
    );

    tokio::spawn(async move {
        // Small initial delay so the first capture lands off the boot
        // critical path (the binary publishes the PG pool around the same
        // time this task spawns).
        tokio::time::sleep(Duration::from_secs(5)).await;

        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut consecutive_failures: u32 = 0;
        loop {
            tick.tick().await;

            // Gate INSIDE the loop so mid-session enroll works. An unenrolled
            // machine simply skips this tick (no log spam — debug only).
            let enrolled = EnvAgentConfig::load()
                .map(|c| c.is_enrolled())
                .unwrap_or(false);
            if !enrolled {
                debug!("env_agent::capture: not enrolled — skipping tick");
                continue;
            }

            match capture_and_push().await {
                Err(e) => {
                    consecutive_failures += 1;
                    warn!("env_agent::capture: {e} (consecutive_failures={consecutive_failures})");
                }
                Ok(()) if consecutive_failures > 0 => {
                    info!(
                        "env_agent::capture: recovered after {consecutive_failures} failed tick(s)"
                    );
                    consecutive_failures = 0;
                }
                Ok(()) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_env::{env_lock, EnvVarRestore};

    /// Secret-safety (end-to-end envelope): a secret-bearing env var and a
    /// password-bearing DSN must never appear in the serialized envelope; the
    /// var NAME must appear with value "present".
    #[test]
    fn secret_safety_envelope_never_leaks_secrets() {
        let _env_lock = env_lock();
        // Restore both vars on the way out (incl. panic): DATABASE_URL is set in
        // dev / DB-gated CI, and blindly removing it would leak a wrong-DB /
        // missing-DSN state to sibling tests in this binary.
        let _restore =
            EnvVarRestore::capture(&["QONTINUI_SECRET_TOKEN_ENVELOPE_TEST", "DATABASE_URL"]);
        // Arrange a secret-bearing allowlisted env var + a password DSN.
        std::env::set_var("QONTINUI_SECRET_TOKEN_ENVELOPE_TEST", "supersecret123");
        std::env::set_var("DATABASE_URL", "postgres://u:pw@h:5432/db");

        // Build env_contract + a sanitized services-style entry directly (we
        // don't depend on the live profile/PG here — this isolates the
        // secret-safety assertion to the collectors' structural guarantees).
        let env_contract = collectors::collect_env_contract();
        let mut sections = Map::new();
        add_section(&mut sections, "env_contract", Some(env_contract));

        let envelope = ConfigEnvelope {
            schema_version: SCHEMA_VERSION,
            captured_at: chrono::Utc::now().to_rfc3339(),
            sections,
            unknown_keys: Map::new(),
        };
        let json = serde_json::to_string(&envelope).expect("serialize envelope");

        // (env vars restored by `_restore` on scope exit.)

        // The secret VALUE must appear nowhere.
        assert!(
            !json.contains("supersecret123"),
            "secret env value leaked into envelope: {json}"
        );
        // The DSN password must appear nowhere.
        assert!(
            !json.contains("\"pw\"") && !json.contains(":pw@") && !json.contains("u:pw"),
            "DSN password leaked into envelope: {json}"
        );
        // The var NAME must be present with value "present".
        assert!(
            json.contains("QONTINUI_SECRET_TOKEN_ENVELOPE_TEST") && json.contains("present"),
            "expected var name + 'present' marker in envelope: {json}"
        );
    }

    #[test]
    fn envelope_serializes_with_schema_version_and_sections() {
        let mut sections = Map::new();
        let mut s = Section::new();
        s.insert("k".to_string(), Value::String("v".to_string()));
        add_section(&mut sections, "versions", Some(s));
        let envelope = ConfigEnvelope {
            schema_version: SCHEMA_VERSION,
            captured_at: "2026-06-22T00:00:00+00:00".to_string(),
            sections,
            unknown_keys: Map::new(),
        };
        let v = serde_json::to_value(&envelope).unwrap();
        assert_eq!(v.get("schema_version").and_then(|x| x.as_u64()), Some(1));
        assert!(v.get("sections").and_then(|s| s.get("versions")).is_some());
        // Additive sibling of `sections` — present even when empty, so a reader
        // can tell "measured everything" from "runner predates the field".
        assert_eq!(v.get("unknown_keys"), Some(&Value::Object(Map::new())));
        // Every section value is a string.
        let versions = v
            .get("sections")
            .and_then(|s| s.get("versions"))
            .and_then(|x| x.as_object())
            .unwrap();
        assert!(versions.values().all(|val| val.is_string()));
    }

    /// The wire shape a later web-side phase reads: section name → key list,
    /// beside `sections` and never inside it. The section's own values stay
    /// strings, and the unmeasured key stays ABSENT from the section — the
    /// whole point of a parallel set rather than a value-shape change.
    #[test]
    fn envelope_carries_unknown_keys_beside_sections_not_inside_them() {
        let mut sections = Map::new();
        let mut s = Section::new();
        s.insert("node".to_string(), Value::String("v22.1.0".to_string()));
        add_section(&mut sections, "versions", Some(s));
        let mut unknown_keys = Map::new();
        unknown_keys.insert(
            "versions".to_string(),
            Value::Array(vec![Value::String("rustc".to_string())]),
        );
        let envelope = ConfigEnvelope {
            schema_version: SCHEMA_VERSION,
            captured_at: "2026-08-14T00:00:00+00:00".to_string(),
            sections,
            unknown_keys,
        };
        let v = serde_json::to_value(&envelope).unwrap();
        assert_eq!(v["unknown_keys"]["versions"][0], "rustc");
        // The section is untouched: still string-only, still without the key.
        let versions = v["sections"]["versions"].as_object().unwrap();
        assert!(versions.values().all(Value::is_string));
        assert!(!versions.contains_key("rustc"));
    }

    #[test]
    fn add_section_omits_empty_and_none() {
        let mut sections = Map::new();
        add_section(&mut sections, "empty", Some(Section::new()));
        add_section(&mut sections, "none", None);
        assert!(sections.is_empty(), "empty/None sections must be omitted");
    }
}
