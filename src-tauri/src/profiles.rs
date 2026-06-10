//! Connection profile loader for the canonical-DB topology.
//!
//! Per topology plan §3 (`tmp_canonical_db_topology_plan.md`), the runner
//! reads its DB / Redis / blob / coord-service connection settings from
//! `~/.qontinui/profiles.json`. The active profile is selected by:
//!
//!   1. `QONTINUI_ENV` env var (highest priority).
//!   2. The file's top-level `"active"` field.
//!   3. `"dev"` if neither is set.
//!
//! Profiles file layout:
//!
//! ```json
//! {
//!   "active": "dev",
//!   "profiles": {
//!     "dev":     { "database_url": "...", "redis_url": "...", "blob": {...}, "coord_url": "...", "auth": {...} },
//!     "staging": { ... },
//!     "prod":    { ... }
//!   }
//! }
//! ```
//!
//! ## Fallback chain
//!
//! When profiles.json is missing or the chosen profile lacks a setting, the
//! loader falls back to legacy env vars so the runner remains bootable on
//! machines that haven't been migrated yet:
//!
//! | Setting       | Legacy env var          |
//! |---------------|-------------------------|
//! | database_url  | `RUNNER_DATABASE_URL`   |
//! | redis_url     | `REDIS_URL`             |
//! | blob.endpoint | `S3_ENDPOINT`           |
//! | coord_url     | `COORD_URL`             |
//!
//! When even the env-var fallback is unavailable for `database_url`, a
//! hardcoded localhost default is returned (matches `main.rs:279`'s prior
//! behavior). Callers needing strict-mode validation can use
//! [`load_strict`].

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Top-level shape of `~/.qontinui/profiles.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfilesFile {
    /// Default profile name when `QONTINUI_ENV` is unset.
    #[serde(default)]
    pub active: Option<String>,
    /// Named profiles keyed by environment label (`dev`, `staging`, `prod`,
    /// `cloud`, custom).
    #[serde(default)]
    pub profiles: HashMap<String, Profile>,
}

/// One environment's connection settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    /// Postgres DSN.
    #[serde(default)]
    pub database_url: Option<String>,
    /// Redis URL (`redis://host:port/db`).
    #[serde(default)]
    pub redis_url: Option<String>,
    /// S3-compatible blob configuration (MinIO in dev, real S3 in prod).
    #[serde(default)]
    pub blob: Option<BlobConfig>,
    /// Coordinator service URL (WebSocket — `ws://` or `wss://`).
    #[serde(default)]
    pub coord_url: Option<String>,
    /// Auth provider configuration.
    #[serde(default)]
    pub auth: Option<AuthConfig>,
}

/// S3-compatible blob storage settings. `kind` distinguishes MinIO from
/// real S3 — both speak the same wire protocol but signing/region defaults
/// differ.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobConfig {
    pub kind: String,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub access_key: Option<String>,
    #[serde(default)]
    pub secret_key: Option<String>,
    #[serde(default)]
    pub bucket: Option<String>,
}

/// Auth posture. Dev profiles use a static token; staging+ uses OIDC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub kind: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
}

/// Resolved profile after fallback chain — every consumer reads this.
#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    /// Which profile name produced this resolution (`dev`, etc., or
    /// `legacy-env` when no profiles.json existed).
    pub source: String,
    pub database_url: String,
    pub redis_url: Option<String>,
    pub blob: Option<BlobConfig>,
    pub coord_url: Option<String>,
    pub auth: Option<AuthConfig>,
}

/// Path of `~/.qontinui/profiles.json` for the current user.
pub fn profiles_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("profiles.json"))
}

/// Resolve the active profile, applying the fallback chain. Always
/// returns a `ResolvedProfile` — never errors. Callers that need an
/// error-on-missing variant should use [`load_strict`].
pub fn load() -> ResolvedProfile {
    match load_inner() {
        Ok(p) => p,
        Err(e) => {
            warn!(
                "Profile load failed: {}. Falling back to legacy env vars.",
                e
            );
            legacy_env_fallback()
        }
    }
}

