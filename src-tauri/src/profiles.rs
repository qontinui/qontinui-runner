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
/// - String family: [`coord_base_with_source`] — always yields a base, so it
///   CANNOT express "isolated";
/// - Option family: [`connected_coord_base`] — the single connected-vs-isolated
///   door. It keys on the `(base, source)` PAIR, not on this variant alone,
///   because [`Self::TierDefault`] is produced by two different inputs and only
///   one of them means connected. Do not re-derive the rule from a bare
///   variant match: that is the defect that classified the entire hosted fleet
///   as isolated.
///
/// There is deliberately no third "Result family" policy. A call site that
/// wants a `Result` maps one of the two above at the boundary; a private
/// per-module wrapper is what let three policies coexist in the first place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoordBase {
    /// A coord base was explicitly configured (env `COORD_HTTP_URL` or the
    /// active profile's `coord_url`). The string is already ws→http normalized.
    Configured(String),
    /// Nothing was configured; this is the dev-localhost guess
    /// (`http://localhost:9870`). Treated as "configured enough" for the
    /// String family but as "unconfigured" (None) for the Option family.
    DevLocalhost(String),
    /// Nothing was configured, but the production coord base
    /// ([`PROD_COORD_BASE`]) applies anyway. **Two different inputs produce
    /// this variant, and the accompanying [`CoordBaseSource`] is the only
    /// thing that tells them apart:**
    ///
    /// - [`CoordBaseSource::TierDefault`] — the runner tier was READ and it
    ///   says `qontinui_account` (hosted fleet). A real, dialable coord: the
    ///   String family uses it AND [`connected_coord_base`] maps it to `Some`
    ///   (unlike `DevLocalhost`) — a hosted runner with no profile must still
    ///   heartbeat/forward against prod.
    /// - [`CoordBaseSource::UnknownTierProdDefault`] — `settings.json` could
    ///   not be read, so the tier is UNKNOWN and prod was a GUESS. The String
    ///   family still uses it (it wants the loud failure);
    ///   [`connected_coord_base`] maps it to `None`, because an unknown tier
    ///   must not authorize egress to production.
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
    /// Nothing configured AND settings.json was unreadable, so the tier is
    /// unknown. The production default applied rather than the dev-localhost
    /// guess — see [`apply_tier_policy`]. Distinct from [`Self::TierDefault`]
    /// so the doctor / 502 bodies can say "we guessed prod because we could
    /// not read your tier", not "your tier is qontinui_account".
    UnknownTierProdDefault,
}

impl CoordBaseSource {
    /// Stable wire string: `"env" | "profile" | "tier_default" |
    /// "dev_localhost_fallback" | "unknown_tier_prod_default"`. Used verbatim
    /// in proxy error JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            CoordBaseSource::Env => "env",
            CoordBaseSource::Profile => "profile",
            CoordBaseSource::TierDefault => "tier_default",
            CoordBaseSource::DevLocalhostFallback => "dev_localhost_fallback",
            CoordBaseSource::UnknownTierProdDefault => "unknown_tier_prod_default",
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
///
/// # This fn CANNOT answer "is this runner connected?"
///
/// It returns only [`CoordBase::Configured`] or [`CoordBase::Unset`] — it never
/// applies the tier arm, so it reports `Unset` for the SHIPPED end-user hosted
/// configuration (a `qontinui_account`-tier runner whose profiles.json carries
/// no `coord_url`). Ten Option-family call sites once matched
/// `Configured ⇒ Some, _ => None` on this and thereby classified the entire
/// hosted fleet as isolated, silently dropping its fleet state.
///
/// Use [`connected_coord_base`] for the connected-vs-isolated decision, or
/// [`coord_base_with_source`] when you need a base string unconditionally.
/// Reach for this one only to inspect what was *explicitly configured*, with
/// no policy applied.
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

/// Outcome of reading `settings.json::tier` — a tri-state, because "we could
/// not read the file" is NOT the same fact as "the runner is Tier 0/1".
///
/// The old `Option<String>` reader discarded BOTH the read error and the parse
/// error with `.ok()?`, so a transient file lock or a truncated settings.json
/// looked identical to a genuinely local runner. Downstream
/// ([`apply_tier_policy`]) that meant a hosted production runner silently
/// dialed dev-localhost for coord — disabling gates, work units, fleet
/// coordination and the merge train, with no error anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TierRead {
    /// settings.json parsed and carries this tier value.
    Known(String),
    /// settings.json parsed but has no usable `tier` (and no `runner_token` to
    /// infer one from) — a genuinely tier-less install.
    Absent,
    /// settings.json could not be read or parsed. The tier is UNKNOWN.
    Unknown(String),
}

