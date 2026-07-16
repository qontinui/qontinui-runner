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
/// - String family: `coord_base_with_source().0`
/// - Option family: `Configured | TierDefault ⇒ Some`, otherwise `None`
///   (no-op when unconfigured on a non-hosted runner)
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
    /// Nothing was configured, but the runner tier is `qontinui_account`
    /// (hosted fleet), so the production coord base ([`PROD_COORD_BASE`])
    /// applies. A real, dialable coord: the String family uses it AND the
    /// Option family maps it to `Some` (unlike `DevLocalhost`) — a hosted
    /// runner with no profile must still heartbeat/forward against prod.
    TierDefault(String),
    /// Nothing configured AND the caller did not want a localhost guess.
    Unset,
}

/// Which arm of the resolution chain produced the effective coord base.
/// Threaded into proxy 502 error bodies and the doctor's `coord_reachable`
/// detail so a misconfigured upstream self-diagnoses in one read
/// (plan 2026-07-16-runner-prod-coord-base-default-and-502-self-diagnosis, D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordBaseSource {
    /// Env `COORD_HTTP_URL` won.
    Env,
    /// The active profile's `coord_url` won.
    Profile,
    /// Nothing configured; the runner tier is `qontinui_account`, so the
    /// production default ([`PROD_COORD_BASE`]) applied.
    TierDefault,
    /// Nothing configured and no hosted tier — the dev-localhost guess.
    DevLocalhostFallback,
}

impl CoordBaseSource {
    /// Stable wire string: `"env" | "profile" | "tier_default" |
    /// "dev_localhost_fallback"`. Used verbatim in proxy error JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            CoordBaseSource::Env => "env",
            CoordBaseSource::Profile => "profile",
            CoordBaseSource::TierDefault => "tier_default",
            CoordBaseSource::DevLocalhostFallback => "dev_localhost_fallback",
        }
    }
}

impl std::fmt::Display for CoordBaseSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
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

/// The production coord base applied when nothing is configured but the
/// runner tier declares hosted-fleet membership (`qontinui_account`).
/// Decided in exactly one place (plan
/// 2026-07-16-runner-prod-coord-base-default-and-502-self-diagnosis, D1).
pub const PROD_COORD_BASE: &str = "https://coord.qontinui.io";

/// The WebSocket form of [`PROD_COORD_BASE`], as persisted into the active
/// profile's `coord_url` by [`ensure_coord_url`] at hosted sign-in (D2).
/// `prod_coord_ws_url_matches_base` proves the pair stays in sync.
pub const PROD_COORD_WS_URL: &str = "wss://coord.qontinui.io/ws";

/// The `settings.json::tier` value that marks a hosted (production) runner —
/// the same discriminator the coord doctor's tier check keys on.
pub const QONTINUI_ACCOUNT_TIER: &str = "qontinui_account";

/// Resolve the coord HTTP base WITHOUT applying any fallback policy.
///
/// Chain: env `COORD_HTTP_URL` (non-empty, trimmed) → active profile's
/// `coord_url` (ws→http via [`coord_ws_to_http`]) → [`CoordBase::Unset`].
///
/// "Pure-ish": reads the process env and `~/.qontinui/profiles.json`, but does
/// no logging and no fallback — callers decide the fallback posture (usually
/// via [`coord_base_policy`] / [`coord_base_with_source`]).
pub fn resolve_coord_base() -> CoordBase {
    resolve_coord_base_with_source().0
}

/// [`resolve_coord_base`] plus WHICH configured arm matched: `Some(Env)` /
/// `Some(Profile)` for `Configured`, `None` for `Unset` (no arm matched).
fn resolve_coord_base_with_source() -> (CoordBase, Option<CoordBaseSource>) {
    if let Ok(v) = std::env::var("COORD_HTTP_URL") {
        let t = v.trim();
        if !t.is_empty() {
            return (
                CoordBase::Configured(t.trim_end_matches('/').to_string()),
                Some(CoordBaseSource::Env),
            );
        }
    }
    // `load_strict` (not `load`) so a missing/invalid profiles.json yields
    // Unset rather than a legacy-env best-effort with no coord_url — matches
    // the prior Option-family resolvers, which all used `load_strict`.
    if let Ok(p) = load_strict() {
        if let Some(ws) = p.coord_url.as_deref() {
            return (
                CoordBase::Configured(coord_ws_to_http(ws)),
                Some(CoordBaseSource::Profile),
            );
        }
    }
    (CoordBase::Unset, None)
}

