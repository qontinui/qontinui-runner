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
}