impl TierRead {
    /// The tier string when it is actually known. `None` for both `Absent`
    /// and `Unknown` — callers that care about the difference must match.
    pub fn known(&self) -> Option<&str> {
        match self {
            Self::Known(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// The persisted runner tier as the serde snake_case string
/// (`"local"` | `"local_provider"` | `"qontinui_account"`).
///
/// Reads the JSON directly rather than importing `Settings` because the
/// `Settings` struct is a main-binary module (not in lib.rs). Errors are
/// PRESERVED as [`TierRead::Unknown`] — see the type docs for why.
///
/// When the document parses but has no `tier` key (a pre-tier settings.json,
/// before `migrate_tier_in_place` has run once), the tier is inferred from a
/// non-empty `web_integration.runner_token` exactly as that migration does —
/// otherwise a hosted install would read as `Absent` during the one boot
/// before the migration persists.
pub fn read_runner_tier() -> TierRead {
    let Some(path) = settings_json_path() else {
        return TierRead::Unknown("cannot resolve settings.json path".to_string());
    };
    if !path.exists() {
        return TierRead::Absent;
    }
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            return TierRead::Unknown(format!("read {} failed: {e}", path.display()));
        }
    };
    let json: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(j) => j,
        Err(e) => {
            return TierRead::Unknown(format!("parse {} failed: {e}", path.display()));
        }
    };
    if let Some(t) = json.get("tier").and_then(|v| v.as_str()) {
        return TierRead::Known(t.to_string());
    }
    // Pre-tier document: mirror `settings::migrate_tier_in_place`'s inference.
    let has_runner_token = json
        .get("web_integration")
        .and_then(|w| w.get("runner_token"))
        .and_then(|v| v.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if has_runner_token {
        TierRead::Known(QONTINUI_ACCOUNT_TIER.to_string())
    } else {
        TierRead::Absent
    }
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
/// - `Unset` + [`TierRead::Unknown`] (settings.json unreadable) ⇒ ALSO
///   [`CoordBase::TierDefault`] on [`PROD_COORD_BASE`], with source
///   [`CoordBaseSource::UnknownTierProdDefault`]. An unreadable tier is
///   "unknown", not "local": guessing dev-localhost silently severs a hosted
///   runner from coord (no gates, no work units, no merge train) with nothing
///   in the logs. Guessing production instead fails LOUDLY (auth/DNS error) on
///   a genuine dev box, which is the recoverable direction.
/// - `Unset` + [`TierRead::Absent`] / any other known tier ⇒ the existing
///   dev-localhost guess.
fn apply_tier_policy(
    resolved: CoordBase,
    configured_source: Option<CoordBaseSource>,
    tier: &TierRead,
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
        CoordBase::Unset => match tier {
            TierRead::Known(t) if t == QONTINUI_ACCOUNT_TIER => (
                CoordBase::TierDefault(PROD_COORD_BASE.to_string()),
                CoordBaseSource::TierDefault,
            ),
            TierRead::Unknown(_) => (
                CoordBase::TierDefault(PROD_COORD_BASE.to_string()),
                CoordBaseSource::UnknownTierProdDefault,
            ),
            _ => (
                CoordBase::DevLocalhost(DEV_LOCALHOST_COORD_BASE.to_string()),
                CoordBaseSource::DevLocalhostFallback,
            ),
        },
    }
}

/// Process-global one-shot WARN guard for the dev-localhost fallback.
static DEV_LOCALHOST_WARN_ONCE: std::sync::Once = std::sync::Once::new();

/// Process-global one-shot INFO guard for the tier-default production base.
static TIER_DEFAULT_INFO_ONCE: std::sync::Once = std::sync::Once::new();

/// Process-global one-shot WARN guard for the unknown-tier production default.
static UNKNOWN_TIER_WARN_ONCE: std::sync::Once = std::sync::Once::new();

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
        _ => TierRead::Absent,
    };
    let (base, source) = apply_tier_policy(resolved, configured_source, &tier);
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
        CoordBaseSource::UnknownTierProdDefault => {
            let reason = match &tier {
                TierRead::Unknown(e) => e.clone(),
                _ => "unknown".to_string(),
            };
            UNKNOWN_TIER_WARN_ONCE.call_once(|| {
                warn!(
                    coord_base = PROD_COORD_BASE,
                    coord_base_source = source.as_str(),
                    reason = %reason,
                    "no coord configured AND settings.json could not be read, so the runner \
                     tier is UNKNOWN — defaulting to the PRODUCTION coordinator rather than \
                     silently dialing dev-localhost (a hosted runner pointed at localhost \
                     loses gates, work units and the merge train with no error). Fix \
                     settings.json, or set COORD_HTTP_URL explicitly."
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

/// **THE** definition of "this runner is CONNECTED to a coordinator."
///
/// The runner has exactly two modes, and mode is a property of the RUNNER, not
/// of the call site:
///
/// - **connected** — the user has a qontinui account (or has explicitly named a
///   coord), so fleet state uploads to a real coordinator;
/// - **isolated** — a standalone app with no coordinator, where every coord
///   surface must no-op cleanly rather than dial a phantom endpoint.
///
/// Mode is derived from CONFIGURATION ONLY — never from reachability. A network
/// blip, a coord outage, or a 503 must never flip a connected runner to
/// isolated: this fn reads env / profiles.json / settings.json and nothing else.
///
/// Option-family mapping of [`coord_base_policy`]. Note it keys on the
/// `(base, source)` PAIR, not on the variant alone — [`CoordBase::TierDefault`]
/// is produced by two different inputs and only one of them means "connected":
///
/// | Variant | Source | Result | Why |
/// |---|---|---|---|
/// | [`CoordBase::Configured`] | `Env` / `Profile` | `Some(base)` | explicitly named coord |
/// | [`CoordBase::TierDefault`] | [`CoordBaseSource::TierDefault`] | `Some(base)` | tier READ, and it says `qontinui_account` |
/// | [`CoordBase::TierDefault`] | [`CoordBaseSource::UnknownTierProdDefault`] | **`None`** | tier NOT read — see below |
/// | [`CoordBase::DevLocalhost`] | `DevLocalhostFallback` | `None` | a GUESS, not a coord |
/// | [`CoordBase::Unset`] | — | `None` | structurally unreachable here |
///
/// **Why the genuine `TierDefault` counts as connected.** It is the SHIPPED
/// end-user configuration: a `qontinui_account`-tier runner that has never had
/// a `coord_url` written into `~/.qontinui/profiles.json`.
/// [`PROD_COORD_BASE`] is a real, dialable coordinator that this runner is a
/// member of; the hosted fleet must still heartbeat, register worktrees, push
/// work units and forward reviews against it. Treating that as isolated
/// classifies the ENTIRE hosted fleet as standalone and silently drops its
/// fleet state — which is precisely the defect that made
/// `HttpWorkUnitSink::from_profile` return `None` on every shipped hosted
/// runner.
///
/// **Why [`CoordBaseSource::UnknownTierProdDefault`] does NOT.** That arm fires
/// when `settings.json` could not be read or parsed, so the tier is UNKNOWN —
/// and unknown must never authorize egress to production. The failure is
/// routine, not exotic: [`read_runner_tier`] returns [`TierRead::Unknown`] on
/// any read/parse error, including losing a race with another runner
/// instance's `fs_atomic` rename of that same file, and [`coord_base_policy`]
/// re-reads it on every call with no caching. A `tier: "local"` dev box that
/// loses that race ONCE would otherwise start dialing
/// `https://coord.qontinui.io` from every Option-family surface — fetching
/// fleet auto-response rules from production, persisting them to
/// `~/.qontinui/fleet-auto-response-rules.json` (from which boot re-seeds them,
/// so a single transient failure arms them permanently), injecting them into
/// live operator terminals, and then shipping the matched screen text and the
/// injected prompt back to prod on every hit. These call sites are
/// fire-and-forget and swallow their outcomes at `debug!`/`warn!`, so the
/// "guessing production fails LOUDLY" rationale in [`apply_tier_policy`] —
/// sound for the String family, which surfaces the failure to an operator —
/// does not transfer to this one. Absence of evidence about the tier is not
/// evidence of membership.
///
/// The String family's posture is deliberately left unchanged:
/// [`coord_base_with_source`] still yields the production base on an unknown
/// tier, because a doctor/proxy caller wants the loud failure.
///
/// `DevLocalhost` stays `None` on the opposite argument: `http://localhost:9870`
/// is a guess made when nothing at all is configured on a non-hosted runner.
/// Dialing it would spam connection errors at a coordinator that is usually not
/// running, and would report "connected" for a runner that is plainly isolated.
///
/// Callers that need the always-returns-a-`String` shape (proxies, doctor,
/// diagnostics) want [`coord_base_with_source`] instead — that family cannot
/// express "isolated" at all, so it is never the right door for a feature that
/// must no-op when the runner is standalone.
pub fn connected_coord_base() -> Option<String> {
    let (base, source) = coord_base_policy();
    classify_connected(base, source)
}

/// The connected-vs-isolated rule itself, as a PURE fn over one
/// [`coord_base_policy`] reading.
///
/// Split out so [`connected_coord_base`] and [`coord_mode`] cannot drift, and
/// so neither has to read `settings.json` twice. That second read is not
/// hypothetical: [`coord_base_policy`] re-reads the file on every call with no
/// caching, and [`read_runner_tier`] returns [`TierRead::Unknown`] whenever it
/// loses a race with another runner instance's atomic rewrite — so a
/// two-call implementation could report `mode: "isolated"` alongside a
/// `source` that says `tier_default`, from the same instant.
fn classify_connected(base: CoordBase, source: CoordBaseSource) -> Option<String> {
    match (base, source) {
        // Explicitly configured — env `COORD_HTTP_URL` or the profile's
        // `coord_url`. The tier is irrelevant; the operator named the coord.
        (CoordBase::Configured(base), _) => Some(base.trim_end_matches('/').to_string()),
        // The tier was actually READ and it says `qontinui_account`.
        (CoordBase::TierDefault(base), CoordBaseSource::TierDefault) => {
            Some(base.trim_end_matches('/').to_string())
        }
        // Everything else is ISOLATED. Notably `(TierDefault,
        // UnknownTierProdDefault)`: the production base was a GUESS made
        // because settings.json was unreadable, and an unknown tier must not
        // authorize egress to prod. Also `DevLocalhost` (a guess, not a coord)
        // and `Unset` (structurally unreachable — `coord_base_policy` never
        // yields it).
        _ => None,
    }
}

/// The two modes a runner can be in, as the frontend sees them.
///
/// Serialized in `snake_case`, i.e. `"connected"` / `"isolated"` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordConnectionMode {
    /// A real coordinator is configured (or implied by the hosted tier): every
    /// coord-backed surface is live.
    Connected,
    /// No coordinator: plan / task / review / worktree surfaces must render
    /// DISABLED with an explicit reason, not broken or silently empty.
    Isolated,
}

impl CoordConnectionMode {
    /// Stable wire string, matching the serde representation.
    pub fn as_str(self) -> &'static str {
        match self {
            CoordConnectionMode::Connected => "connected",
            CoordConnectionMode::Isolated => "isolated",
        }
    }
}

/// The runner's coord mode plus the evidence behind it — the shape the
/// frontend consumes (plan
/// `2026-08-18-runner-embedded-pg-parity-and-coord-http-migration` §6.4).
///
/// `get_coord_http_base` cannot express this: it belongs to the always-yields-a-
/// base String family, so an isolated runner gets back `http://localhost:9870`
/// and the UI cannot tell "coord lives there" from "there is no coord". The UI
/// requirement is to show the plan / task / review / worktree surfaces DISABLED
/// with an explicit "connect a qontinui account to enable" reason, which needs a
/// mode, not a string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordModeReport {
    /// `connected` iff [`connected_coord_base`] would yield `Some`.
    pub mode: CoordConnectionMode,
    /// The connected base, `None` iff `mode == Isolated`. Deliberately NOT the
    /// String family's answer: surfacing the dev-localhost guess (or a
    /// prod base guessed off an unreadable `settings.json`) here would let a
    /// caller dial exactly the endpoint the mode says not to.
    pub base: Option<String>,
    /// Which arm of the resolution chain decided it — [`CoordBaseSource::as_str`].
    /// Present in BOTH modes, because it is what makes the isolated reason
    /// actionable: `dev_localhost_fallback` means "no account configured",
    /// while `unknown_tier_prod_default` means "settings.json is unreadable —
    /// fix that file", which is a different message to show the operator.
    pub source: String,
}

/// One reading of the runner's coord mode. Derived from CONFIGURATION ONLY, so
/// it never flips on a network blip — see [`connected_coord_base`].
pub fn coord_mode() -> CoordModeReport {
    let (base, source) = coord_base_policy();
    let base = classify_connected(base, source);
    CoordModeReport {
        mode: match base {
            Some(_) => CoordConnectionMode::Connected,
            None => CoordConnectionMode::Isolated,
        },
        base,
        source: source.as_str().to_string(),
    }
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
    use crate::test_env::env_lock;

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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
            &TierRead::Known(QONTINUI_ACCOUNT_TIER.to_string()),
        );
        assert_eq!(base, CoordBase::Configured("https://c.example".into()));
        assert_eq!(source, CoordBaseSource::Env);
    }

