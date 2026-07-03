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
//!     "env_contract": { "<VAR_NAME>": "present" }
//!   }
//! }
//! ```
//!
//! Every section value MUST be a string. A failing/empty section is OMITTED.
//!
//! ## Fail-open posture (mirrors `fleet.rs`)
//!
//! Network pushes use an exponential-backoff retry ([2,4,8,16,32,60]s, 10s
//! timeout) cloned from `fleet::post_budget_with_retry`. A terminal failure
//! returns `Err`; the periodic loop logs+swallows so the runner never blocks.
//! The last envelope is cached to `~/.qontinui/last_env_capture.json` BEFORE the
//! POST so an operator can inspect "what would be pushed" even when the backend
//! is unreachable.

pub mod collectors;
pub mod config;
pub mod enroll;

use std::sync::OnceLock;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Value};
use tracing::{debug, info, warn};

use self::collectors::Section;
use self::config::EnvAgentConfig;

/// Current envelope schema version. Must match the backend's expectation.
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
    add_section(
        &mut sections,
        "versions",
        Some(collectors::collect_versions()),
    );
    add_section(
        &mut sections,
        "env_contract",
        Some(collectors::collect_env_contract()),
    );

    ConfigEnvelope {
        schema_version: SCHEMA_VERSION,
        captured_at: chrono::Utc::now().to_rfc3339(),
        sections,
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

/// Spawn the periodic env-capture task on the ambient tokio runtime.
///
/// Interval from `QONTINUI_ENV_CAPTURE_INTERVAL_SECS` (default 900s, floored at
/// 60s). `MissedTickBehavior::Skip` mirrors `fleet::spawn_heartbeat` — a missed
/// tick (system suspend) doesn't blast catch-up. The enrollment gate is checked
/// INSIDE the loop so a mid-session enroll is picked up without a runner
/// restart. Runs once shortly after start, then on the interval. Failures
/// `warn!` and retry on the next tick; the loop never panics.
pub fn spawn_env_capture() {
    let secs: u64 = std::env::var("QONTINUI_ENV_CAPTURE_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(900)
        .max(60);

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

    /// Secret-safety (end-to-end envelope): a secret-bearing env var and a
    /// password-bearing DSN must never appear in the serialized envelope; the
    /// var NAME must appear with value "present".
    #[test]
    fn secret_safety_envelope_never_leaks_secrets() {
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
        };
        let json = serde_json::to_string(&envelope).expect("serialize envelope");

        std::env::remove_var("QONTINUI_SECRET_TOKEN_ENVELOPE_TEST");
        std::env::remove_var("DATABASE_URL");

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
        };
        let v = serde_json::to_value(&envelope).unwrap();
        assert_eq!(v.get("schema_version").and_then(|x| x.as_u64()), Some(1));
        assert!(v.get("sections").and_then(|s| s.get("versions")).is_some());
        // Every section value is a string.
        let versions = v
            .get("sections")
            .and_then(|s| s.get("versions"))
            .and_then(|x| x.as_object())
            .unwrap();
        assert!(versions.values().all(|val| val.is_string()));
    }

    #[test]
    fn add_section_omits_empty_and_none() {
        let mut sections = Map::new();
        add_section(&mut sections, "empty", Some(Section::new()));
        add_section(&mut sections, "none", None);
        assert!(sections.is_empty(), "empty/None sections must be omitted");
    }
}