// ---------------------------------------------------------------------------
// Runner-tier reader (relocated from `coord_doctor` so the policy layer and
// the doctor share ONE tier reader). Minimal read of `settings.json::tier` —
// the `Settings` struct itself is a main-binary module (not in lib.rs), which
// is exactly why this reads the JSON file instead of importing the type.
// ---------------------------------------------------------------------------

/// Path of the runner's `settings.json` (`QONTINUI_CONFIG_DIR` override →
/// platform config dir + `com.qontinui.runner`). The same file
/// `settings::load_settings()` reads.
pub fn settings_json_path() -> Option<PathBuf> {
    let dir = std::env::var("QONTINUI_CONFIG_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::config_dir().map(|d| d.join("com.qontinui.runner")))?;
    Some(dir.join("settings.json"))
}

/// The persisted runner tier as the serde snake_case string
/// (`"local"` | `"local_provider"` | `"qontinui_account"`), or `None` when the
/// settings file is absent/unreadable (which `Settings::default()` would treat
/// as `Local`).
pub fn read_runner_tier() -> Option<String> {
    let path = settings_json_path()?;
    let bytes = std::fs::read(path).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("tier")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Coord-base policy layer: resolver outcome × runner tier → effective base.
// ---------------------------------------------------------------------------

/// Pure policy core (unit-testable — tier injected as a parameter, no global
/// settings.json read): decide the effective coord base + its source from the
/// raw resolver outcome, the configured-arm source, and the runner tier.
///
/// - `Configured` passes through with its recorded source.
/// - `Unset` + tier `"qontinui_account"` ⇒ [`CoordBase::TierDefault`] on
///   [`PROD_COORD_BASE`] (a hosted runner must never dial dev-localhost).
/// - `Unset` + any other tier (incl. absent/unreadable settings.json) ⇒ the
///   existing dev-localhost guess.
fn apply_tier_policy(
    resolved: CoordBase,
    configured_source: Option<CoordBaseSource>,
    tier: Option<&str>,
) -> (CoordBase, CoordBaseSource) {
    match resolved {
        CoordBase::Configured(base) => (
            CoordBase::Configured(base),
            configured_source.unwrap_or(CoordBaseSource::Profile),
        ),
        CoordBase::TierDefault(base) => {
            (CoordBase::TierDefault(base), CoordBaseSource::TierDefault)
        }
        CoordBase::DevLocalhost(base) => (
            CoordBase::DevLocalhost(base),
            CoordBaseSource::DevLocalhostFallback,
        ),
        CoordBase::Unset => {
            if tier == Some(QONTINUI_ACCOUNT_TIER) {
                (
                    CoordBase::TierDefault(PROD_COORD_BASE.to_string()),
                    CoordBaseSource::TierDefault,
                )
            } else {
                (
                    CoordBase::DevLocalhost(DEV_LOCALHOST_COORD_BASE.to_string()),
                    CoordBaseSource::DevLocalhostFallback,
                )
            }
        }
    }
}

/// Process-global one-shot WARN guard for the dev-localhost fallback.
static DEV_LOCALHOST_WARN_ONCE: std::sync::Once = std::sync::Once::new();

/// Process-global one-shot INFO guard for the tier-default production base.
static TIER_DEFAULT_INFO_ONCE: std::sync::Once = std::sync::Once::new();

/// Resolve the coord base, applying the tier-aware fallback policy, keeping
/// the enum shape so Option-family callers can map each variant explicitly
/// (`Configured | TierDefault ⇒ Some`, `DevLocalhost | Unset ⇒ None`).
///
/// Never returns [`CoordBase::Unset`]: the unset state resolves to either the
/// tier default (hosted runner) or the dev-localhost guess, each announcing
/// itself with exactly ONE process-global log line (across all call sites, for
/// the life of the process) the first time it applies.
pub fn coord_base_policy() -> (CoordBase, CoordBaseSource) {
    let (resolved, configured_source) = resolve_coord_base_with_source();
    // The tier only matters for the Unset arm — skip the settings.json read
    // when a base is configured.
    let tier = match resolved {
        CoordBase::Unset => read_runner_tier(),
        _ => None,
    };
    let (base, source) = apply_tier_policy(resolved, configured_source, tier.as_deref());
    match source {
        CoordBaseSource::TierDefault => {
            TIER_DEFAULT_INFO_ONCE.call_once(|| {
                info!(
                    coord_base = PROD_COORD_BASE,
                    coord_base_source = source.as_str(),
                    "no coord configured (COORD_HTTP_URL unset, profile has no coord_url) — \
                     runner tier is qontinui_account, defaulting to the production \
                     coordinator"
                );
            });
        }
        CoordBaseSource::DevLocalhostFallback => {
            DEV_LOCALHOST_WARN_ONCE.call_once(|| {
                warn!(
                    coord_base = DEV_LOCALHOST_COORD_BASE,
                    coord_base_source = source.as_str(),
                    "no coord configured (COORD_HTTP_URL unset, profile has no coord_url) — \
                     falling back to dev-localhost coord; set COORD_HTTP_URL or profiles.json \
                     coord_url to point at the real coordinator"
                );
            });
        }
        CoordBaseSource::Env | CoordBaseSource::Profile => {}
    }
    (base, source)
}

/// The String-family policy fn: the effective coord HTTP base plus its
/// source. Successor of the old `coord_base_or_dev_localhost()` — every
/// String-family call site converges here so a production-tier runner with
/// nothing configured dials prod coord, not dev-localhost.
pub fn coord_base_with_source() -> (String, CoordBaseSource) {
    let (base, source) = coord_base_policy();
    let base = match base {
        CoordBase::Configured(b) | CoordBase::DevLocalhost(b) | CoordBase::TierDefault(b) => b,
        // Structurally unreachable (coord_base_policy never yields Unset);
        // fall back defensively rather than panicking.
        CoordBase::Unset => DEV_LOCALHOST_COORD_BASE.to_string(),
    };
    (base, source)
}

// ---------------------------------------------------------------------------
// D2 — persist `coord_url` at hosted sign-in, create-if-absent only.
// ---------------------------------------------------------------------------

/// Ensure the active profile in `~/.qontinui/profiles.json` has a `coord_url`,
/// creating the file / profile entry when missing. NEVER clobbers an existing
/// value — when `coord_url` is already present (non-null) the file is not
/// written at all (byte-identical).
///
/// Edits the file as `serde_json::Value` (read → mutate only the one missing
/// key → write), never via the typed [`ProfilesFile`] round-trip, which would
/// silently DROP any unknown keys a user's file carries.
pub fn ensure_coord_url(ws_url: &str) -> Result<()> {
    let path = profiles_path().ok_or_else(|| anyhow!("could not resolve home directory"))?;
    let env_active = std::env::var("QONTINUI_ENV").ok();
    ensure_coord_url_at(&path, env_active.as_deref(), ws_url)
}

/// Path-parameterized core of [`ensure_coord_url`] (hermetic tests point it at
/// a temp file; `env_active` stands in for the `QONTINUI_ENV` read so tests
/// never touch process env).
fn ensure_coord_url_at(
    path: &std::path::Path,
    env_active: Option<&str>,
    ws_url: &str,
) -> Result<()> {
    use serde_json::{Map, Value};

    if !path.exists() {
        // Fresh install: minimal `{active, profiles.{<active>}.coord_url}`.
        let active = env_active.unwrap_or("dev").to_string();
        let mut profile = Map::new();
        profile.insert("coord_url".to_string(), Value::String(ws_url.to_string()));
        let mut profiles = Map::new();
        profiles.insert(active.clone(), Value::Object(profile));
        let mut root = Map::new();
        root.insert("active".to_string(), Value::String(active));
        root.insert("profiles".to_string(), Value::Object(profiles));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut bytes = serde_json::to_vec_pretty(&Value::Object(root))?;
        bytes.push(b'\n');
        std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
        info!(
            "ensure_coord_url: created {} with coord_url",
            path.display()
        );
        return Ok(());
    }

    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut root: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing {} (refusing to overwrite)", path.display()))?;
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("{}: root is not a JSON object", path.display()))?;

    // Same active-profile selection as `load_inner`: env → file `active` → dev.
    let active = env_active
        .map(str::to_string)
        .or_else(|| {
            root_obj
                .get("active")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "dev".to_string());

    let profiles = root_obj
        .entry("profiles".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let profiles_obj = profiles
        .as_object_mut()
        .ok_or_else(|| anyhow!("{}: \"profiles\" is not a JSON object", path.display()))?;
    let profile = profiles_obj
        .entry(active.clone())
        .or_insert_with(|| Value::Object(Map::new()));
    let profile_obj = profile.as_object_mut().ok_or_else(|| {
        anyhow!(
            "{}: profile '{active}' is not a JSON object",
            path.display()
        )
    })?;

    match profile_obj.get("coord_url") {
        // Present with a real value ⇒ no write at all (byte-identical file).
        Some(v) if !v.is_null() => Ok(()),
        // Absent (or JSON null, which deserializes to None anyway): add ONLY
        // this key; every sibling key — known or unknown — rides along in the
        // Value tree untouched.
        _ => {
            profile_obj.insert("coord_url".to_string(), Value::String(ws_url.to_string()));
            let mut out = serde_json::to_vec_pretty(&root)?;
            out.push(b'\n');
            std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))?;
            info!(
                "ensure_coord_url: added coord_url to profile '{active}' in {}",
                path.display()
            );
            Ok(())
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
    fn coord_base_with_source_env_wins_and_reports_env_source() {
        // The String-family contract: always yields a base. When the env path
        // is taken it's the configured base with source `env`, regardless of
        // any profiles.json / settings.json on the test machine.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = CoordEnvRestore {
            prev: std::env::var("COORD_HTTP_URL").ok(),
        };
        std::env::set_var("COORD_HTTP_URL", "http://configured:1234");
        let (base, source) = coord_base_with_source();
        assert_eq!(base, "http://configured:1234");
        assert_eq!(source, CoordBaseSource::Env);
        assert_eq!(source.as_str(), "env");
    }

    #[test]
    fn resolve_with_source_env_arm_is_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = CoordEnvRestore {
            prev: std::env::var("COORD_HTTP_URL").ok(),
        };
        std::env::set_var("COORD_HTTP_URL", "https://env-coord.example/");
        let (base, source) = resolve_coord_base_with_source();
        assert_eq!(
            base,
            CoordBase::Configured("https://env-coord.example".to_string())
        );
        assert_eq!(source, Some(CoordBaseSource::Env));
    }

    // ------------------------------------------------------------------
    // apply_tier_policy — the pure matrix core. Tier is a PARAMETER here
    // (no global settings.json read), so these are fully hermetic.
    // ------------------------------------------------------------------

    #[test]
    fn policy_configured_env_passes_through_with_env_source() {
        let (base, source) = apply_tier_policy(
            CoordBase::Configured("https://c.example".into()),
            Some(CoordBaseSource::Env),
            Some(QONTINUI_ACCOUNT_TIER),
        );
        assert_eq!(base, CoordBase::Configured("https://c.example".into()));
        assert_eq!(source, CoordBaseSource::Env);
    }

    #[test]
    fn policy_configured_profile_passes_through_with_profile_source() {
        // Tier must be irrelevant when a base is configured — exercise all
        // tiers against the profile arm.
        for tier in [
            Some("local"),
            Some("local_provider"),
            Some(QONTINUI_ACCOUNT_TIER),
            None,
        ] {
            let (base, source) = apply_tier_policy(
                CoordBase::Configured("https://p.example".into()),
                Some(CoordBaseSource::Profile),
                tier,
            );
            assert_eq!(base, CoordBase::Configured("https://p.example".into()));
            assert_eq!(source, CoordBaseSource::Profile, "tier {tier:?}");
        }
    }

    #[test]
    fn policy_unset_hosted_tier_yields_prod_tier_default() {
        let (base, source) = apply_tier_policy(CoordBase::Unset, None, Some(QONTINUI_ACCOUNT_TIER));
        assert_eq!(base, CoordBase::TierDefault(PROD_COORD_BASE.to_string()));
        assert_eq!(source, CoordBaseSource::TierDefault);
        assert_eq!(source.as_str(), "tier_default");
    }

    #[test]
    fn policy_unset_non_hosted_tiers_keep_dev_localhost_guess() {
        // "local", "local_provider", absent/unreadable settings.json (None),
        // and even an unknown future tier string all keep the dev guess.
        for tier in [
            Some("local"),
            Some("local_provider"),
            Some("something_new"),
            None,
        ] {
            let (base, source) = apply_tier_policy(CoordBase::Unset, None, tier);
            assert_eq!(
                base,
                CoordBase::DevLocalhost(DEV_LOCALHOST_COORD_BASE.to_string()),
                "tier {tier:?}"
            );
            assert_eq!(
                source,
                CoordBaseSource::DevLocalhostFallback,
                "tier {tier:?}"
            );
            assert_eq!(source.as_str(), "dev_localhost_fallback");
        }
    }

    #[test]
    fn prod_coord_ws_url_matches_base() {
        // The D2 persisted WS url and the D1 HTTP default must always be the
        // same coordinator: ws→http normalization maps one onto the other.
        assert_eq!(coord_ws_to_http(PROD_COORD_WS_URL), PROD_COORD_BASE);
    }

    // ------------------------------------------------------------------
    // ensure_coord_url_at — create-if-absent file semantics (hermetic:
    // path-parameterized, env-active injected, temp dirs).
    // ------------------------------------------------------------------

    #[test]
    fn ensure_coord_url_absent_file_creates_minimal_structure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".qontinui").join("profiles.json");
        ensure_coord_url_at(&path, None, PROD_COORD_WS_URL).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["active"], "dev");
        assert_eq!(v["profiles"]["dev"]["coord_url"], PROD_COORD_WS_URL);
    }

    #[test]
    fn ensure_coord_url_present_value_is_untouched_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        // Deliberately quirky formatting + unknown keys: no write may occur.
        let original = "{\"active\":\"prod\",   \"mystery\": [1,2,3],\n \"profiles\":{\"prod\":{\"coord_url\":\"wss://custom.example/ws\",\"extra\":true}}}";
        std::fs::write(&path, original).unwrap();
        ensure_coord_url_at(&path, None, PROD_COORD_WS_URL).unwrap();
        let after = std::fs::read(&path).unwrap();
        assert_eq!(
            std::str::from_utf8(&after).unwrap(),
            original,
            "a present coord_url must mean NO write at all"
        );
    }

    #[test]
    fn ensure_coord_url_missing_key_added_unknown_siblings_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let original = serde_json::json!({
            "active": "dev",
            "unknown_top_level": {"keep": "me"},
            "profiles": {
                "dev": {
                    "database_url": "postgres://u:p@h:5433/db",
                    "unknown_profile_key": 42
                },
                "staging": {"coord_url": "wss://staging.example/ws"}
            }
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&original).unwrap()).unwrap();
        ensure_coord_url_at(&path, None, PROD_COORD_WS_URL).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // The one missing key was added…
        assert_eq!(v["profiles"]["dev"]["coord_url"], PROD_COORD_WS_URL);
        // …and every other byte of structure survives, including keys the
        // typed ProfilesFile round-trip would have dropped.
        assert_eq!(v["unknown_top_level"]["keep"], "me");
        assert_eq!(v["profiles"]["dev"]["unknown_profile_key"], 42);
        assert_eq!(
            v["profiles"]["dev"]["database_url"],
            "postgres://u:p@h:5433/db"
        );
        assert_eq!(
            v["profiles"]["staging"]["coord_url"],
            "wss://staging.example/ws"
        );
        assert_eq!(v["active"], "dev");
    }

    #[test]
    fn ensure_coord_url_env_active_selects_that_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let original = serde_json::json!({
            "active": "dev",
            "profiles": {"dev": {}, "cloud": {}}
        });
        std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        // env override (QONTINUI_ENV stand-in) targets "cloud", not "dev".
        ensure_coord_url_at(&path, Some("cloud"), PROD_COORD_WS_URL).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["profiles"]["cloud"]["coord_url"], PROD_COORD_WS_URL);
        assert!(v["profiles"]["dev"].get("coord_url").is_none());
    }

    #[test]
    fn ensure_coord_url_active_profile_entry_created_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let original = serde_json::json!({"active": "prod", "profiles": {}});
        std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        ensure_coord_url_at(&path, None, PROD_COORD_WS_URL).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["profiles"]["prod"]["coord_url"], PROD_COORD_WS_URL);
    }

    #[test]
    fn ensure_coord_url_unparseable_file_errors_without_clobbering() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(ensure_coord_url_at(&path, None, PROD_COORD_WS_URL).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{not json");
    }
}