/// Strict variant: errors if profiles.json is missing or the active
/// profile lacks a `database_url`. Used by tooling that must not silently
/// connect to localhost.
pub fn load_strict() -> Result<ResolvedProfile> {
    load_inner()
}

fn load_inner() -> Result<ResolvedProfile> {
    let path = profiles_path().ok_or_else(|| anyhow!("Could not resolve home directory"))?;
    if !path.exists() {
        return Err(anyhow!("profiles.json not found at {}", path.display()));
    }

    let bytes = std::fs::read(&path)
        .with_context(|| format!("Reading profiles file at {}", path.display()))?;
    let file: ProfilesFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("Parsing profiles file at {}", path.display()))?;

    let active = std::env::var("QONTINUI_ENV")
        .ok()
        .or_else(|| file.active.clone())
        .unwrap_or_else(|| "dev".to_string());

    let profile = file.profiles.get(&active).cloned().ok_or_else(|| {
        anyhow!(
            "Active profile '{}' not present in {}",
            active,
            path.display()
        )
    })?;

    let database_url = profile
        .database_url
        .clone()
        .or_else(|| std::env::var("RUNNER_DATABASE_URL").ok())
        .ok_or_else(|| {
            anyhow!(
                "Profile '{}' has no database_url and RUNNER_DATABASE_URL is unset",
                active
            )
        })?;

    debug!("Loaded profile '{}' from {}", active, path.display());

    Ok(ResolvedProfile {
        source: active,
        database_url,
        redis_url: profile
            .redis_url
            .or_else(|| std::env::var("REDIS_URL").ok()),
        blob: profile.blob,
        coord_url: profile
            .coord_url
            .or_else(|| std::env::var("COORD_URL").ok()),
        auth: profile.auth,
    })
}

/// Pure-env-var fallback when profiles.json is missing or unparseable.
/// Mirrors the legacy main.rs:279 default so machines that haven't been
/// migrated to the canonical-DB topology continue to work.
fn legacy_env_fallback() -> ResolvedProfile {
    let database_url = std::env::var("RUNNER_DATABASE_URL").unwrap_or_else(|_| {
        "host=localhost port=5432 user=qontinui_user password=qontinui_dev_password dbname=qontinui_db".to_string()
    });

    info!("Using legacy env-var configuration (profiles.json not found)");

    ResolvedProfile {
        source: "legacy-env".to_string(),
        database_url,
        redis_url: std::env::var("REDIS_URL").ok(),
        blob: None,
        coord_url: std::env::var("COORD_URL").ok(),
        auth: None,
    }
}

// ============================================================================
// Shared coord HTTP base resolver
// ============================================================================
//
// Historically ~12 call sites each re-implemented the same chain
// (env `COORD_HTTP_URL` → profile `coord_url` ws→http → `http://localhost:9870`)
// with subtly different trimming and, worse, a SILENT localhost fallback. The
// operator's stance is "log loudly, don't silently write to a phantom coord":
// localhost is the legitimate local-dev coord, so we keep the fallback but emit
// exactly one process-global WARN the first time we guess it.

/// Outcome of resolving the coord HTTP base, before any dev-localhost policy
/// is applied. Lets each caller family pick its own fallback posture:
/// - String family: `coord_base_or_dev_localhost().unwrap_or_else(localhost)`
/// - Option family: `Configured ⇒ Some`, otherwise `None` (no-op when unconfigured)
/// - Result family: map as appropriate, preserving the call site's contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoordBase {
    /// A coord base was explicitly configured (env `COORD_HTTP_URL` or the
    /// active profile's `coord_url`). The string is already ws→http normalized.
    Configured(String),
    /// Nothing was configured; this is the dev-localhost guess
    /// (`http://localhost:9870`). Treated as "configured enough" for the
    /// String family but as "unconfigured" (None) for the Option family.
    DevLocalhost(String),
    /// Nothing configured AND the caller did not want a localhost guess.
    Unset,
}

