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

/// The production `qontinui-web` FastAPI backend FQDN — where `/api/v1/*`
/// actually lives (the `qontinui.io` frontend host has no such routes).
/// Duplicated from `api_config::PROD_API_BASE_URL` rather than imported:
/// that module reaches into `crate::settings`/`crate::mcp`, which exist
/// only in the `main` binary's module tree, not this lib crate's — so it
/// isn't reachable from lib-crate bin targets like `qontinui_profile`
/// without a larger refactor out of scope here. Keep these two values in
/// sync by hand if `api.qontinui.io` ever changes. Fleet-join, 2026-08-24.
pub const PROD_API_BASE_URL: &str = "https://api.qontinui.io";

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
    // The arm is bound and dropped in the open rather than hidden behind a
    // `.0`: this fn's whole contract is "no policy applied", so a caller who
    // wants to know WHICH configured arm matched is asking a different question
    // and must call `resolve_coord_base_with_source` (or `coord_base_policy`)
    // directly. See the coord_mcp.rs note on why a `.0`-only wrapper is a
    // discard layer rather than a convenience.
    let (base, _configured_source) = resolve_coord_base_with_source();
    base
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

/// Which arm produced the lib-side `settings.json` path.
///
/// The house `(value, source)` shape (`CoordBaseSource`,
/// `api_config::ApiBaseUrlArm`) applied to this reader, so the config report's
/// layer 3 can ASK where the path came from rather than re-deriving the
/// override rule — the second copy of a precedence rule being the thing the
/// whole report is built to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsJsonPathSource {
    /// Env `QONTINUI_CONFIG_DIR` was set to a NON-EMPTY value and was used as
    /// the directory.
    ///
    /// **The emptiness filter is shared with the bin reader, and that is the
    /// point.** `settings::resolve_config_dir_from` — the BIN resolver of the
    /// same file — discards an exported-but-empty `QONTINUI_CONFIG_DIR`
    /// (`.filter(|s| !s.is_empty())`), because an exported-but-empty variable
    /// is how a shell communicates absence (the same rule
    /// `api_config::resolve_api_base_url` and `external_volume` apply). This
    /// resolver used to honour the variable whatever it contained, which sent
    /// it to a CWD-relative `settings.json` while the bin went to the platform
    /// config dir: two readers, same variable, different files.
    ///
    /// That fork stopped being merely a reporting curiosity when this module
    /// gained a tier WRITER ([`promote_tier_to_account`]): with an empty
    /// `QONTINUI_CONFIG_DIR` exported, `qontinui_profile device pair` would
    /// `create_dir_all` and write `./settings.json` into the operator's CWD,
    /// print "runner tier promoted to qontinui_account", and leave the tier the
    /// runner actually reads untouched — a success message for a write that
    /// landed nowhere anyone reads. One rule now, for both.
    ///
    /// `config_report`'s layers 2 and 3 still print both resolvers' rows: they
    /// are two independent code paths over one variable, and the report's job
    /// is to show that they agree rather than to assume it.
    EnvConfigDir,
    /// No `QONTINUI_CONFIG_DIR`; the platform config dir +
    /// `com.qontinui.runner` was used.
    PlatformConfigDir,
    /// Neither available — `dirs::config_dir()` returned `None`, so there is no
    /// path at all. The value is `None`, never a guess.
    Unresolvable,
}

impl SettingsJsonPathSource {
    /// Stable wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            SettingsJsonPathSource::EnvConfigDir => "env:QONTINUI_CONFIG_DIR",
            SettingsJsonPathSource::PlatformConfigDir => "platform_config_dir",
            SettingsJsonPathSource::Unresolvable => "unresolvable",
        }
    }
}

impl std::fmt::Display for SettingsJsonPathSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Path of the runner's `settings.json` (a non-empty `QONTINUI_CONFIG_DIR`
/// override → platform config dir + `com.qontinui.runner`), plus WHICH arm
/// produced it.
///
/// The same file `settings::load_settings()` reads, by the same rule — an
/// exported-but-empty `QONTINUI_CONFIG_DIR` is "unset" here exactly as it is in
/// `settings::resolve_config_dir_from`. See
/// [`SettingsJsonPathSource::EnvConfigDir`] for why that has to hold now that
/// this module WRITES the file as well as reading it.
pub fn settings_json_path() -> (Option<PathBuf>, SettingsJsonPathSource) {
    if let Some(dir) = std::env::var("QONTINUI_CONFIG_DIR")
        .ok()
        .filter(|s| !s.is_empty())
    {
        return (
            Some(PathBuf::from(dir).join("settings.json")),
            SettingsJsonPathSource::EnvConfigDir,
        );
    }
    match dirs::config_dir() {
        Some(d) => (
            Some(d.join("com.qontinui.runner").join("settings.json")),
            SettingsJsonPathSource::PlatformConfigDir,
        ),
        None => (None, SettingsJsonPathSource::Unresolvable),
    }
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
    /// settings.json parsed (or was simply absent) and carries no usable
    /// `tier`, and NONE of the signals [`read_runner_tier_at`] consults
    /// inferred one — neither `web_integration.runner_token` nor a device
    /// pairing (`paired_user.json`). A genuinely tier-less install.
    ///
    /// Whether `QONTINUI_SERVER_MODE` is on that list depends on WHICH
    /// question was asked: [`read_runner_tier`] (this process's tier) consults
    /// it, [`read_runner_tier_from_document`] (what the document says) does
    /// not. See [`TierSignals::server_mode`].
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

// ---------------------------------------------------------------------------
// Runner-tier INFERENCE — the ONE rule, shared by both readers.
//
// There used to be two. `settings::migrate_tier_in_place` (the runner bin) is
// the copy `require_tier_2()` sees through `load_settings`; `read_runner_tier`
// below is the copy `coord_doctor` consults, and it carried a hand-mirrored
// duplicate of the `runner_token` rule under the comment "mirror
// `settings::migrate_tier_in_place`'s inference". A rule that must be mirrored
// by hand is a rule that WILL drift — and it had: only one of the two ever
// learned about `QONTINUI_SERVER_MODE`.
//
// So the rule lands here, in the lib, beside the reader and the writer, and
// `settings::migrate_tier_in_place` calls it. This module's own doc gives the
// argument: one module => one schema => writer and reader cannot drift.
//
// The inference is PURE — every signal arrives as a parameter, no env reads
// and no I/O — so both call sites and the tests can drive every combination.
// The probes live in thin wrappers at each call site.
// ---------------------------------------------------------------------------

/// The `settings.json::tier` value for Tier 0. Its counterpart is
/// [`QONTINUI_ACCOUNT_TIER`]; there is deliberately no constant for
/// `local_provider`, which no inference can ever produce (see
/// [`tier_is_open_to_inference`]).
pub const LOCAL_TIER: &str = "local";

/// Everything the runner-tier inference is allowed to look at.
///
/// A struct rather than three positional `bool`s because the call sites read
/// `TierSignals { paired, .. }` instead of `infer_tier(false, true, false)`,
/// and because adding a fourth signal then lands as one named field rather
/// than a fourth silent argument at every call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TierSignals {
    /// `web_integration.runner_token` is present and non-empty.
    ///
    /// **Legacy.** `server_mode/mod.rs` records that the WS relay "no longer
    /// consults" this field and authenticates with the device JWT from
    /// `AuthManager` instead; it is retained only so legacy installs
    /// round-trip it. Kept as a signal because a non-empty token still proves
    /// the operator once signed into Qontinui on this box.
    pub has_runner_token: bool,
    /// `QONTINUI_SERVER_MODE` — this runner was launched headless, and a
    /// headless runner exists to be driven over the network.
    ///
    /// **A per-process launch property, not a fact about the document**, and
    /// the two consequences of that are both load-bearing:
    ///
    /// 1. **It is never persisted.** A promotion whose ONLY firing signal is
    ///    `server_mode` is applied in memory for the life of the process and
    ///    rolled back out of anything written to disk — see
    ///    `settings::TierMigration::ProcessLocal` and
    ///    `settings::document_to_persist`. Persisting it would let one headless
    ///    launch permanently flip the `settings.json` a desktop primary reads,
    ///    and would clobber the documented `QONTINUI_RUNNER_TIER=local` opt-out
    ///    on disk even while honouring it in memory.
    /// 2. **Only a process-scoped reader may set it.** Whether it is `true`
    ///    depends on who is asking: [`read_runner_tier`] (this process's tier)
    ///    passes [`crate::instance_env::server_mode`], while
    ///    [`read_runner_tier_from_document`] (what the settings DOCUMENT says —
    ///    the reader `coord_doctor` uses) passes `false`, because the doctor's
    ///    own env says nothing about how the runner was launched.
    ///
    /// Pinned by `tier_matrix_tests`'
    /// `server_mode_is_a_process_fact_not_a_document_fact` and
    /// `the_headless_default_is_never_persisted`.
    pub server_mode: bool,
    /// A `paired_user.json` binding is on disk — [`crate::pair::device_is_paired`].
    ///
    /// Unlike `server_mode` this IS a disk fact, so both readers can and do
    /// see it. A paired device is bound to a Qontinui account, which is what
    /// Tier 2 *means*.
    pub paired: bool,
}