    #[test]
    fn policy_configured_profile_passes_through_with_profile_source() {
        // Tier must be irrelevant when a base is configured — exercise all
        // tiers (including the unreadable one) against the profile arm.
        for tier in [
            TierRead::Known("local".into()),
            TierRead::Known("local_provider".into()),
            TierRead::Known(QONTINUI_ACCOUNT_TIER.into()),
            TierRead::Absent,
            TierRead::Unknown("io error".into()),
        ] {
            let (base, source) = apply_tier_policy(
                CoordBase::Configured("https://p.example".into()),
                Some(CoordBaseSource::Profile),
                &tier,
            );
            assert_eq!(base, CoordBase::Configured("https://p.example".into()));
            assert_eq!(source, CoordBaseSource::Profile, "tier {tier:?}");
        }
    }

    #[test]
    fn policy_unset_hosted_tier_yields_prod_tier_default() {
        let (base, source) = apply_tier_policy(
            CoordBase::Unset,
            None,
            &TierRead::Known(QONTINUI_ACCOUNT_TIER.to_string()),
        );
        assert_eq!(base, CoordBase::TierDefault(PROD_COORD_BASE.to_string()));
        assert_eq!(source, CoordBaseSource::TierDefault);
        assert_eq!(source.as_str(), "tier_default");
    }