/// Pure conversion: `ws[s]://host[:port][/ws][/]` → `http[s]://host[:port]`.
///
/// This is the single source of truth that the two historical normalizers
/// (`pair::coord_http_base_from_url` and `agent_worktree::coord_ws_to_http`)
/// now both delegate to. It is the SUPERSET of both old behaviors:
/// - strips a trailing `/` *and* a trailing `/ws` (the `agent_worktree`
///   variant did both; the `pair` variant only stripped `/ws`),
/// - flips `ws://`→`http://`, `wss://`→`https://`,
/// - passes through anything already `http(s)://` (or scheme-less) unchanged.
///
/// Stripping the bare trailing `/` is a strict superset: every input the `pair`
/// tests exercise (`ws://h:9870/ws`, `ws://h:9870`, `http://h:9870`) maps
/// identically under both old fns and this one. See the `unified_ws_to_http_*`
/// tests for the equivalence proof against both old suites.
pub fn coord_ws_to_http(coord_url: &str) -> String {
    let trimmed = coord_url.trim_end_matches('/').trim_end_matches("/ws");
    if let Some(rest) = trimmed.strip_prefix("ws://") {
        format!("http://{}", rest)
    } else if let Some(rest) = trimmed.strip_prefix("wss://") {
        format!("https://{}", rest)
    } else {
        trimmed.to_string()
    }
}

/// The dev-localhost coord base used when nothing is configured.
pub const DEV_LOCALHOST_COORD_BASE: &str = "http://localhost:9870";

/// Resolve the coord HTTP base WITHOUT applying any dev-localhost policy.
///
/// Chain: env `COORD_HTTP_URL` (non-empty, trimmed) → active profile's
/// `coord_url` (ws→http via [`coord_ws_to_http`]) → [`CoordBase::Unset`].
///
/// "Pure-ish": reads the process env and `~/.qontinui/profiles.json`, but does
/// no logging and no fallback — callers decide the localhost posture.
pub fn resolve_coord_base() -> CoordBase {
    if let Ok(v) = std::env::var("COORD_HTTP_URL") {
        let t = v.trim();
        if !t.is_empty() {
            return CoordBase::Configured(t.trim_end_matches('/').to_string());
        }
    }
    // `load_strict` (not `load`) so a missing/invalid profiles.json yields
    // Unset rather than a legacy-env best-effort with no coord_url — matches
    // the prior Option-family resolvers, which all used `load_strict`.
    if let Ok(p) = load_strict() {
        if let Some(ws) = p.coord_url.as_deref() {
            return CoordBase::Configured(coord_ws_to_http(ws));
        }
    }
    CoordBase::Unset
}

/// Process-global one-shot WARN guard for the dev-localhost fallback.
static DEV_LOCALHOST_WARN_ONCE: std::sync::Once = std::sync::Once::new();