/// What the inference resolved to. Only two arms: `local_provider` (Tier 1) is
/// unreachable by inference — it exists only as an explicit operator choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferredTier {
    /// Tier 0 — no signal fired.
    Local,
    /// Tier 2 — at least one account-binding signal fired.
    QontinuiAccount,
}

impl InferredTier {
    /// The serde snake_case `settings.json::tier` string.
    pub fn as_str(self) -> &'static str {
        match self {
            InferredTier::Local => LOCAL_TIER,
            InferredTier::QontinuiAccount => QONTINUI_ACCOUNT_TIER,
        }
    }
}

/// The runner-tier inference: which tier an install resolves to when it has
/// not been given one explicitly.
///
/// Any ONE of these lands the install in Tier 2 — the tier that is allowed to
/// talk to coord:
///
/// 1. [`TierSignals::has_runner_token`] — the operator signed into Qontinui on
///    this box before (legacy, but still proof of an account).
/// 2. [`TierSignals::paired`] — this device holds a Qontinui account binding.
///    A redeemed pairing IS a cloud-account bind, so this is not merely *a*
///    better signal than the legacy token: it is the signal the rest of the
///    system already runs on.
/// 3. [`TierSignals::server_mode`] — launched headless, i.e. to be driven over
///    the network, which requires the tier that talks to coord.
///
/// Otherwise Tier 0.
///
/// # This can only ever promote
///
/// The function has no way to express "demote": its callers apply it only
/// where the persisted tier is absent or `local` (see
/// [`tier_is_open_to_inference`]), and its non-`Local` arm is the single value
/// [`QONTINUI_ACCOUNT_TIER`]. Silent demotion of a working primary is the top
/// risk in this area and a known historical failure mode, so the shape of the
/// rule rules it out rather than a guard catching it.
pub fn infer_tier(signals: TierSignals) -> InferredTier {
    if signals.has_runner_token || signals.paired || signals.server_mode {
        InferredTier::QontinuiAccount
    } else {
        InferredTier::Local
    }
}

/// Is an install's persisted tier still open to (re-)inference?
///
/// `persisted_tier` is the `settings.json::tier` string, or `None` when the
/// document has no tier at all (a pre-tier install, or one that has never
/// completed a load). `chosen_explicitly` is `settings.json::tier_chosen_explicitly`.
///
/// # Why this is not simply "have we inferred once already?"
///
/// It used to be. `settings::migrate_tier_in_place` was gated on
/// `tier_initialized`, a ONE-SHOT latch — so a box that first booted before it
/// was paired was stuck at `Local` permanently, and the only way out was a
/// button in a WebView the headless box does not have. Re-running the
/// inference when the signals change is the fix.
///
/// But the latch was load-bearing for a second reason: it stopped the
/// inference from fighting an operator who deliberately chose Tier 0. So the
/// unlatch has to distinguish **"never chosen"** from **"chosen as Local"**,
/// and those two were genuinely indistinguishable — the inference and
/// `commands::auth::set_runner_tier` both wrote the same
/// `tier_initialized = true`. Hence `tier_chosen_explicitly`, written by
/// `set_runner_tier` and by nothing else, `#[serde(default)]` so every
/// existing install reads as "never chosen" and is eligible.
///
/// # The three arms
///
/// - `chosen_explicitly` ⇒ **closed**. The operator's word is final.
/// - no tier / `local` ⇒ **open**. `Local` is exactly the value the inference
///   itself produces, so on a document with no explicit choice recorded it is
///   evidence of nothing.
/// - any other tier ⇒ **closed**. `qontinui_account` has nowhere to be
///   promoted to, and `local_provider` is unreachable by inference — the sole
///   writer of Tier 1 is `set_runner_tier`, so finding it on disk IS an
///   explicit choice, recorded before the field existed to say so. That is a
///   deduction from the writer set, not a guess at intent: reading *`Local`*
///   as a choice would be the guess, and it is the one this function refuses
///   to make.
pub fn tier_is_open_to_inference(persisted_tier: Option<&str>, chosen_explicitly: bool) -> bool {
    if chosen_explicitly {
        return false;
    }
    match persisted_tier.map(str::trim) {
        None | Some("") => true,
        Some(t) => t == LOCAL_TIER,
    }
}

/// Does a settings document written BEFORE `tier_chosen_explicitly` existed
/// nonetheless PROVE that a human chose its tier?
///
/// # Why a back-fill is needed at all
///
/// `tier_chosen_explicitly` is `#[serde(default)]`, so every pre-Phase-3
/// document reads "never chose". For the PAIRING signal that ambiguity is
/// genuine and acceptable — a paired box that reads `local` really might have
/// been latched there by the old one-shot inference. For the legacy
/// `runner_token` signal it is not, and taking it at face value is a straight
/// regression: it silently re-promotes a box whose operator deliberately opted
/// out, which is the subtlest failure this whole area has.
///
/// # The deduction
///
/// Enumerate the pre-Phase-3 writers of `settings.json::tier`:
///
/// - the old one-shot inference (`migrate_tier_in_place`): a non-empty
///   `web_integration.runner_token` ⇒ `qontinui_account`, otherwise `local`;
/// - `commands::auth::finalize_signed_in` and `redeem_pair_code`: they write
///   `qontinui_account` and nothing else;
/// - `commands::auth::set_runner_tier`: the operator's own choice, any tier.
///
/// So a document carrying `tier_initialized = true`, `tier = "local"` **and** a
/// non-empty `runner_token` cannot have come from any automatic writer — the
/// inference would have produced `qontinui_account` from that very token. Only
/// `set_runner_tier` could have written it. That is the case this closes: the
/// operator who signed in, then opened the SetupWizard and picked Local to stop
/// the cloud round-trips.
///
/// # What it deliberately does NOT deduce
///
/// - **`qontinui_account`.** The reverse asymmetry does not hold: `redeem_pair_code`
///   and `finalize_signed_in` write that value automatically, so finding it
///   proves nothing about a human. It needs no back-fill anyway —
///   [`tier_is_open_to_inference`] already closes on it, and there is nothing
///   above Tier 2 to promote to.
/// - **`local_provider`.** Only `set_runner_tier` writes it, so it IS an
///   explicit choice — but it is already closed by
///   [`tier_is_open_to_inference`] with that argument stated, and duplicating
///   the deduction here would put the same rule in two places.
/// - **An uninitialized document.** `save_settings` serializes the whole
///   struct, so `tier` is present in every file the runner ever wrote; with
///   `tier_initialized == false` its value is just the struct's `#[default]`
///   and carries no decision.
///
/// # The one corner where this over-reads, and why that is the safe direction
///
/// `local` + a token is also reachable without a choice: boot once with no
/// token (the inference latches `local`), then persist a `runner_token` through
/// a Save that does not promote the tier (an incomplete web-integration
/// config). That box reads as "chose Local" here and will not be auto-promoted.
/// The alternative error is to bring an opted-out box online with the cloud
/// tier — a product-posture change made silently — so this is the conservative
/// side, and it is no longer a dead end: `qontinui_profile tier --clear-choice`
/// re-opens the install to inference from a headless box, and the SetupWizard's
/// tier step does it in the app.
pub fn legacy_tier_choice_is_deducible(
    tier_initialized: bool,
    persisted_tier: Option<&str>,
    has_runner_token: bool,
) -> bool {
    tier_initialized && has_runner_token && persisted_tier.map(str::trim) == Some(LOCAL_TIER)
}