    #[test]
    fn policy_unset_non_hosted_tiers_keep_dev_localhost_guess() {
        // "local", "local_provider", a genuinely tier-less settings.json, and
        // even an unknown future tier string all keep the dev guess. NOTE:
        // `TierRead::Unknown` is deliberately NOT in this list — see
        // `policy_unset_unreadable_tier_prefers_production`.
        for tier in [
            TierRead::Known("local".into()),
            TierRead::Known("local_provider".into()),
            TierRead::Known("something_new".into()),
            TierRead::Absent,
        ] {
            let (base, source) = apply_tier_policy(CoordBase::Unset, None, &tier);
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

    /// H4: an UNREADABLE settings.json means the tier is unknown, and a hosted
    /// runner must never be silently dropped onto dev-localhost coord (which
    /// costs it gates, work units, fleet coordination and the merge train with
    /// no error anywhere). Prefer production — that direction fails loudly on
    /// a genuine dev box instead of failing silently on a hosted one.
    #[test]
    fn policy_unset_unreadable_tier_prefers_production() {
        let (base, source) = apply_tier_policy(
            CoordBase::Unset,
            None,
            &TierRead::Unknown("sharing violation".into()),
        );
        assert_eq!(
            base,
            CoordBase::TierDefault(PROD_COORD_BASE.to_string()),
            "an unknown tier must not resolve to dev-localhost"
        );
        assert_eq!(source, CoordBaseSource::UnknownTierProdDefault);
        assert_eq!(source.as_str(), "unknown_tier_prod_default");
    }

    /// `TierRead::Unknown` must stay distinguishable from every known tier and
    /// from `Absent` — the whole point of the tri-state.
    #[test]
    fn tier_read_unknown_is_not_a_known_tier() {
        assert_eq!(TierRead::Known("local".into()).known(), Some("local"));
        assert_eq!(TierRead::Absent.known(), None);
        assert_eq!(TierRead::Unknown("boom".into()).known(), None);
        assert_ne!(TierRead::Absent, TierRead::Unknown("boom".into()));
    }

    // ------------------------------------------------------------------
    // connected_coord_base — the single definition of "connected". These go
    // end-to-end (env → profiles.json → settings.json tier → policy → Option)
    // rather than through `apply_tier_policy`, because the DEFECT this phase
    // fixes lived in the composition, not in the policy matrix: the policy
    // already said `TierDefault ⇒ Some`, and the Option-family call sites
    // simply never asked it.
    //
    // Hermetic on any machine, including a dev box with a real
    // `~/.qontinui/profiles.json`:
    //   * `COORD_HTTP_URL` removed  ⇒ the env arm misses;
    //   * `QONTINUI_ENV` pointed at a profile name that cannot exist ⇒
    //     `load_strict()` errors ⇒ the profile arm misses ⇒ `Unset`;
    //   * `QONTINUI_CONFIG_DIR` pointed at a temp dir ⇒ `read_runner_tier()`
    //     reads OUR settings.json, never the operator's.
    // Serialized on the shared `env_lock` (process-global env), and restored
    // through `EnvVarRestore` on the panic path too.
    // ------------------------------------------------------------------

    /// A profile name no real `profiles.json` can carry, so the profile arm of
    /// `resolve_coord_base()` misses deterministically on every machine.
    const NO_SUCH_PROFILE: &str = "__qontinui_test_no_such_profile__";

    /// Env vars every `connected_coord_base` test mutates.
    const COORD_ENV_KEYS: &[&str] = &["COORD_HTTP_URL", "QONTINUI_ENV", "QONTINUI_CONFIG_DIR"];

    /// Point the resolver at a hermetic config dir with nothing configured:
    /// no `COORD_HTTP_URL`, an unresolvable active profile, and `settings.json`
    /// written from `settings_json` (`None` ⇒ no file at all, i.e.
    /// [`TierRead::Absent`]).
    fn isolate_coord_env(dir: &std::path::Path, settings_json: Option<&str>) {
        std::env::remove_var("COORD_HTTP_URL");
        std::env::set_var("QONTINUI_ENV", NO_SUCH_PROFILE);
        std::env::set_var("QONTINUI_CONFIG_DIR", dir);
        if let Some(body) = settings_json {
            std::fs::write(dir.join("settings.json"), body).unwrap();
        }
        // Precondition: nothing is configured, so the tier arm is what decides.
        assert_eq!(
            resolve_coord_base(),
            CoordBase::Unset,
            "test setup failed to reach the Unset arm"
        );
    }

    /// The shipped end-user hosted configuration: tier `qontinui_account`, no
    /// `coord_url` in profiles.json, no `COORD_HTTP_URL`. This runner IS
    /// connected — reading it as isolated silently drops the entire hosted
    /// fleet's work units, worktrees, reviews, plans and tasks.
    #[test]
    fn connected_coord_base_hosted_tier_with_nothing_configured_is_connected() {
        let _g = env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(COORD_ENV_KEYS);
        let dir = tempfile::tempdir().unwrap();
        isolate_coord_env(
            dir.path(),
            Some(&format!(r#"{{"tier":"{QONTINUI_ACCOUNT_TIER}"}}"#)),
        );
        assert_eq!(
            read_runner_tier(),
            TierRead::Known(QONTINUI_ACCOUNT_TIER.into())
        );
        assert_eq!(
            connected_coord_base(),
            Some(PROD_COORD_BASE.to_string()),
            "a hosted runner with no explicit coord_url must read as CONNECTED"
        );
    }

    /// An UNREADABLE settings.json means the tier is UNKNOWN — and unknown must
    /// NOT authorize egress to production.
    ///
    /// The String family deliberately keeps its fail-loud posture here (it
    /// still yields [`PROD_COORD_BASE`], with source
    /// [`CoordBaseSource::UnknownTierProdDefault`], so a doctor/proxy caller
    /// gets an auth/DNS error it can surface). The Option family must not: its
    /// call sites are fire-and-forget pollers that swallow outcomes at
    /// `debug!`/`warn!`, so a hosted-looking guess produces silent egress
    /// instead of a loud failure. `read_runner_tier` returns `Unknown` on ANY
    /// read/parse error — including losing a race with another runner
    /// instance's atomic rewrite of settings.json — and `coord_base_policy`
    /// re-reads the file on every call, so a `tier: "local"` box only has to
    /// lose that race once. Absence of evidence about the tier is not evidence
    /// of fleet membership.
    #[test]
    fn connected_coord_base_unreadable_settings_is_isolated() {
        let _g = env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(COORD_ENV_KEYS);
        let dir = tempfile::tempdir().unwrap();
        isolate_coord_env(dir.path(), Some("{not json"));
        assert!(matches!(read_runner_tier(), TierRead::Unknown(_)));
        assert_eq!(
            connected_coord_base(),
            None,
            "an UNKNOWN tier must not read as connected — that would let one \
             transient settings.json read failure point a local dev box at \
             production coord"
        );
        // The policy layer itself is untouched: it still resolves the prod base
        // with the `unknown_tier_prod_default` source. Only the Option family's
        // interpretation of that pair changed.
        let (base, source) = coord_base_policy();
        assert_eq!(base, CoordBase::TierDefault(PROD_COORD_BASE.to_string()));
        assert_eq!(source, CoordBaseSource::UnknownTierProdDefault);
        // …and the String family keeps its fail-loud posture unchanged.
        assert_eq!(coord_base_with_source().0, PROD_COORD_BASE);
    }

    /// A non-hosted runner with nothing configured is ISOLATED — and must NOT
    /// leak the dev-localhost guess into the Option family.
    #[test]
    fn connected_coord_base_non_hosted_tier_is_isolated() {
        let _g = env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(COORD_ENV_KEYS);
        for tier in ["local", "local_provider", "something_new"] {
            let dir = tempfile::tempdir().unwrap();
            isolate_coord_env(dir.path(), Some(&format!(r#"{{"tier":"{tier}"}}"#)));
            assert_eq!(
                connected_coord_base(),
                None,
                "tier {tier} must read as isolated"
            );
            // The non-vacuous half of the property: the String family DOES
            // still hand back the dev-localhost guess on this exact input, so
            // the `None` above is the Option family deliberately dropping a
            // base that was available — not an input that produced nothing.
            // That is precisely what the guess must never leak as "connected".
            let (base, source) = coord_base_with_source();
            assert_eq!(base, DEV_LOCALHOST_COORD_BASE, "tier {tier}");
            assert_eq!(source, CoordBaseSource::DevLocalhostFallback, "tier {tier}");
        }
    }

    /// No settings.json at all (`TierRead::Absent`) is a genuinely tier-less
    /// install: isolated.
    #[test]
    fn connected_coord_base_absent_settings_is_isolated() {
        let _g = env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(COORD_ENV_KEYS);
        let dir = tempfile::tempdir().unwrap();
        isolate_coord_env(dir.path(), None);
        assert_eq!(read_runner_tier(), TierRead::Absent);
        assert_eq!(connected_coord_base(), None);
    }

    /// An explicit `COORD_HTTP_URL` wins over every tier — including the ones
    /// that would otherwise isolate the runner.
    #[test]
    fn connected_coord_base_env_wins_over_every_tier() {
        let _g = env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(COORD_ENV_KEYS);
        for tier in ["local", "local_provider", QONTINUI_ACCOUNT_TIER] {
            let dir = tempfile::tempdir().unwrap();
            isolate_coord_env(dir.path(), Some(&format!(r#"{{"tier":"{tier}"}}"#)));
            std::env::set_var("COORD_HTTP_URL", "https://explicit.example/");
            assert_eq!(
                connected_coord_base(),
                Some("https://explicit.example".to_string()),
                "explicit COORD_HTTP_URL must win on tier {tier} (and be slash-trimmed)"
            );
        }
    }

    // ------------------------------------------------------------------
    // coord_mode — the frontend-facing projection (plan §6.4). These pin the
    // MAPPING, not the policy: the policy is already covered above, and what
    // could silently rot here is the report drifting from
    // `connected_coord_base` (a UI that renders "connected" while every
    // Option-family call site no-ops is worse than one that renders nothing).
    // ------------------------------------------------------------------

    /// The shipped hosted configuration reports CONNECTED, carries the base,
    /// and names the arm that decided it.
    #[test]
    fn coord_mode_hosted_tier_is_connected_with_a_base() {
        let _g = env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(COORD_ENV_KEYS);
        let dir = tempfile::tempdir().unwrap();
        isolate_coord_env(
            dir.path(),
            Some(&format!(r#"{{"tier":"{QONTINUI_ACCOUNT_TIER}"}}"#)),
        );
        let got = coord_mode();
        assert_eq!(
            got,
            CoordModeReport {
                mode: CoordConnectionMode::Connected,
                base: Some(PROD_COORD_BASE.to_string()),
                source: "tier_default".to_string(),
            }
        );
    }

    /// A non-hosted runner with nothing configured reports ISOLATED — and
    /// `base` is `None`, NOT the dev-localhost guess the String family would
    /// have handed back. A UI that received that guess could dial exactly the
    /// endpoint the mode just told it does not exist.
    #[test]
    fn coord_mode_isolated_carries_no_base_and_names_the_guess() {
        let _g = env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(COORD_ENV_KEYS);
        let dir = tempfile::tempdir().unwrap();
        isolate_coord_env(dir.path(), Some(r#"{"tier":"local"}"#));
        let got = coord_mode();
        assert_eq!(got.mode, CoordConnectionMode::Isolated);
        assert_eq!(got.base, None);
        assert_eq!(got.source, "dev_localhost_fallback");
        // The String family DOES have a base on this same input — which is why
        // it cannot express this mode.
        assert_eq!(coord_base_with_source().0, DEV_LOCALHOST_COORD_BASE);
    }

    /// An unreadable settings.json reports ISOLATED with the
    /// `unknown_tier_prod_default` source, so the UI can say "fix
    /// settings.json" rather than "connect an account" — a different, and
    /// actionable, reason.
    #[test]
    fn coord_mode_unknown_tier_is_isolated_with_a_distinguishable_reason() {
        let _g = env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(COORD_ENV_KEYS);
        let dir = tempfile::tempdir().unwrap();
        isolate_coord_env(dir.path(), Some("{not json"));
        let got = coord_mode();
        assert_eq!(got.mode, CoordConnectionMode::Isolated);
        assert_eq!(got.base, None);
        assert_eq!(
            got.source, "unknown_tier_prod_default",
            "the isolated reason must distinguish an unreadable settings.json              from a runner that simply has no account"
        );
    }

    /// An explicit `COORD_HTTP_URL` reports CONNECTED with source `env`, on a
    /// tier that would otherwise isolate.
    #[test]
    fn coord_mode_explicit_env_is_connected() {
        let _g = env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(COORD_ENV_KEYS);
        let dir = tempfile::tempdir().unwrap();
        isolate_coord_env(dir.path(), Some(r#"{"tier":"local"}"#));
        std::env::set_var("COORD_HTTP_URL", "https://explicit.example/");
        let got = coord_mode();
        assert_eq!(got.mode, CoordConnectionMode::Connected);
        assert_eq!(got.base.as_deref(), Some("https://explicit.example"));
        assert_eq!(got.source, "env");
    }

    /// The report must never disagree with the resolver every coord call site
    /// actually uses. Both read the same policy, so this pins the projection
    /// across every tier the policy distinguishes.
    #[test]
    fn coord_mode_agrees_with_connected_coord_base_on_every_tier() {
        let _g = env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(COORD_ENV_KEYS);
        for settings in [
            None,
            Some(format!(r#"{{"tier":"{QONTINUI_ACCOUNT_TIER}"}}"#)),
            Some(r#"{"tier":"local"}"#.to_string()),
            Some(r#"{"tier":"local_provider"}"#.to_string()),
            Some(r#"{"tier":"something_new"}"#.to_string()),
            Some("{not json".to_string()),
        ] {
            let dir = tempfile::tempdir().unwrap();
            isolate_coord_env(dir.path(), settings.as_deref());
            let expected = connected_coord_base();
            let got = coord_mode();
            assert_eq!(got.base, expected, "settings {settings:?}");
            assert_eq!(
                got.mode == CoordConnectionMode::Connected,
                expected.is_some(),
                "settings {settings:?}"
            );
        }
    }

    /// The wire strings the frontend switches on. A rename here silently
    /// breaks a `mode === "connected"` check in TypeScript, which no Rust test
    /// would otherwise catch.
    #[test]
    fn coord_mode_serializes_to_the_documented_wire_strings() {
        let report = CoordModeReport {
            mode: CoordConnectionMode::Isolated,
            base: None,
            source: "dev_localhost_fallback".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            serde_json::json!({
                "mode": "isolated",
                "base": null,
                "source": "dev_localhost_fallback",
            })
        );
        assert_eq!(
            serde_json::to_value(CoordConnectionMode::Connected).unwrap(),
            serde_json::json!("connected")
        );
        // `as_str` and the serde representation must not drift apart.
        for m in [
            CoordConnectionMode::Connected,
            CoordConnectionMode::Isolated,
        ] {
            assert_eq!(
                serde_json::to_value(m).unwrap(),
                serde_json::Value::String(m.as_str().to_string())
            );
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