/// Resolve the coord HTTP base, applying the dev-localhost policy.
///
/// - `Configured(base)` ⇒ `Some(base)`.
/// - Nothing configured ⇒ `Some("http://localhost:9870")`, emitting exactly
///   ONE process-global `tracing::warn!` (across all call sites, for the life
///   of the process) the first time the localhost guess is used.
///
/// Returns `None` only... never, actually — the localhost guess always applies
/// here. Callers that must stay no-op when unconfigured should call
/// [`resolve_coord_base`] directly and map `Configured ⇒ Some`, else `None`.
/// (The `Option` return type is kept so String-family sites can write
/// `coord_base_or_dev_localhost().unwrap_or_else(|| DEV_LOCALHOST.into())`
/// without changing should the policy ever grow an opt-out.)
pub fn coord_base_or_dev_localhost() -> Option<String> {
    match resolve_coord_base() {
        CoordBase::Configured(base) => Some(base),
        CoordBase::DevLocalhost(base) => Some(base),
        CoordBase::Unset => {
            DEV_LOCALHOST_WARN_ONCE.call_once(|| {
                warn!(
                    coord_base = DEV_LOCALHOST_COORD_BASE,
                    "no coord configured (COORD_HTTP_URL unset, profile has no coord_url) — \
                     falling back to dev-localhost coord; set COORD_HTTP_URL or profiles.json \
                     coord_url to point at the real coordinator"
                );
            });
            Some(DEV_LOCALHOST_COORD_BASE.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_profiles_file() {
        let json = r#"{
            "active": "dev",
            "profiles": {
                "dev": {
                    "database_url": "postgres://u:p@h:5433/db"
                }
            }
        }"#;
        let parsed: ProfilesFile = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.active.as_deref(), Some("dev"));
        let dev = parsed.profiles.get("dev").unwrap();
        assert_eq!(
            dev.database_url.as_deref(),
            Some("postgres://u:p@h:5433/db")
        );
    }

    #[test]
    fn parses_full_profiles_file() {
        let json = r#"{
            "active": "dev",
            "profiles": {
                "dev": {
                    "database_url": "postgres://u:p@h:5433/db",
                    "redis_url": "redis://h:6380/0",
                    "blob": {
                        "kind": "s3-compatible",
                        "endpoint": "http://h:9100",
                        "access_key": "k",
                        "secret_key": "s",
                        "bucket": "qontinui-dev"
                    },
                    "coord_url": "ws://h:9870",
                    "auth": { "kind": "static-dev-token", "token": "t" }
                }
            }
        }"#;
        let parsed: ProfilesFile = serde_json::from_str(json).unwrap();
        let dev = parsed.profiles.get("dev").unwrap();
        assert!(dev.blob.is_some());
        assert_eq!(dev.blob.as_ref().unwrap().kind, "s3-compatible");
        assert_eq!(dev.coord_url.as_deref(), Some("ws://h:9870"));
        assert_eq!(dev.auth.as_ref().unwrap().kind, "static-dev-token");
    }

    /// RAII guard that restores `RUNNER_DATABASE_URL` to its pre-test
    /// value on drop, including the panic path. Without this, a panic
    /// in the test body between `remove_var` and the manual restore
    /// would leak the unset state to any sibling test (current or
    /// future) that reads the var.
    struct DbUrlRestore {
        prev: Option<String>,
    }
    impl Drop for DbUrlRestore {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("RUNNER_DATABASE_URL", v),
                None => std::env::remove_var("RUNNER_DATABASE_URL"),
            }
        }
    }

    #[test]
    fn legacy_fallback_uses_env_or_localhost_default() {
        let _restore = DbUrlRestore {
            prev: std::env::var("RUNNER_DATABASE_URL").ok(),
        };
        std::env::remove_var("RUNNER_DATABASE_URL");
        let p = legacy_env_fallback();
        assert_eq!(p.source, "legacy-env");
        assert!(p.database_url.contains("qontinui_user"));
        // `_restore` drops here, including the panic path above.
    }

    // ------------------------------------------------------------------
    // Unified ws→http equivalence: the new `coord_ws_to_http` must match
    // BOTH historical normalizers on every case their own tests exercised.
    // ------------------------------------------------------------------

    /// Reference impl of the OLD `pair::coord_http_base_from_url` — kept
    /// here so the equivalence assertion is self-contained and survives
    /// the real fn being folded into a re-export.
    fn old_pair_from_url(coord_url: &str) -> String {
        let trimmed = coord_url.trim_end_matches("/ws");
        trimmed
            .strip_prefix("wss://")
            .map(|rest| format!("https://{rest}"))
            .or_else(|| {
                trimmed
                    .strip_prefix("ws://")
                    .map(|rest| format!("http://{rest}"))
            })
            .unwrap_or_else(|| trimmed.to_string())
    }

    /// Reference impl of the OLD `agent_worktree::coord_ws_to_http`.
    fn old_worktree_ws_to_http(coord_url: &str) -> String {
        let trimmed = coord_url.trim_end_matches('/').trim_end_matches("/ws");
        if let Some(rest) = trimmed.strip_prefix("ws://") {
            format!("http://{}", rest)
        } else if let Some(rest) = trimmed.strip_prefix("wss://") {
            format!("https://{}", rest)
        } else {
            trimmed.to_string()
        }
    }

    #[test]
    fn unified_ws_to_http_matches_old_pair_test_cases() {
        // Exactly the inputs pair.rs:923-956 asserts on.
        for input in [
            "ws://localhost:9870/ws",
            "wss://coord.qontinui.io:9870/ws",
            "ws://host:9870/ws",
            "ws://host:9870",
            "http://host:9870",
        ] {
            assert_eq!(
                coord_ws_to_http(input),
                old_pair_from_url(input),
                "unified disagrees with old pair fn on {input:?}"
            );
        }
        // Spot-check the concrete expected values too.
        assert_eq!(
            coord_ws_to_http("ws://localhost:9870/ws"),
            "http://localhost:9870"
        );
        assert_eq!(
            coord_ws_to_http("wss://coord.qontinui.io:9870/ws"),
            "https://coord.qontinui.io:9870"
        );
    }

    #[test]
    fn unified_ws_to_http_matches_old_worktree_test_cases() {
        // Exactly the inputs agent_worktree/mod.rs:1491-1512 asserts on.
        for input in [
            "ws://h:9870",
            "wss://h:9870",
            "http://h:9870",
            "https://h:9870",
            "ws://h:9870/ws",
            "wss://h:9870/ws",
            "http://h:9870/ws",
            "ws://h:9870/ws/",
            "ws://h:9870/",
        ] {
            assert_eq!(
                coord_ws_to_http(input),
                old_worktree_ws_to_http(input),
                "unified disagrees with old worktree fn on {input:?}"
            );
        }
        // The cases that distinguish the two old fns (trailing-slash handling).
        assert_eq!(coord_ws_to_http("ws://h:9870/ws/"), "http://h:9870");
        assert_eq!(coord_ws_to_http("ws://h:9870/"), "http://h:9870");
    }

    // ------------------------------------------------------------------
    // resolve_coord_base — env wins; profile ws→http; unset ⇒ Unset.
    // These mutate process-wide env, so serialize via a module mutex (the
    // runner test harness mutates env globally — memory
    // `feedback_env_var_tests_serialize`).
    // ------------------------------------------------------------------

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct CoordEnvRestore {
        prev: Option<String>,
    }
    impl Drop for CoordEnvRestore {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("COORD_HTTP_URL", v),
                None => std::env::remove_var("COORD_HTTP_URL"),
            }
        }
    }

    #[test]
    fn resolve_coord_base_env_wins() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = CoordEnvRestore {
            prev: std::env::var("COORD_HTTP_URL").ok(),
        };
        std::env::set_var("COORD_HTTP_URL", "http://env-coord:9999/");
        // Env wins regardless of any profiles.json on the test machine, and
        // the trailing slash is trimmed.
        assert_eq!(
            resolve_coord_base(),
            CoordBase::Configured("http://env-coord:9999".to_string())
        );
    }

    #[test]
    fn resolve_coord_base_empty_env_is_ignored() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = CoordEnvRestore {
            prev: std::env::var("COORD_HTTP_URL").ok(),
        };
        // Whitespace-only env is treated as unset; falls through to profile
        // (which, absent a configured coord_url in the test env, is Unset).
        std::env::set_var("COORD_HTTP_URL", "   ");
        // We can't assert the profile branch deterministically (the dev box
        // may have a profiles.json), but we CAN assert the empty env did not
        // win: the result is never `Configured("   ")`.
        assert_ne!(resolve_coord_base(), CoordBase::Configured("".to_string()));
        assert_ne!(
            resolve_coord_base(),
            CoordBase::Configured("   ".to_string())
        );
    }

    #[test]
    fn coord_base_or_dev_localhost_never_none() {
        // The String-family contract: this always yields Some(...). When the
        // env path is taken it's the configured base; otherwise it's the
        // localhost guess (Unset) or a profile value.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = CoordEnvRestore {
            prev: std::env::var("COORD_HTTP_URL").ok(),
        };
        std::env::set_var("COORD_HTTP_URL", "http://configured:1234");
        assert_eq!(
            coord_base_or_dev_localhost().as_deref(),
            Some("http://configured:1234")
        );
    }
}