/// The persisted runner tier as the serde snake_case string
/// (`"local"` | `"local_provider"` | `"qontinui_account"`).
///
/// Reads the JSON directly rather than importing `Settings` because the
/// `Settings` struct is a main-binary module (not in lib.rs). Errors are
/// PRESERVED as [`TierRead::Unknown`] — see the type docs for why.
///
/// This is the **process** reader: *what tier is THIS process running at?* It
/// therefore consults [`crate::instance_env::server_mode`], exactly as
/// `settings::load_settings_full` does in the runner bin, so the two in-process
/// answers agree. Its counterpart is [`read_runner_tier_from_document`].
///
/// # Why the two must not be the same function
///
/// They answer different questions, and collapsing them broke a real
/// configuration. The supervisor spawns a NAMED headless secondary with
/// `QONTINUI_SERVER_MODE=1` and `QONTINUI_INSTANCE_NAME` but no
/// `QONTINUI_RUNNER_TIER` (that is gated on `is_temp_runner` — see
/// `qontinui-supervisor/src/process/env_forwarders.rs`). In that one process,
/// `settings::load_settings()` resolved `QontinuiAccount` (so `require_tier_2`
/// passed and the relay ran) while a hardcoded `server_mode: false` here
/// resolved `Absent` — which [`apply_tier_policy`] turns into `DevLocalhost`,
/// so [`connected_coord_base`] returned `None` and every coord consumer in that
/// same runner saw "no coord" on a runner that believed it was Tier 2.
///
/// The pairing probe ([`crate::pair::device_is_paired`]) and the server-mode
/// probe are taken here, in the env-facing wrapper, so [`read_runner_tier_at`]
/// stays hermetic.
pub fn read_runner_tier() -> TierRead {
    read_runner_tier_resolved(crate::instance_env::server_mode())
}

/// The **document** reader: *what does this settings document say?* —
/// deliberately blind to `QONTINUI_SERVER_MODE`, which is a property of a
/// RUNNING runner's process and not of the file.
///
/// This is the reader `coord_doctor` consults. The doctor may be a separate
/// process (the standalone `coord_doctor` bin) whose environment says nothing
/// about how any runner was launched, so consulting its own
/// `QONTINUI_SERVER_MODE` would be reporting the diagnostician's shell as if it
/// were the patient's state. The tier check's message says so in as many words,
/// and that promise is kept HERE rather than by a comment.
pub fn read_runner_tier_from_document() -> TierRead {
    read_runner_tier_resolved(/* server_mode = */ false)
}

/// Shared body of [`read_runner_tier`] / [`read_runner_tier_from_document`]:
/// resolve the path, take the pairing probe, apply the injected `server_mode`.
fn read_runner_tier_resolved(server_mode: bool) -> TierRead {
    let (path, _path_source) = settings_json_path();
    let Some(path) = path else {
        return TierRead::Unknown("cannot resolve settings.json path".to_string());
    };
    read_runner_tier_at(&path, crate::pair::device_is_paired(), server_mode)
}

/// Path-parameterized core of [`read_runner_tier`] /
/// [`read_runner_tier_from_document`] — the reader half of the pair whose
/// writer is [`promote_tier_to_account_at`]. Split for the same reason
/// [`ensure_coord_url_at`] is: hermetic tests against a temp file, no process
/// env. `paired` and `server_mode` are injected for the same reason — and
/// `server_mode` additionally because its value is the ONE thing the two
/// wrappers differ on (see [`TierSignals::server_mode`]).
///
/// It applies the SAME [`infer_tier`] / [`tier_is_open_to_inference`] rule that
/// `settings::migrate_tier_in_place` applies in the runner bin — one rule, two
/// call sites, no hand-mirrored duplicate. Concretely, a paired box whose
/// `settings.json` still reads `tier: "local"` (the latched box the headless
/// defect actually produces) reads back as `qontinui_account` here, which is
/// what makes the doctor's tier check agree with the runner's own gate.
pub fn read_runner_tier_at(path: &std::path::Path, paired: bool, server_mode: bool) -> TierRead {
    // An ABSENT settings.json is a document with no tier, not a different kind
    // of fact — so it goes through the same inference as a present-but-tierless
    // one, from an empty object. Otherwise a paired box that had never written
    // its settings (the fresh headless install, exactly the case
    // `promote_tier_to_account_at` creates a file for) would read `Absent` here
    // while the runner itself resolved Tier 2 — the two readers disagreeing
    // again, in the one place the rule is supposed to be shared.
    let json: serde_json::Value = if path.exists() {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                return TierRead::Unknown(format!("read {} failed: {e}", path.display()));
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(j) => j,
            Err(e) => {
                return TierRead::Unknown(format!("parse {} failed: {e}", path.display()));
            }
        }
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };
    let persisted = json.get("tier").and_then(|v| v.as_str());
    let has_runner_token = json
        .get("web_integration")
        .and_then(|w| w.get("runner_token"))
        .and_then(|v| v.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    // PRESENT-and-false is not the same document as ABSENT, and only the raw
    // tree can tell them apart — which is why this reader parses to a `Value`
    // rather than reusing a typed struct with `#[serde(default)]`. An absent
    // key means the document predates the field, so the choice is DEDUCED from
    // what the old writers could have produced; a present one is read as
    // written. (A present non-bool is malformed, not a choice.) The runner
    // bin's twin of this is `settings::migrate_tier_chosen_explicitly`, which
    // reaches the same fact from the raw `Value` it already parses.
    let chosen_explicitly = match json.get("tier_chosen_explicitly") {
        Some(v) => v.as_bool().unwrap_or(false),
        None => legacy_tier_choice_is_deducible(
            json.get("tier_initialized")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            persisted,
            has_runner_token,
        ),
    };
    let signals = TierSignals {
        has_runner_token,
        paired,
        server_mode,
    };

    if tier_is_open_to_inference(persisted, chosen_explicitly)
        && infer_tier(signals) == InferredTier::QontinuiAccount
    {
        return TierRead::Known(QONTINUI_ACCOUNT_TIER.to_string());
    }
    match persisted.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => TierRead::Known(t.to_string()),
        // Parsed fine, carries no tier, and nothing infers one: a genuinely
        // tier-less install. NOT `Known("local")` — see [`TierRead`].
        None => TierRead::Absent,
    }
}

// ---------------------------------------------------------------------------
// Runner-tier WRITER — the ONE tier-writing path.
//
// Deliberately beside [`read_runner_tier`]: one module ⇒ one schema ⇒ writer
// and reader cannot drift. This is the module doc's own argument applied to the
// half that was missing.
//
// It lives in the LIB rather than in `settings.rs` because `settings` is
// declared in `main.rs` — the runner BIN's module tree — and the headless doors
// (`bin/qontinui_profile.rs`) are a second bin that links only this lib. So
// `settings.rs` is literally unreachable from the door that most needs to
// promote the tier, which is why the two pairing doors disagreed in the first
// place: `redeem_pair_code` (WebView-only) promoted, `qontinui_profile device
// pair` (headless) did not. Both now call this.
// ---------------------------------------------------------------------------

/// What a tier write actually did. Every arm is a normal outcome — the callers
/// are best-effort and log rather than fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierWrite {
    /// `settings.json` was rewritten. Every key the edit did not name rode
    /// along untouched.
    Written,
    /// Every key the edit names already held the target value — no write at
    /// all, the file is byte-identical.
    Unchanged,
    /// This runner is a SECONDARY instance, so nothing was read or written.
    /// See [`crate::instance_env::is_secondary`] and the caller note on
    /// [`apply_tier_edit_at`].
    SkippedSecondary,
}

impl TierWrite {
    /// Stable wire/log string.
    pub fn as_str(self) -> &'static str {
        match self {
            TierWrite::Written => "written",
            TierWrite::Unchanged => "unchanged",
            TierWrite::SkippedSecondary => "skipped_secondary",
        }
    }
}

impl std::fmt::Display for TierWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The tier-owned keys a write may touch. `None` means "leave whatever is
/// there" — an edit names only the keys it is responsible for, so no caller can
/// clear a key it never meant to think about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TierEdit {
    /// `settings.json::tier`.
    pub tier: Option<String>,
    /// `settings.json::tier_initialized`.
    pub tier_initialized: Option<bool>,
    /// `settings.json::tier_chosen_explicitly` — see
    /// [`tier_is_open_to_inference`]. **A promotion must leave this `None`**:
    /// an inference has no business claiming a human chose.
    pub tier_chosen_explicitly: Option<bool>,
}

/// Persist `tier = "qontinui_account"` (+ `tier_initialized = true`) into the
/// runner's `settings.json`, returning the outcome and the path it resolved.
///
/// Called by BOTH doors that redeem a device pairing — `redeem_pair_code`
/// (the Tauri command) and `qontinui_profile device pair` (the headless CLI) —
/// because redeeming a pair code IS a cloud-account bind, and tier 2 is the
/// tier that is allowed to talk to coord.
///
/// **Promotes only, never demotes**: the sole value this function can write is
/// [`QONTINUI_ACCOUNT_TIER`]. **And it is a promotion, not a choice** — it does
/// not touch `tier_chosen_explicitly`, which only a human picking a tier may
/// set. Pinned by `promote_never_records_an_explicit_choice`.
///
/// The path comes back so a caller can NAME the file it wrote; a bare "promoted"
/// message is unfalsifiable, and this writer resolves its path from the process
/// env.
pub fn promote_tier_to_account() -> Result<(TierWrite, PathBuf)> {
    let (path, source) = settings_json_path();
    let path =
        path.ok_or_else(|| anyhow!("cannot resolve settings.json path (source: {source})"))?;
    let outcome = promote_tier_to_account_at(&path, crate::instance_env::is_secondary())?;
    Ok((outcome, path))
}

/// Path-parameterized core of [`promote_tier_to_account`] (hermetic tests point
/// it at a temp file and inject the predicate, so they never touch process env).
pub fn promote_tier_to_account_at(path: &std::path::Path, is_secondary: bool) -> Result<TierWrite> {
    apply_tier_edit_at(
        path,
        is_secondary,
        TierEdit {
            tier: Some(QONTINUI_ACCOUNT_TIER.to_string()),
            tier_initialized: Some(true),
            // NOT a choice. See [`TierEdit::tier_chosen_explicitly`].
            tier_chosen_explicitly: None,
        },
    )
}

/// Record the operator's EXPLICIT tier choice: `tier`, `tier_initialized` and
/// `tier_chosen_explicitly = true` — the headless equivalent of the
/// SetupWizard's TierStep (`commands::auth::set_runner_tier`), reached from
/// `qontinui_profile tier --set`.
///
/// Unlike [`promote_tier_to_account`] this CAN write a lower tier, because a
/// human said so. That is the only kind of demotion this module permits: the
/// inference itself has no arm that can express one.
pub fn set_tier_choice_at(
    path: &std::path::Path,
    is_secondary: bool,
    tier: &str,
) -> Result<TierWrite> {
    if !TIER_VALUES.contains(&tier) {
        return Err(anyhow!(
            "invalid tier {tier:?} — expected one of {}",
            TIER_VALUES.join(" | ")
        ));
    }
    apply_tier_edit_at(
        path,
        is_secondary,
        TierEdit {
            tier: Some(tier.to_string()),
            tier_initialized: Some(true),
            tier_chosen_explicitly: Some(true),
        },
    )
}

/// Clear `tier_chosen_explicitly`, re-opening the install to
/// [`infer_tier`] — the door `coord_doctor`'s "credentialed but not authorized"
/// remediation names, reached from `qontinui_profile tier --clear-choice`.
///
/// It deliberately leaves `tier` alone. Clearing the FLAG is not a tier change:
/// on the next settings load the inference re-runs and, if a signal fires,
/// promotes — and if none fires the box stays exactly where it is. Writing a
/// tier here as well would make "un-pin me" and "set me to Tier 2" the same
/// button, which is how the pin got there in the first place.
pub fn clear_tier_choice_at(path: &std::path::Path, is_secondary: bool) -> Result<TierWrite> {
    apply_tier_edit_at(
        path,
        is_secondary,
        TierEdit {
            tier: None,
            tier_initialized: None,
            tier_chosen_explicitly: Some(false),
        },
    )
}

/// Every value `settings.json::tier` may legally carry (the serde snake_case
/// spelling of `settings::RunnerTier`). Named here because the lib has no
/// `RunnerTier` — see [`apply_tier_edit_at`] on why there is no typed
/// round-trip.
pub const TIER_VALUES: &[&str] = &[LOCAL_TIER, "local_provider", QONTINUI_ACCOUNT_TIER];

/// Apply a [`TierEdit`] to `settings.json` as a `serde_json::Value`-tree edit.
///
/// Honours all three conditions of the runner bin's own persist guard,
/// `settings::should_persist_migration(needs_persist, is_secondary, provenance)`:
///
/// 1. **Something to persist.** A file where every named key already holds the
///    target value is left BYTE-IDENTICAL ([`TierWrite::Unchanged`]).
/// 2. **`!is_secondary`.** A supervisor-launched runner carrying
///    `QONTINUI_INSTANCE_NAME` must never write the shared `settings.json`:
///    `settings::migrate_tier_in_place` infers `Local` for it (no
///    `runner_token`), so a secondary write silently DEMOTES the primary on
///    disk — the FOOTGUN GUARD in `settings::load_settings_full`. Note the
///    nuance that guard documents: the path IS instance-scoped when
///    `QONTINUI_CONFIG_DIR` is set, so the hazard is specifically a secondary
///    with `QONTINUI_INSTANCE_NAME` and no `QONTINUI_CONFIG_DIR`. The
///    predicate stays the conservative one anyway. Checked FIRST, before any
///    I/O. Callers that can (the runner bin) should apply an in-memory-only
///    tier overlay on this arm instead.
/// 3. **Authoritative source.** Satisfied STRUCTURALLY by the `serde_json::Value`
///    edit, exactly as [`ensure_coord_url_at`] does it: an unparseable
///    `settings.json` is an `Err` ("refusing to overwrite"), never an
///    all-defaults clobber. The lib has no `Settings` struct and must not
///    synthesize one — a typed round-trip would silently drop every key the lib
///    does not model.
///
/// The write itself goes through [`crate::fs_atomic::atomic_write`], the same
/// writer `settings::save_settings` uses, because `settings.json` has REAL
/// concurrent readers: the runner's relay loop re-reads it every iteration, and
/// a truncating write races it into a partial parse — which the reader reports
/// as [`TierRead::Unknown`], which [`apply_tier_policy`] turns into
/// `UnknownTierProdDefault`. A tier writer that can make the tier unreadable is
/// the failure this whole module exists to prevent.
///
/// An ABSENT `settings.json` is created carrying just the named keys. That is
/// the fresh headless box: pairing before the runner has ever written its
/// settings, which is precisely the case that would otherwise be latched at
/// `Local` forever by the one-shot `migrate_tier_in_place`. Every remaining
/// field comes from its serde default on the next load (pinned by
/// `settings::minimal_promoted_settings_json_parses`).
///
/// # In-process callers must drop the settings parse cache
///
/// The runner bin caches its parse of `settings.json`
/// (`settings::SETTINGS_CACHE`, validated on mtime+size). This writer cannot
/// invalidate it — it is in the lib, and the cache is bin-side — so the bin
/// wraps it: `settings::promote_tier_to_account` calls this and then drops the
/// cache. Bin code calls the wrapper, never this function directly.
pub fn apply_tier_edit_at(
    path: &std::path::Path,
    is_secondary: bool,
    edit: TierEdit,
) -> Result<TierWrite> {
    use serde_json::{Map, Value};

    // Condition 2 — before any I/O at all.
    if is_secondary {
        return Ok(TierWrite::SkippedSecondary);
    }

    /// The edit as `(key, value)` pairs, in a stable order.
    fn pairs(edit: &TierEdit) -> Vec<(&'static str, Value)> {
        let mut out: Vec<(&'static str, Value)> = Vec::with_capacity(3);
        if let Some(t) = &edit.tier {
            out.push(("tier", Value::String(t.clone())));
        }
        if let Some(b) = edit.tier_initialized {
            out.push(("tier_initialized", Value::Bool(b)));
        }
        if let Some(b) = edit.tier_chosen_explicitly {
            out.push(("tier_chosen_explicitly", Value::Bool(b)));
        }
        out
    }
    let pairs = pairs(&edit);
    if pairs.is_empty() {
        return Ok(TierWrite::Unchanged);
    }

    if !path.exists() {
        let mut root = Map::new();
        for (k, v) in pairs {
            root.insert(k.to_string(), v);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut bytes = serde_json::to_vec_pretty(&Value::Object(root))?;
        bytes.push(b'\n');
        crate::fs_atomic::atomic_write(path, &bytes)
            .with_context(|| format!("writing {}", path.display()))?;
        info!("apply_tier_edit: created {} with {edit:?}", path.display());
        return Ok(TierWrite::Written);
    }

    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    // Condition 3: a document we cannot parse is NOT authoritative state we may
    // replace with our own keys.
    let mut root: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing {} (refusing to overwrite)", path.display()))?;
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("{}: root is not a JSON object", path.display()))?;

    // Condition 1: nothing to persist ⇒ no write at all (byte-identical file).
    if pairs.iter().all(|(k, v)| root_obj.get(*k) == Some(v)) {
        return Ok(TierWrite::Unchanged);
    }

    // Insert ONLY the named keys; every sibling — known or unknown — rides
    // along in the Value tree untouched.
    for (k, v) in pairs {
        root_obj.insert(k.to_string(), v);
    }
    let mut out = serde_json::to_vec_pretty(&root)?;
    out.push(b'\n');
    crate::fs_atomic::atomic_write(path, &out)
        .with_context(|| format!("writing {}", path.display()))?;
    info!("apply_tier_edit: applied {edit:?} to {}", path.display());
    Ok(TierWrite::Written)
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
        // Silence here is DELIBERATE, and it is not an oversight to be fixed by
        // adding two more `call_once` lines.
        //
        // The three arms above log because each is an ANOMALY: nothing was
        // configured and the runner had to guess. `Env` and `Profile` are the
        // arms that fire on a correctly-configured machine — every healthy
        // runner in the fleet takes one of them on every call — so a log line
        // here would be pure noise on exactly the machines that have nothing
        // wrong with them, while still answering the operator's real question
        // ("which arm won?") only for whoever happens to be reading the log at
        // the moment the process first resolved it.
        //
        // Attribution is the REPORT's job, not the log's. `config_report`'s
        // layer 4 asks this function directly and prints
        // `CoordBaseSource::as_str()` for whichever arm won, on demand, at a
        // stamped instant — which is a strictly better answer than a one-shot
        // line buried in a rotating log, and it covers these two arms with no
        // logging at all. See
        // `2026-08-20-effective-config-provenance-and-env-generation` Phase 2.
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

/// Every process-env variable that can change what [`connected_coord_base`]
/// answers — the module's whole env surface, in one place.
///
/// This is NOT test scaffolding that leaked: it is a declaration, and it exists
/// because the answer to "what must a test isolate to pin a tier?" was
/// maintained by hand in three separate test binaries and drifted the moment
/// `QONTINUI_SERVER_MODE` became a tier signal. Two of the three lists never
/// learned about it, so on a box that exports the variable a `{"tier":"local"}`
/// fixture inferred `qontinui_account` and the assertions flipped. Every test
/// that isolates this surface captures THIS list.
///
/// - `COORD_HTTP_URL` — the explicit override arm of [`resolve_coord_base`].
/// - `QONTINUI_ENV` — selects the active profile, whose `coord_url` is the
///   next arm.
/// - `QONTINUI_CONFIG_DIR` — where `settings.json` (and therefore the persisted
///   tier) is read from.
/// - `QONTINUI_SECURE_STORAGE_DIR` — where `paired_user.json` lives; pairing is
///   a tier signal ([`crate::pair::device_is_paired`]).
/// - `QONTINUI_SERVER_MODE` — [`read_runner_tier`] is the PROCESS reader, so a
///   headless launch infers Tier 2 from a document that says otherwise.
pub const COORD_BASE_ENV_KEYS: &[&str] = &[
    "COORD_HTTP_URL",
    "QONTINUI_ENV",
    "QONTINUI_CONFIG_DIR",
    "QONTINUI_SECURE_STORAGE_DIR",
    "QONTINUI_SERVER_MODE",
];

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

    /// Env vars every `connected_coord_base` test mutates — the module's own
    /// declaration of that surface, shared with the runner bin's fixtures
    /// rather than restated here. See [`super::COORD_BASE_ENV_KEYS`] for what
    /// each one does and why a second copy of this list is a bug.
    const COORD_ENV_KEYS: &[&str] = super::COORD_BASE_ENV_KEYS;

    /// Point the resolver at a hermetic config dir with nothing configured:
    /// no `COORD_HTTP_URL`, an unresolvable active profile, and `settings.json`
    /// written from `settings_json` (`None` ⇒ no file at all, i.e.
    /// [`TierRead::Absent`]).
    fn isolate_coord_env(dir: &std::path::Path, settings_json: Option<&str>) {
        std::env::remove_var("COORD_HTTP_URL");
        std::env::set_var("QONTINUI_ENV", NO_SUCH_PROFILE);
        std::env::set_var("QONTINUI_CONFIG_DIR", dir);
        // Hermetic pairing state too — an empty dir means "not paired".
        std::env::set_var("QONTINUI_SECURE_STORAGE_DIR", dir);
        // …and hermetic launch state: `read_runner_tier` asks the process env
        // whether THIS process is headless.
        std::env::remove_var("QONTINUI_SERVER_MODE");
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
        // …and the String family keeps its fail-loud posture unchanged —
        // asserted as the full (value, source) pair, because the base alone is
        // byte-identical to the genuine `tier_default` answer and only the arm
        // distinguishes "your tier says hosted" from "we could not read it".
        assert_eq!(
            coord_base_with_source(),
            (
                PROD_COORD_BASE.to_string(),
                CoordBaseSource::UnknownTierProdDefault
            )
        );
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
        assert_eq!(
            coord_base_with_source(),
            (
                DEV_LOCALHOST_COORD_BASE.to_string(),
                CoordBaseSource::DevLocalhostFallback
            )
        );
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

    // ------------------------------------------------------------------
    // The SHARED runner-tier inference — `infer_tier` +
    // `tier_is_open_to_inference`, the one rule `settings::migrate_tier_in_place`
    // and `read_runner_tier_at` both call. Pure: every signal is a parameter.
    // ------------------------------------------------------------------

    /// Any ONE account-binding signal lands the install in Tier 2, and only
    /// the empty set yields Tier 0. Exhaustive over the 2^3 combinations —
    /// there are eight, so there is no reason to sample.
    #[test]
    fn infer_tier_is_the_or_of_its_signals() {
        for (has_runner_token, server_mode, paired) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (false, false, true),
            (true, true, false),
            (true, false, true),
            (false, true, true),
            (true, true, true),
        ] {
            let got = infer_tier(TierSignals {
                has_runner_token,
                server_mode,
                paired,
            });
            let want = if has_runner_token || server_mode || paired {
                InferredTier::QontinuiAccount
            } else {
                InferredTier::Local
            };
            assert_eq!(
                got, want,
                "runner_token={has_runner_token} server_mode={server_mode} paired={paired}"
            );
        }
        assert_eq!(InferredTier::Local.as_str(), LOCAL_TIER);
        assert_eq!(
            InferredTier::QontinuiAccount.as_str(),
            QONTINUI_ACCOUNT_TIER
        );
    }

    /// The eligibility rule, stated directly. `Local` is open because it is
    /// exactly the value the inference produces; `local_provider` is closed
    /// because NO inference can produce it, so finding it on disk is evidence
    /// of an explicit choice made before the field existed to record one.
    #[test]
    fn tier_is_open_to_inference_arms() {
        // Open: nothing recorded, or an inferred Local.
        assert!(tier_is_open_to_inference(None, false));
        assert!(tier_is_open_to_inference(Some(""), false));
        assert!(tier_is_open_to_inference(Some("  "), false));
        assert!(tier_is_open_to_inference(Some(LOCAL_TIER), false));

        // Closed: the operator said so.
        assert!(!tier_is_open_to_inference(None, true));
        assert!(!tier_is_open_to_inference(Some(LOCAL_TIER), true));

        // Closed: nothing to promote to / unreachable by inference.
        assert!(!tier_is_open_to_inference(
            Some(QONTINUI_ACCOUNT_TIER),
            false
        ));
        assert!(!tier_is_open_to_inference(Some("local_provider"), false));

        // Closed: an unrecognized value is somebody else's, not ours to
        // overwrite.
        assert!(!tier_is_open_to_inference(
            Some("enterprise_someday"),
            false
        ));
    }

    // ------------------------------------------------------------------
    // read_runner_tier_at — the lib-side reader `coord_doctor` consults,
    // now sharing the inference above.
    // ------------------------------------------------------------------

    /// The live reproduction this plan was written from, in one test: a
    /// correctly-paired box whose settings.json still reads `tier: "local"`.
    /// Before Phase 3 the doctor reported `BLOCKED at: tier` on it.
    #[test]
    fn read_runner_tier_at_promotes_a_latched_local_document_when_paired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"tier":"local","tier_initialized":true}"#).unwrap();

        assert_eq!(
            read_runner_tier_at(&path, /* paired = */ false, /* server_mode = */ false),
            TierRead::Known(LOCAL_TIER.to_string())
        );
        assert_eq!(
            read_runner_tier_at(&path, /* paired = */ true, /* server_mode = */ false),
            TierRead::Known(QONTINUI_ACCOUNT_TIER.to_string())
        );
    }

    /// …and an explicit choice closes it in this reader too, from the
    /// persisted `tier_chosen_explicitly` key. Both readers must agree, or
    /// `coord doctor` and `require_tier_2()` disagree about the same box —
    /// which is the class of defect this phase exists to remove.
    #[test]
    fn read_runner_tier_at_honours_an_explicit_local_choice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"tier":"local","tier_initialized":true,"tier_chosen_explicitly":true}"#,
        )
        .unwrap();

        assert_eq!(
            read_runner_tier_at(&path, /* paired = */ true, /* server_mode = */ false),
            TierRead::Known(LOCAL_TIER.to_string()),
            "an operator who chose Tier 0 keeps it even on a paired box"
        );
    }

    /// A tier-less document + pairing is a promotion, not `Absent`; a
    /// tier-less document with no signal at all stays `Absent` — the
    /// tri-state's whole point is that "no tier" and "Tier 0" are different
    /// facts.
    #[test]
    fn read_runner_tier_at_tierless_document_follows_the_signals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"web_integration":{"runner_token":""}}"#).unwrap();

        assert_eq!(
            read_runner_tier_at(&path, /* paired = */ false, /* server_mode = */ false),
            TierRead::Absent
        );
        assert_eq!(
            read_runner_tier_at(&path, /* paired = */ true, /* server_mode = */ false),
            TierRead::Known(QONTINUI_ACCOUNT_TIER.to_string())
        );
    }

    /// The legacy signal still fires — `runner_token` is deprecated, not
    /// removed, and an install carrying one must not regress to `Absent`.
    #[test]
    fn read_runner_tier_at_legacy_runner_token_still_infers_tier_2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"web_integration":{"runner_token":"legacy"}}"#).unwrap();
        assert_eq!(
            read_runner_tier_at(&path, /* paired = */ false, /* server_mode = */ false),
            TierRead::Known(QONTINUI_ACCOUNT_TIER.to_string())
        );
    }

    /// An ABSENT settings.json is a tier-less document, not a separate fact:
    /// unpaired it is `Absent`, paired it is Tier 2. The fresh headless
    /// install pairs before the runner has ever written its settings, and the
    /// two readers must not disagree about that box either.
    #[test]
    fn read_runner_tier_at_absent_file_follows_the_signals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert!(!path.exists());

        assert_eq!(
            read_runner_tier_at(&path, /* paired = */ false, /* server_mode = */ false),
            TierRead::Absent
        );
        assert_eq!(
            read_runner_tier_at(&path, /* paired = */ true, /* server_mode = */ false),
            TierRead::Known(QONTINUI_ACCOUNT_TIER.to_string())
        );
    }

    /// NO-DOWNGRADE: an unreadable document is `Unknown`, and pairing does not
    /// license a guess over it. Absence of evidence about the tier is not
    /// evidence of a tier.
    #[test]
    fn read_runner_tier_at_unparseable_is_unknown_even_when_paired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{not json").unwrap();
        assert!(matches!(
            read_runner_tier_at(&path, /* paired = */ true, /* server_mode = */ false),
            TierRead::Unknown(_)
        ));
    }

    // ------------------------------------------------------------------
    // promote_tier_to_account_at — the ONE tier writer, shared by the
    // WebView pair door and the headless CLI pair door. Hermetic:
    // path-parameterized, `is_secondary` injected, temp dirs, no process env.
    // ------------------------------------------------------------------

    /// A box latched at Tier 0 by the one-shot `migrate_tier_in_place` is what
    /// the headless defect actually produces. Promotion must move it.
    #[test]
    fn promote_tier_local_settings_becomes_qontinui_account() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "tier": "local",
                "tier_initialized": true,
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            promote_tier_to_account_at(&path, false).unwrap(),
            TierWrite::Written
        );
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["tier"], QONTINUI_ACCOUNT_TIER);
        assert_eq!(v["tier_initialized"], true);
        // And the reader agrees with the writer — the property this module
        // exists to hold.
        assert_eq!(
            read_runner_tier_at(&path, /* paired = */ false, /* server_mode = */ false),
            TierRead::Known(QONTINUI_ACCOUNT_TIER.to_string())
        );
    }

    /// The `Value`-tree property, and the reason a typed round-trip is banned
    /// here: the lib has no `Settings` struct, so synthesizing one would
    /// silently destroy every key it does not model — which is most of them.
    #[test]
    fn promote_tier_preserves_unknown_sibling_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = serde_json::json!({
            "tier": "local",
            "tier_initialized": true,
            "local_user_id": "1f0a1c2e-0000-4000-8000-000000000001",
            "web_integration": {"runner_token": "", "enabled": true},
            "saved_projects": [{"id": "p1", "path": "/tmp/p1"}],
            "an_entirely_unmodelled_key": {"deep": [1, 2, {"three": true}]},
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&original).unwrap()).unwrap();

        assert_eq!(
            promote_tier_to_account_at(&path, false).unwrap(),
            TierWrite::Written
        );
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["tier"], QONTINUI_ACCOUNT_TIER);
        assert_eq!(v["tier_initialized"], true);
        assert_eq!(
            v["local_user_id"], "1f0a1c2e-0000-4000-8000-000000000001",
            "an unrelated key must survive the promotion"
        );
        assert_eq!(v["web_integration"]["enabled"], true);
        assert_eq!(v["saved_projects"][0]["id"], "p1");
        assert_eq!(v["an_entirely_unmodelled_key"]["deep"][2]["three"], true);
    }

    /// Condition 1 (nothing to persist) + the no-demote rule: an
    /// already-promoted file is not rewritten at all.
    #[test]
    fn promote_tier_already_account_is_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // Quirky formatting on purpose: any write at all would change it.
        let original =
            "{\"tier\":\"qontinui_account\",   \"tier_initialized\":true,\n \"extra\":[1,2]}";
        std::fs::write(&path, original).unwrap();

        assert_eq!(
            promote_tier_to_account_at(&path, false).unwrap(),
            TierWrite::Unchanged
        );
        assert_eq!(
            std::str::from_utf8(&std::fs::read(&path).unwrap()).unwrap(),
            original,
            "an already-qontinui_account settings.json must mean NO write at all"
        );
    }

    /// Condition 3 (authoritative source), structurally: an unparseable
    /// `settings.json` is refused, never replaced with our two keys.
    #[test]
    fn promote_tier_unparseable_file_is_refused_without_clobbering() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, b"{\"tier\": \"local\",").unwrap();

        let err = promote_tier_to_account_at(&path, false).unwrap_err();
        assert!(
            err.to_string().contains("refusing to overwrite"),
            "the refusal must be explicit, got: {err}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"{\"tier\": \"local\",",
            "a corrupt settings.json must be left byte-identical"
        );
    }

    /// Condition 2 (`!is_secondary`) — the single most dangerous arm. A
    /// secondary carrying `QONTINUI_INSTANCE_NAME` and no
    /// `QONTINUI_CONFIG_DIR` resolves the PRIMARY's shared settings.json, so a
    /// write here silently demotes the primary on its next load. Nothing may be
    /// written, and nothing may even be read.
    #[test]
    fn promote_tier_secondary_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = "{\"tier\":\"local\",\"tier_initialized\":true}";
        std::fs::write(&path, original).unwrap();

        assert_eq!(
            promote_tier_to_account_at(&path, true).unwrap(),
            TierWrite::SkippedSecondary
        );
        assert_eq!(
            std::str::from_utf8(&std::fs::read(&path).unwrap()).unwrap(),
            original,
            "a secondary must never write the shared settings.json"
        );

        // Same predicate on a path that does not exist: still no file created.
        let absent = dir.path().join("nope").join("settings.json");
        assert_eq!(
            promote_tier_to_account_at(&absent, true).unwrap(),
            TierWrite::SkippedSecondary
        );
        assert!(!absent.exists());
    }

    /// The fresh headless box: paired before the runner ever wrote a
    /// settings.json. Without this arm the box is latched at `Local` by the
    /// one-shot `migrate_tier_in_place` on its first boot.
    #[test]
    fn promote_tier_absent_file_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("com.qontinui.runner").join("settings.json");
        assert_eq!(
            promote_tier_to_account_at(&path, false).unwrap(),
            TierWrite::Written
        );
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["tier"], QONTINUI_ACCOUNT_TIER);
        assert_eq!(v["tier_initialized"], true);
        assert_eq!(
            v.as_object().unwrap().len(),
            2,
            "a created settings.json carries ONLY the tier keys; every other \
             field must come from its serde default"
        );
    }

    /// **A promotion is not a choice.** `tier_chosen_explicitly` permanently
    /// closes the tier inference and `coord doctor` tells operators to clear
    /// it, so its entire safety argument is that it records a HUMAN picking a
    /// tier. This writer is reached from both automatic pairing doors, so it
    /// must never touch the key — on a document that carries it, or on one that
    /// does not.
    #[test]
    fn promote_never_records_an_explicit_choice() {
        let dir = tempfile::tempdir().unwrap();

        // (a) a document with no such key: the promotion must not add one.
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"tier":"local","tier_initialized":true}"#).unwrap();
        assert_eq!(
            promote_tier_to_account_at(&path, false).unwrap(),
            TierWrite::Written
        );
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            v.get("tier_chosen_explicitly").is_none(),
            "an automatic promotion must not claim the operator chose"
        );

        // (b) a created file: only the two tier keys, never the choice flag.
        let fresh = dir.path().join("fresh").join("settings.json");
        assert_eq!(
            promote_tier_to_account_at(&fresh, false).unwrap(),
            TierWrite::Written
        );
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&fresh).unwrap()).unwrap();
        assert!(v.get("tier_chosen_explicitly").is_none());

        // (c) a document that already carries the flag keeps its value —
        //     promoting is not a licence to rewrite the operator's record.
        //     (`tier: "local"` + the flag is a pinned box; the writer still
        //     promotes the TIER, because the callers only reach it from a
        //     pairing that just succeeded — but it must not touch the flag.)
        let pinned = dir.path().join("pinned.json");
        std::fs::write(
            &pinned,
            r#"{"tier":"local","tier_initialized":true,"tier_chosen_explicitly":true}"#,
        )
        .unwrap();
        assert_eq!(
            promote_tier_to_account_at(&pinned, false).unwrap(),
            TierWrite::Written
        );
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&pinned).unwrap()).unwrap();
        assert_eq!(v["tier_chosen_explicitly"], true);
    }

    // ------------------------------------------------------------------
    // set_tier_choice_at / clear_tier_choice_at — the headless TierStep and
    // its un-set (`qontinui_profile tier --set` / `--clear-choice`), which is
    // the door `coord doctor`'s TIER_FIX_UNPIN names.
    // ------------------------------------------------------------------

    /// `--set` records BOTH the tier and the fact that a human chose it, and
    /// the reader then refuses to infer over it — on a paired box, which is
    /// precisely where an unrecorded choice would have been overridden.
    #[test]
    fn set_tier_choice_records_the_choice_and_closes_inference() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"tier":"local","tier_initialized":true}"#).unwrap();

        // Paired: without a recorded choice this document reads as Tier 2.
        assert_eq!(
            read_runner_tier_at(&path, /* paired = */ true, /* server_mode = */ false),
            TierRead::Known(QONTINUI_ACCOUNT_TIER.to_string())
        );

        assert_eq!(
            set_tier_choice_at(&path, false, LOCAL_TIER).unwrap(),
            TierWrite::Written
        );
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["tier"], LOCAL_TIER);
        assert_eq!(v["tier_initialized"], true);
        assert_eq!(v["tier_chosen_explicitly"], true);
        assert_eq!(
            read_runner_tier_at(&path, /* paired = */ true, /* server_mode = */ true),
            TierRead::Known(LOCAL_TIER.to_string()),
            "neither pairing nor a headless launch may override an operator's \
             recorded choice"
        );

        // It can also write a HIGHER tier — a human said so.
        assert_eq!(
            set_tier_choice_at(&path, false, QONTINUI_ACCOUNT_TIER).unwrap(),
            TierWrite::Written
        );
        assert_eq!(
            read_runner_tier_at(&path, /* paired = */ false, /* server_mode = */ false),
            TierRead::Known(QONTINUI_ACCOUNT_TIER.to_string())
        );

        // A nonsense tier is refused, and nothing is written.
        let before = std::fs::read(&path).unwrap();
        assert!(set_tier_choice_at(&path, false, "enterprise_someday").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    /// `--clear-choice` re-opens the inference and touches NOTHING else. The
    /// tier it leaves behind is still `local`; what changes is that the
    /// inference is allowed to look at it again.
    #[test]
    fn clear_tier_choice_reopens_inference_without_setting_a_tier() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"tier":"local","tier_initialized":true,"tier_chosen_explicitly":true,"keep":42}"#,
        )
        .unwrap();
        assert_eq!(
            read_runner_tier_at(&path, /* paired = */ true, /* server_mode = */ false),
            TierRead::Known(LOCAL_TIER.to_string()),
            "pinned"
        );

        assert_eq!(
            clear_tier_choice_at(&path, false).unwrap(),
            TierWrite::Written
        );
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["tier_chosen_explicitly"], false);
        assert_eq!(v["tier"], LOCAL_TIER, "the tier itself is left alone");
        assert_eq!(v["keep"], 42, "and so is every unrelated key");

        assert_eq!(
            read_runner_tier_at(&path, /* paired = */ true, /* server_mode = */ false),
            TierRead::Known(QONTINUI_ACCOUNT_TIER.to_string()),
            "un-pinned, the pairing signal resolves Tier 2 — which is what \
             TIER_FIX_UNPIN promises the operator"
        );

        // A secondary may not run it either: same shared-settings.json hazard.
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            clear_tier_choice_at(&path, true).unwrap(),
            TierWrite::SkippedSecondary
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    // ------------------------------------------------------------------
    // legacy_tier_choice_is_deducible — the pre-`tier_chosen_explicitly`
    // upgrade path.
    // ------------------------------------------------------------------

    /// The deduction, stated directly, and the three shapes it refuses.
    #[test]
    fn legacy_tier_choice_is_deducible_arms() {
        // The one sound deduction: `local` + a token is a value the OLD
        // inference could not have produced (it would have said
        // qontinui_account), so only `set_runner_tier` can have written it.
        assert!(legacy_tier_choice_is_deducible(
            true,
            Some(LOCAL_TIER),
            true
        ));

        // No token: `local` is exactly what the old inference produced. That
        // is the box the unlatch exists to rescue.
        assert!(!legacy_tier_choice_is_deducible(
            true,
            Some(LOCAL_TIER),
            false
        ));

        // Never initialized: `tier` is just the struct default, not a decision.
        assert!(!legacy_tier_choice_is_deducible(
            false,
            Some(LOCAL_TIER),
            true
        ));

        // `qontinui_account` proves nothing about a human: `redeem_pair_code`
        // and `finalize_signed_in` write it automatically. (It needs no
        // back-fill either — `tier_is_open_to_inference` already closes it.)
        assert!(!legacy_tier_choice_is_deducible(
            true,
            Some(QONTINUI_ACCOUNT_TIER),
            true
        ));
        assert!(!legacy_tier_choice_is_deducible(
            true,
            Some(QONTINUI_ACCOUNT_TIER),
            false
        ));

        // `local_provider` IS an explicit choice, but that deduction already
        // lives in `tier_is_open_to_inference`; stating it twice is how rules
        // drift.
        assert!(!legacy_tier_choice_is_deducible(
            true,
            Some("local_provider"),
            true
        ));
        assert!(!tier_is_open_to_inference(Some("local_provider"), false));

        // No tier at all: nothing to deduce from.
        assert!(!legacy_tier_choice_is_deducible(true, None, true));
    }

    /// **The regression this closes**, end to end in the doctor's reader: an
    /// operator signed in (token persisted), then chose Local in the
    /// SetupWizard BEFORE `tier_chosen_explicitly` existed. On upgrade their
    /// box must not be silently re-promoted.
    #[test]
    fn a_pre_phase_3_explicit_local_is_not_re_promoted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"tier":"local","tier_initialized":true,
                "web_integration":{"runner_token":"legacy-token"}}"#,
        )
        .unwrap();

        for (paired, server_mode) in [(false, false), (true, false), (true, true)] {
            assert_eq!(
                read_runner_tier_at(&path, paired, server_mode),
                TierRead::Known(LOCAL_TIER.to_string()),
                "paired={paired} server_mode={server_mode}: a deducible explicit \
                 choice must survive every signal"
            );
        }

        // PRESENT-and-false is a different document: the key exists, so nothing
        // is deduced and the token promotes as it always did. This is the
        // distinction that forced a raw `Value` read rather than a
        // `#[serde(default)]` struct.
        let explicit_false = dir.path().join("explicit_false.json");
        std::fs::write(
            &explicit_false,
            r#"{"tier":"local","tier_initialized":true,"tier_chosen_explicitly":false,
                "web_integration":{"runner_token":"legacy-token"}}"#,
        )
        .unwrap();
        assert_eq!(
            read_runner_tier_at(&explicit_false, false, false),
            TierRead::Known(QONTINUI_ACCOUNT_TIER.to_string())
        );
    }

    /// Idempotence across the two doors: the CLI door promoting after the
    /// WebView door already did must be a pure no-op.
    #[test]
    fn promote_tier_is_idempotent_across_both_doors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{\"tier\":\"local\",\"tier_initialized\":false}").unwrap();

        assert_eq!(
            promote_tier_to_account_at(&path, false).unwrap(),
            TierWrite::Written
        );
        let after_first = std::fs::read(&path).unwrap();
        assert_eq!(
            promote_tier_to_account_at(&path, false).unwrap(),
            TierWrite::Unchanged
        );
        assert_eq!(std::fs::read(&path).unwrap(), after_first);
    }
}
