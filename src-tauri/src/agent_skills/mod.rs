//! Resolution of the agent **skills** provisioned into a spawned session's
//! `.claude/skills/` — embedded defaults, optionally overridden by the
//! signed-in account.
//!
//! ## `agent_skills`, never `skills`
//!
//! `crate::skills` is the automation-template registry — workflow building
//! blocks, Postgres-backed via `database/pg/skills.rs` — an entirely unrelated
//! concept that [`crate::agent_commands`] already has to disambiguate against.
//! A third meaning of "skill" in one crate is a defect waiting to happen, so
//! this module never shortens its name.
//!
//! ## The resolution chain
//!
//! ```text
//! resolution order (per skill name):
//!     account override  ─┐
//!     fleet default     ─┴─ one server-side query  →  disk cache  →  embedded default
//! ```
//!
//! The first two rungs are **not** re-derived here. qontinui-web's
//! `GET /api/v1/agent-text-units` already returns the *resolved* view — account
//! overrides plus the unshadowed `organization_id IS NULL` fleet defaults — and
//! reports which layer each row came from in `source`. A client-side merge
//! would be a second implementation of a rule the store owns.
//!
//! Everything below that is [`crate::agent_commands`]' shape, deliberately:
//! override-by-NAME rather than concatenation, fail-soft at every layer, an
//! on-disk cache keyed by backend URL, and a `validate_override`-style
//! rejection that falls back one rung and warns.
//!
//! ## Override-by-name, NOT concatenation
//!
//! Two entries cannot both become `.claude/skills/coord-revive/`. An account
//! skill named `coord-revive` **replaces** the embedded one; it never coexists
//! with it. An account skill whose name matches no embedded default is
//! additive — refusing to write it would discard user content silently.
//!
//! ## Fail-soft at every layer
//!
//! No fetch failure, auth failure, malformed unit, or cache IO error may
//! produce an error value that reaches a spawn path. Every failure degrades one
//! step down the chain and warns. The floor is the embedded bundle, which is
//! byte-identically what a device with no account receives.
//!
//! ## `invocable_only=true` is mandatory on this fetch
//!
//! The corpus carries underscore-prefixed **copy-source specs**
//! (`_gate-registration`, `_loop-control`) that other units paste from and that
//! must never become invocable. Anything this module fetches is written to
//! disk, so the fetch passes `invocable_only=true` and
//! [`validate_override`] refuses a non-invocable unit a second time — the
//! query parameter is the server's job and the check is ours, and a fleet
//! device must not depend on a backend it cannot audit to get that right.
//!
//! ## The content is untrusted remote text
//!
//! A skill bundle is markdown and shell text rather than compiled code, but it
//! is instructions to an agent, it can include a `.sh` the agent is told to
//! run, and it becomes files in a session's working directory. Three
//! consequences, all enforced before anything is written:
//!
//! * Names and every `files` key go through the canonical validators in
//!   `qontinui_types::agent_text_units`, so a key can never escape the skill's
//!   own directory (no `..`, no absolute path, no drive letter, no backslash).
//! * Per-file, whole-bundle and file-count caps are the store's own constants,
//!   so a unit the store accepted cannot be one the runner then refuses.
//! * Nothing is ever written with an executable bit — see
//!   [`crate::fleet_skills`].
//!
//! On top of those, [`self_path`] refuses a bundle that cannot reach its own
//! files once provisioned.

pub mod self_path;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use qontinui_types::agent_text_units::{
    validate_agent_text_unit_files, validate_agent_text_unit_invocability,
    validate_agent_text_unit_name, AgentTextUnit, AgentTextUnitFiles, AgentTextUnitKind,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Wall-clock budget for the whole override fetch, matching
/// `agent_commands::FETCH_TIMEOUT`. Provisioning runs on a spawn path, so the
/// network layer is hard-bounded — a slow or black-holed backend degrades to
/// the cache rather than delaying a session launch.
///
/// Note what this costs at a spawn: the commands fetch and this one run
/// sequentially at each call site, so the worst case a session pays for
/// text-corpus provisioning is **two** of these budgets, not one. That is
/// accepted rather than parallelized because both are fail-soft and a spawn
/// that took 8 s longer is strictly better than one that launched without its
/// tooling. The skills half is also the cheap half: the corpus measured 193 KB
/// over 9 units on 2026-08-22 against 1.61 MB for the commands, which is the
/// whole reason this is a `kind`-filtered request rather than a shared one.
const FETCH_TIMEOUT: Duration = Duration::from_secs(4);

/// Page size for the list endpoint. Nothing here may hardcode the corpus size;
/// 500 is the endpoint's documented `limit` ceiling.
const FETCH_LIMIT: u32 = 500;

/// Filename of the on-disk override cache, under the runner's per-instance
/// config dir — the same convention `agent_commands`, `prompts.rs` and
/// `backup.rs` already use. Deliberately NOT a new path scheme, and
/// deliberately a *different file* from the commands cache: the two are fetched
/// by separate `kind`-filtered requests and either may be stale alone.
const CACHE_FILE: &str = "agent-skills-cache.json";

/// Schema version of [`CachedSkills`]. A cache written by a different version
/// is ignored (and overwritten on the next successful fetch) rather than parsed
/// on a guess.
const CACHE_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Resolved skills
// ---------------------------------------------------------------------------

/// Where a resolved skill's files came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    /// Embedded in this binary (`crate::fleet_skills::FLEET_SKILLS`).
    Builtin,
    /// Fetched from (or cached from) the signed-in account — either the
    /// account's own override or the fleet-default layer behind it.
    Account,
}

impl SkillSource {
    pub fn as_str(self) -> &'static str {
        match self {
            SkillSource::Builtin => "builtin",
            SkillSource::Account => "account",
        }
    }
}

/// One skill as it will actually be written to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSkill {
    /// Slug — the directory name under `.claude/skills/`.
    pub name: String,
    /// The bundle: relative path → text. Guaranteed non-empty and to contain
    /// `SKILL.md` when it came through [`validate_override`].
    pub files: AgentTextUnitFiles,
    /// Which layer supplied [`files`](Self::files).
    pub source: SkillSource,
}

impl ResolvedSkill {
    /// The `.claude/skills/` subdirectory name for this skill.
    pub fn dir_name(&self) -> &str {
        &self.name
    }
}

/// Validate one fetched unit into a [`ResolvedSkill`], or explain why it is
/// unusable. A rejected unit falls back to the embedded default for its name.
///
/// The order matters and is the order the failures are cheapest to explain in:
/// wrong kind, bad name, not invocable, bad files, then the self-path shape.
pub(crate) fn validate_override(unit: &AgentTextUnit) -> Result<ResolvedSkill, String> {
    if unit.kind.as_str() != AgentTextUnitKind::SKILL {
        // Not a filter miss to shrug at: a `command` row provisioned as a skill
        // would create `.claude/skills/<name>/<name>.md` with no `SKILL.md`,
        // i.e. a directory the harness reads as a broken skill.
        return Err(format!(
            "unit kind is {:?}, not {:?}",
            unit.kind.as_str(),
            AgentTextUnitKind::SKILL
        ));
    }
    let name = unit.name.trim();
    validate_agent_text_unit_name(name).map_err(|e| e.to_string())?;
    validate_agent_text_unit_invocability(name, unit.is_invocable).map_err(|e| e.to_string())?;
    if !unit.is_invocable {
        // The fetch already asks the server for invocable units only. This is
        // the second half of that: a copy-source spec written into
        // `.claude/skills/` becomes a skill the harness offers, and a fleet
        // device must not depend on a query parameter it cannot audit.
        return Err("unit is not invocable and must not be provisioned".to_string());
    }
    // Caps and per-file relative-subpath validation, from the canonical
    // validators: non-empty, <= MAX_FILES_PER_UNIT entries, every key a safe
    // relative path, no blank file, each file <= MAX_FILE_BYTES, the bundle
    // <= MAX_UNIT_BYTES, and `SKILL.md` present.
    validate_agent_text_unit_files(&unit.kind, name, &unit.files).map_err(|e| e.to_string())?;

    let violations = self_path::skill_self_path_violations(&unit.files);
    if !violations.is_empty() {
        return Err(format!(
            "cannot reach its own files once provisioned: {}",
            violations
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }

    Ok(ResolvedSkill {
        name: name.to_string(),
        files: unit.files.clone(),
        source: SkillSource::Account,
    })
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// One embedded default: a name and its bundle, as `(relative path, text)`
/// pairs. `crate::fleet_skills::FLEET_SKILLS` is a slice of these.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddedSkill {
    pub name: &'static str,
    pub files: &'static [(&'static str, &'static str)],
}

/// Embedded defaults plus the account's units, layered **by name**.
#[derive(Debug, Clone)]
pub struct AgentSkillRegistry {
    builtin: Vec<ResolvedSkill>,
    overrides: Vec<ResolvedSkill>,
}

impl Default for AgentSkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentSkillRegistry {
    /// A registry holding only this binary's embedded defaults — exactly what a
    /// device with no account resolves to.
    pub fn new() -> Self {
        Self::from_embedded(crate::fleet_skills::FLEET_SKILLS)
    }

    /// A registry over an arbitrary embedded bundle. `new()` passes the shipped
    /// one; tests pass their own, so the layering rules are proved against a
    /// bundle rather than against whatever this binary happens to embed today.
    pub fn from_embedded(bundle: &[EmbeddedSkill]) -> Self {
        Self {
            builtin: bundle
                .iter()
                .map(|skill| ResolvedSkill {
                    name: skill.name.to_string(),
                    files: skill
                        .files
                        .iter()
                        .map(|(path, text)| ((*path).to_string(), (*text).to_string()))
                        .collect(),
                    source: SkillSource::Builtin,
                })
                .collect(),
            overrides: Vec::new(),
        }
    }

    /// Install the account layer, dropping (and warning about) any unit that
    /// fails validation. Returns the number of units accepted.
    ///
    /// Never fails: a wholly malformed payload yields zero overrides, which is
    /// the embedded-default state.
    pub fn set_overrides(&mut self, units: Vec<AgentTextUnit>) -> usize {
        let mut accepted: Vec<ResolvedSkill> = Vec::with_capacity(units.len());
        let mut seen: HashSet<String> = HashSet::new();
        for unit in &units {
            match validate_override(unit) {
                Ok(resolved) => {
                    if !seen.insert(resolved.name.clone()) {
                        warn!(
                            "agent_skills: duplicate unit for {:?} — keeping the first and \
                             ignoring the rest (the account layer is unique per name)",
                            resolved.name
                        );
                        continue;
                    }
                    accepted.push(resolved);
                }
                Err(why) => {
                    warn!(
                        "agent_skills: ignoring malformed skill {:?} ({why}) — falling back \
                         to the embedded default for it",
                        unit.name
                    );
                }
            }
        }
        self.overrides = accepted;
        self.overrides.len()
    }

    /// The resolved skill set, in a stable order: every embedded default (in
    /// bundle order), replaced in place by a same-named account unit, followed
    /// by account skills that have no embedded counterpart.
    pub fn all(&self) -> Vec<&ResolvedSkill> {
        let mut out: Vec<&ResolvedSkill> =
            Vec::with_capacity(self.builtin.len() + self.overrides.len());
        for b in &self.builtin {
            match self.overrides.iter().find(|o| o.name == b.name) {
                Some(o) => out.push(o),
                None => out.push(b),
            }
        }
        let builtin_names: HashSet<&str> = self.builtin.iter().map(|s| s.name.as_str()).collect();
        for o in &self.overrides {
            if !builtin_names.contains(o.name.as_str()) {
                out.push(o);
            }
        }
        out
    }

    /// Resolve one skill by name (account layer wins).
    pub fn get(&self, name: &str) -> Option<&ResolvedSkill> {
        self.overrides
            .iter()
            .find(|s| s.name == name)
            .or_else(|| self.builtin.iter().find(|s| s.name == name))
    }

    /// How many defaults are embedded in this binary.
    pub fn builtin_count(&self) -> usize {
        self.builtin.len()
    }

    /// How many account units are installed.
    pub fn override_count(&self) -> usize {
        self.overrides.len()
    }

    /// Install an account layer that has NOT been through
    /// [`validate_override`]. Test-only, and it exists for exactly one purpose:
    /// proving that the provisioner's own traversal refusal holds when it is
    /// handed a registry the resolver would never have produced. Production
    /// code has no way to build one.
    #[cfg(test)]
    pub(crate) fn set_unvalidated_overrides(&mut self, skills: Vec<ResolvedSkill>) {
        self.overrides = skills;
    }
}

// ---------------------------------------------------------------------------
// Disk cache
// ---------------------------------------------------------------------------

/// The on-disk cache of the account layer.
///
/// `backend_url` is part of the record and is checked on read: a cache written
/// against one backend must never be served to a session pointed at another.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedSkills {
    cache_version: u32,
    backend_url: String,
    fetched_at: String,
    skills: Vec<AgentTextUnit>,
}

/// Absolute path of the skill cache for this runner instance, or `None` when
/// the platform has no config dir.
fn cache_path() -> Option<PathBuf> {
    let base = dirs::config_dir()?.join("com.qontinui.runner");
    Some(crate::instance::scope_path(&base).join(CACHE_FILE))
}

/// Read the cache at `path`, accepting it only when it was written by this
/// cache version against `backend_url`. Any IO/parse failure is a miss, never
/// an error.
fn read_cache_at(path: &Path, backend_url: &str) -> Option<Vec<AgentTextUnit>> {
    let raw = std::fs::read_to_string(path).ok()?;
    let cached: CachedSkills = match serde_json::from_str(&raw) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "agent_skills: skill cache at {} is unparseable ({e}) — ignoring it",
                path.display()
            );
            return None;
        }
    };
    if cached.cache_version != CACHE_VERSION {
        debug!(
            "agent_skills: skill cache version {} != {CACHE_VERSION} — ignoring it",
            cached.cache_version
        );
        return None;
    }
    if cached.backend_url != backend_url {
        debug!(
            "agent_skills: skill cache was written against {:?} but this session resolves \
             {:?} — ignoring it rather than crossing backends",
            cached.backend_url, backend_url
        );
        return None;
    }
    Some(cached.skills)
}

/// Persist `skills` as the cache at `path`. Best-effort: a write failure is
/// warned and swallowed.
fn write_cache_at(path: &Path, backend_url: &str, skills: &[AgentTextUnit]) {
    let record = CachedSkills {
        cache_version: CACHE_VERSION,
        backend_url: backend_url.to_string(),
        fetched_at: now_rfc3339(),
        skills: skills.to_vec(),
    };
    let bytes = match serde_json::to_vec_pretty(&record) {
        Ok(b) => b,
        Err(e) => {
            warn!("agent_skills: could not serialize the skill cache ({e})");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(
                "agent_skills: could not create cache dir {} ({e}) — continuing without a cache",
                parent.display()
            );
            return;
        }
    }
    if let Err(e) = crate::fs_atomic::atomic_write(path, &bytes) {
        warn!(
            "agent_skills: could not write the skill cache {} ({e}) — continuing",
            path.display()
        );
    }
}

/// Remove the cache at `path`, ignoring a missing file.
fn clear_cache_at(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => info!(
            "agent_skills: cleared the skill cache at {} (no account for this device)",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!(
            "agent_skills: could not clear the skill cache {} ({e})",
            path.display()
        ),
    }
}

/// RFC 3339 timestamp, matching the wire convention of the schemas types.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// Fetch
// ---------------------------------------------------------------------------

/// The list endpoint's envelope. Only `items` is consumed — `pagination` is
/// irrelevant at corpus scale and unknown fields are ignored.
#[derive(Debug, Deserialize)]
struct AgentTextUnitListResponse {
    #[serde(default)]
    items: Vec<AgentTextUnit>,
}

/// What one attempt at the account layer established.
///
/// **Absent** and **unknown** are not the same fact. `NoAccount` is an
/// authoritative "this device has no account layer" and therefore *invalidates*
/// a cache; `Unavailable` is "could not tell", which is exactly when the cache
/// is the right answer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FetchOutcome {
    /// Authenticated fetch succeeded; this is the resolved skill set (possibly
    /// empty, which is authoritative).
    Fresh(Vec<AgentTextUnit>),
    /// No usable credential, or the backend rejected it (401/403).
    NoAccount,
    /// Transport error, server error, or an unparseable body — UNKNOWN.
    Unavailable(String),
}

/// Perform the fetch on a dedicated thread with its own current-thread tokio
/// runtime.
///
/// Callers reach this from a *sync* provisioning function invoked from *async*
/// spawn paths. `Handle::block_on` panics when called from inside a runtime
/// worker, so the work is moved onto its own thread instead — the same
/// arrangement, and the same reasoning, as `agent_commands`.
fn fetch_skills_blocking(base_url: &str) -> FetchOutcome {
    let url = base_url.to_string();
    let handle = std::thread::Builder::new()
        .name("agent-skills-fetch".to_string())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    return FetchOutcome::Unavailable(format!("could not build a runtime: {e}"))
                }
            };
            rt.block_on(fetch_skills_async(&url))
        });
    match handle {
        Ok(h) => h
            .join()
            .unwrap_or_else(|_| FetchOutcome::Unavailable("the fetch thread panicked".to_string())),
        Err(e) => FetchOutcome::Unavailable(format!("could not spawn a fetch thread: {e}")),
    }
}

/// The list URL this module fetches, split out so a test can pin the two query
/// parameters that are load-bearing rather than cosmetic.
///
/// * `kind=skill` — `/agent-text-units` serves the whole corpus, and fetching
///   the commands here would provision them into `.claude/skills/`.
/// * `invocable_only=true` — see the module docs. Without it the
///   underscore-prefixed copy-source specs are written to disk and become
///   invocable units.
fn list_url(base_url: &str) -> String {
    format!(
        "{base_url}/api/v1/agent-text-units?kind={}&invocable_only=true&limit={FETCH_LIMIT}",
        AgentTextUnitKind::SKILL
    )
}

/// GET the resolved skill units with the stored bearer.
async fn fetch_skills_async(base_url: &str) -> FetchOutcome {
    let auth = crate::auth::AuthManager::new();
    let token = match auth.get_access_token() {
        Ok(t) if !t.trim().is_empty() => t,
        Ok(_) | Err(_) => {
            debug!(
                "agent_skills: no stored access token — resolving the embedded defaults \
                 (sign in to use account skills)"
            );
            return FetchOutcome::NoAccount;
        }
    };

    let client = match reqwest::Client::builder().timeout(FETCH_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => return FetchOutcome::Unavailable(format!("could not build an HTTP client: {e}")),
    };
    let url = list_url(base_url);
    let resp = match client.get(&url).bearer_auth(&token).send().await {
        Ok(r) => r,
        Err(e) => return FetchOutcome::Unavailable(format!("GET {url} failed: {e}")),
    };
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return FetchOutcome::NoAccount;
    }
    if !status.is_success() {
        return FetchOutcome::Unavailable(format!("GET {url} returned HTTP {status}"));
    }
    match resp.json::<AgentTextUnitListResponse>().await {
        Ok(body) => FetchOutcome::Fresh(body.items),
        Err(e) => FetchOutcome::Unavailable(format!("GET {url} returned an unreadable body: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// What [`resolve_with`] decided should happen to the on-disk cache.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CacheAction {
    /// Persist this unit set as the new cache.
    Store(Vec<AgentTextUnit>),
    /// Delete any cache — the account layer authoritatively does not apply.
    Clear,
    /// Leave the cache exactly as it is.
    Keep,
}

/// Pure resolver: turn one [`FetchOutcome`] plus whatever the cache held into
/// the registry to provision. Split out from [`resolve_registry`] so every
/// resolution gate is testable without a backend or a filesystem.
pub(crate) fn resolve_with(
    outcome: FetchOutcome,
    cached: Option<Vec<AgentTextUnit>>,
) -> (AgentSkillRegistry, CacheAction) {
    let mut registry = AgentSkillRegistry::new();
    match outcome {
        FetchOutcome::Fresh(units) => {
            let n = registry.set_overrides(units.clone());
            debug!("agent_skills: fetched {n} account skill(s)");
            (registry, CacheAction::Store(units))
        }
        FetchOutcome::NoAccount => {
            // Authoritative absence: a stale cache from a previous sign-in must
            // not keep shadowing the defaults after sign-out.
            (registry, CacheAction::Clear)
        }
        FetchOutcome::Unavailable(why) => {
            match cached {
                Some(units) => {
                    let n = registry.set_overrides(units);
                    warn!(
                        "agent_skills: account skills unavailable ({why}) — serving {n} \
                         cached skill(s)"
                    );
                }
                None => {
                    warn!(
                        "agent_skills: account skills unavailable ({why}) and no usable cache \
                         — serving the embedded defaults"
                    );
                }
            }
            // Never overwrite or drop a cache on an inconclusive fetch.
            (registry, CacheAction::Keep)
        }
    }
}

/// Resolve the skill set to provision: fresh fetch → disk cache → embedded
/// defaults.
///
/// Never fails and never panics. Every layer degrades to the next one; the
/// floor is the embedded bundle.
pub fn resolve_registry() -> AgentSkillRegistry {
    let base_url = crate::api_config::get_api_base_url();
    let path = cache_path();
    let outcome = fetch_skills_blocking(&base_url);
    let cached = match (&outcome, &path) {
        // Only pay for the cache read when it can actually be used.
        (FetchOutcome::Unavailable(_), Some(p)) => read_cache_at(p, &base_url),
        _ => None,
    };
    let (registry, action) = resolve_with(outcome, cached);
    if let Some(p) = &path {
        match action {
            CacheAction::Store(units) => write_cache_at(p, &base_url, &units),
            CacheAction::Clear => clear_cache_at(p),
            CacheAction::Keep => {}
        }
    }
    registry
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use qontinui_types::agent_text_units::{MAX_FILES_PER_UNIT, MAX_FILE_BYTES, MAX_UNIT_BYTES};

    /// A two-file bundle that passes every validator — the shape the real
    /// `coord-revive` has.
    pub(crate) fn bundle(entries: &[(&str, &str)]) -> AgentTextUnitFiles {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    pub(crate) fn skill_unit(name: &str, files: AgentTextUnitFiles) -> AgentTextUnit {
        AgentTextUnit {
            id: format!("id-{name}"),
            kind: AgentTextUnitKind::skill(),
            name: name.to_string(),
            organization_id: Some("org-1".to_string()),
            created_by_user_id: Some("user-1".to_string()),
            entrypoint: "SKILL.md".to_string(),
            files,
            checksum: None,
            is_shared: false,
            is_invocable: true,
            current_version: 1,
            source: "user".to_string(),
            source_path: None,
            source_commit: None,
            created_at: "2026-08-24T00:00:00Z".to_string(),
            updated_at: "2026-08-24T00:00:00Z".to_string(),
        }
    }

    pub(crate) fn simple_unit(name: &str, body: &str) -> AgentTextUnit {
        skill_unit(name, bundle(&[("SKILL.md", body)]))
    }

    /// The embedded bundle the layering tests run against, so they prove the
    /// rules rather than whatever this binary happens to embed today.
    pub(crate) const TEST_BUNDLE: &[EmbeddedSkill] = &[
        EmbeddedSkill {
            name: "coord-revive",
            files: &[
                ("SKILL.md", "# coord-revive (embedded)\n"),
                ("coord-revive.sh", "#!/usr/bin/env bash\necho embedded\n"),
            ],
        },
        EmbeddedSkill {
            name: "preflight",
            files: &[("SKILL.md", "# preflight (embedded)\n")],
        },
    ];

    fn registry() -> AgentSkillRegistry {
        AgentSkillRegistry::from_embedded(TEST_BUNDLE)
    }

    fn resolve_over_test_bundle(
        outcome: FetchOutcome,
        cached: Option<Vec<AgentTextUnit>>,
    ) -> (AgentSkillRegistry, CacheAction) {
        // `resolve_with` builds `AgentSkillRegistry::new()`, i.e. the SHIPPED
        // bundle. These tests want the layering proved over a bundle that is
        // non-empty regardless of what Phase 6 has embedded so far, so they
        // rebuild the same decision over `TEST_BUNDLE`.
        let (shipped, action) = resolve_with(outcome, cached);
        let mut over_test = registry();
        // Re-apply exactly the overrides `resolve_with` accepted.
        over_test.overrides = shipped.overrides;
        (over_test, action)
    }

    // -- the resolution chain -----------------------------------------------

    /// A device with no account resolves the embedded defaults, byte
    /// identically.
    #[test]
    fn no_account_resolves_embedded_defaults_byte_identically() {
        let (reg, action) = resolve_over_test_bundle(FetchOutcome::NoAccount, None);
        assert_eq!(action, CacheAction::Clear);
        assert_eq!(reg.override_count(), 0);

        let resolved = reg.all();
        assert_eq!(resolved.len(), TEST_BUNDLE.len());
        for (i, embedded) in TEST_BUNDLE.iter().enumerate() {
            assert_eq!(resolved[i].name, embedded.name);
            assert_eq!(resolved[i].source, SkillSource::Builtin);
            for (path, text) in embedded.files {
                assert_eq!(
                    resolved[i].files.get(*path).map(String::as_str),
                    Some(*text),
                    "embedded default {}/{path} must be served byte-identically",
                    embedded.name
                );
            }
        }
    }

    /// An account skill REPLACES the same-named default rather than coexisting
    /// with it — two entries cannot both become one directory.
    #[test]
    fn override_replaces_the_default_by_name() {
        let (reg, action) = resolve_over_test_bundle(
            FetchOutcome::Fresh(vec![simple_unit("coord-revive", "# mine\n")]),
            None,
        );
        assert!(matches!(action, CacheAction::Store(_)));

        let resolved = reg.all();
        assert_eq!(
            resolved.len(),
            TEST_BUNDLE.len(),
            "an override must REPLACE the default, not be appended alongside it"
        );
        let hit = reg.get("coord-revive").expect("override resolves by name");
        assert_eq!(
            hit.files.get("SKILL.md").map(String::as_str),
            Some("# mine\n")
        );
        assert_eq!(hit.source, SkillSource::Account);
        assert_eq!(
            hit.files.len(),
            1,
            "the override's bundle replaces the default's WHOLE bundle — a partial \
             merge would leave the default's stale sibling files behind"
        );
        assert_eq!(
            resolved.iter().filter(|s| s.name == "coord-revive").count(),
            1
        );
    }

    /// An account skill with no embedded counterpart is additive.
    #[test]
    fn account_only_skill_is_additive() {
        let (reg, _) = resolve_over_test_bundle(
            FetchOutcome::Fresh(vec![simple_unit("visual-audit", "# mine\n")]),
            None,
        );
        assert_eq!(reg.all().len(), TEST_BUNDLE.len() + 1);
        assert_eq!(
            reg.get("visual-audit").unwrap().source,
            SkillSource::Account
        );
    }

    /// A cached skill with the network down still wins over the default — and
    /// the cache is NOT clobbered by the failed fetch.
    #[test]
    fn cached_skill_survives_an_unavailable_backend() {
        let (reg, action) = resolve_over_test_bundle(
            FetchOutcome::Unavailable("connection refused".to_string()),
            Some(vec![simple_unit("coord-revive", "# cached\n")]),
        );
        assert_eq!(action, CacheAction::Keep);
        let hit = reg.get("coord-revive").unwrap();
        assert_eq!(
            hit.files.get("SKILL.md").map(String::as_str),
            Some("# cached\n")
        );
        assert_eq!(hit.source, SkillSource::Account);
    }

    /// Unavailable with nothing cached is the embedded-default floor.
    #[test]
    fn unavailable_with_no_cache_falls_back_to_defaults() {
        let (reg, action) =
            resolve_over_test_bundle(FetchOutcome::Unavailable("dns failure".to_string()), None);
        assert_eq!(action, CacheAction::Keep);
        assert_eq!(reg.override_count(), 0);
        assert_eq!(reg.get("preflight").unwrap().source, SkillSource::Builtin);
    }

    /// An authoritative empty account means "no skills", and it replaces the
    /// cache rather than leaving a stale one in place.
    #[test]
    fn empty_fresh_fetch_clears_the_override_layer() {
        let (reg, action) = resolve_over_test_bundle(FetchOutcome::Fresh(vec![]), None);
        assert_eq!(reg.override_count(), 0);
        assert_eq!(action, CacheAction::Store(vec![]));
    }

    /// Duplicate names in one payload keep the first and drop the rest — the
    /// resolved set can never contain two entries writing the same directory.
    #[test]
    fn duplicate_names_collapse() {
        let (reg, _) = resolve_over_test_bundle(
            FetchOutcome::Fresh(vec![
                simple_unit("dupe", "# first\n"),
                simple_unit("dupe", "# second\n"),
            ]),
            None,
        );
        assert_eq!(reg.override_count(), 1);
        assert_eq!(
            reg.get("dupe")
                .unwrap()
                .files
                .get("SKILL.md")
                .map(String::as_str),
            Some("# first\n")
        );
    }

    /// Nothing here may assume how many skills the binary embeds.
    #[test]
    fn registry_does_not_assume_a_bundle_size() {
        let shipped = AgentSkillRegistry::new();
        assert_eq!(
            shipped.builtin_count(),
            crate::fleet_skills::FLEET_SKILLS.len()
        );
        assert_eq!(registry().builtin_count(), TEST_BUNDLE.len());
    }

    // -- validation ----------------------------------------------------------

    /// **Falsification gate.** A `files` key that escapes the skill's own
    /// directory must never reach the filesystem layer. If any of these is
    /// admitted, the provisioner writes outside `.claude/skills/<name>/`.
    #[test]
    fn traversal_and_absolute_file_paths_are_rejected() {
        for bad in [
            "../evil.md",
            "..",
            "./SKILL.md",
            "a/../../evil.md",
            "/etc/passwd",
            "C:/Windows/system32/evil.md",
            "c:evil.md",
            "sub\\evil.md",
            "..\\evil.md",
            "",
            "a//b.md",
            "trailing/",
            " leading.md",
            "trailing /file.md",
            "trailing.md ",
            "ends.with.dot.",
            "nul.md",
            "sub/con.sh",
            "a/b/c/d/e/f/g/h/i.md",
        ] {
            let unit = skill_unit(
                "probe",
                bundle(&[("SKILL.md", "# probe\n"), (bad, "payload\n")]),
            );
            assert!(
                validate_override(&unit).is_err(),
                "{bad:?} must be rejected as a skill file path"
            );
        }
        // And the rejection reaches the registry, not just the validator.
        let mut reg = registry();
        let n = reg.set_overrides(vec![skill_unit(
            "coord-revive",
            bundle(&[("SKILL.md", "# x\n"), ("../../evil.md", "pwn\n")]),
        )]);
        assert_eq!(n, 0);
        assert_eq!(
            reg.get("coord-revive").unwrap().source,
            SkillSource::Builtin
        );
    }

    /// The paths that must keep working — a stricter Rust rule would refuse
    /// units the store happily accepted.
    #[test]
    fn ordinary_relative_paths_are_accepted() {
        for good in [
            "SKILL.md",
            "coord-revive.sh",
            "reference/policy.md",
            ".gitkeep-ish.md",
            "a b.md",
        ] {
            let unit = skill_unit(
                "probe",
                bundle(&[("SKILL.md", "# probe\n"), (good, "payload\n")]),
            );
            assert!(
                validate_override(&unit).is_ok(),
                "{good:?} should be accepted: {:?}",
                validate_override(&unit).err()
            );
        }
    }

    /// A traversal in the unit NAME cannot escape `.claude/skills/` either.
    #[test]
    fn traversal_names_are_rejected() {
        for bad in [
            "../evil",
            "..",
            ".",
            "a/b",
            "a\\b",
            "C:evil",
            "",
            "nul",
            "NUL",
            "Coord-Revive",
        ] {
            let unit = simple_unit(bad, "# x\n");
            assert!(
                validate_override(&unit).is_err(),
                "{bad:?} must be rejected as a skill name"
            );
        }
    }

    /// Per-file, whole-bundle and file-count caps, each proved at the boundary.
    #[test]
    fn size_and_count_caps_are_enforced() {
        // Per-file.
        let over_file = "x".repeat(MAX_FILE_BYTES + 1);
        let unit = skill_unit(
            "probe",
            bundle(&[("SKILL.md", "# ok\n"), ("big.md", &over_file)]),
        );
        assert!(validate_override(&unit).unwrap_err().contains("too large"));

        // Whole bundle: each file inside the per-file cap, the sum over the
        // unit cap. Per-file caps alone do not bound a bundle.
        let chunk = "y".repeat(MAX_FILE_BYTES);
        let n = MAX_UNIT_BYTES / MAX_FILE_BYTES + 1;
        let mut files = bundle(&[("SKILL.md", "# ok\n")]);
        for i in 0..n {
            files.insert(format!("chunk{i}.md"), chunk.clone());
        }
        let unit = skill_unit("probe", files);
        let err = validate_override(&unit).unwrap_err();
        assert!(err.contains("unit is too large"), "{err}");

        // File count.
        let mut files = bundle(&[("SKILL.md", "# ok\n")]);
        for i in 0..=MAX_FILES_PER_UNIT {
            files.insert(format!("f{i}.md"), "x\n".to_string());
        }
        let unit = skill_unit("probe", files);
        assert!(validate_override(&unit)
            .unwrap_err()
            .contains("too many files"));
    }

    /// An empty bundle, a blank file, and a bundle with no `SKILL.md` are all
    /// unusable — a blank override shadowing a working default is the exact
    /// failure the fail-soft chain exists to avoid.
    #[test]
    fn empty_blank_and_entrypointless_bundles_are_rejected() {
        assert!(validate_override(&skill_unit("probe", AgentTextUnitFiles::new())).is_err());
        assert!(validate_override(&skill_unit("probe", bundle(&[("SKILL.md", "  \n")]))).is_err());
        assert!(validate_override(&skill_unit(
            "probe",
            bundle(&[("readme.md", "# not an entrypoint\n")])
        ))
        .is_err());
    }

    /// A non-invocable unit is refused even if the server sent it — the
    /// `invocable_only=true` query parameter is the server's job and this is
    /// ours.
    #[test]
    fn non_invocable_units_are_never_provisioned() {
        let mut unit = simple_unit("_gate-registration", "# copy-source spec\n");
        unit.is_invocable = false;
        assert!(validate_override(&unit)
            .unwrap_err()
            .contains("not invocable"));

        // And the underscore/invocability pairing is refused from the other
        // side too: an underscore unit claiming to be invocable is malformed.
        let mut lying = simple_unit("_gate-registration", "# copy-source spec\n");
        lying.is_invocable = true;
        assert!(validate_override(&lying).is_err());
    }

    /// A `command` row must never be provisioned as a skill.
    #[test]
    fn a_non_skill_kind_is_refused() {
        let mut unit = simple_unit("vet-plan", "# /vet-plan\n");
        unit.kind = AgentTextUnitKind::command();
        assert!(validate_override(&unit).unwrap_err().contains("kind"));
    }

    /// A bundle that cannot reach its own script once provisioned is refused
    /// and the embedded default is served instead — the shape gate, wired into
    /// the resolution chain rather than living beside it.
    #[test]
    fn a_self_path_violating_bundle_falls_back_to_the_default() {
        let broken = skill_unit(
            "coord-revive",
            bundle(&[
                (
                    "SKILL.md",
                    "# coord-revive\nbash .../coord-revive/coord-revive.sh\n",
                ),
                ("coord-revive.sh", "#!/usr/bin/env bash\necho hi\n"),
            ]),
        );
        let err = validate_override(&broken).unwrap_err();
        assert!(err.contains("cannot reach its own files"), "{err}");

        let mut reg = registry();
        assert_eq!(reg.set_overrides(vec![broken]), 0);
        assert_eq!(
            reg.get("coord-revive").unwrap().source,
            SkillSource::Builtin
        );
    }

    // -- fetch URL -----------------------------------------------------------

    /// The two query parameters that are load-bearing rather than cosmetic.
    #[test]
    fn the_list_url_filters_by_kind_and_invocability() {
        let url = list_url("https://api.example");
        assert!(
            url.starts_with("https://api.example/api/v1/agent-text-units?"),
            "{url}"
        );
        assert!(url.contains("kind=skill"), "{url}");
        assert!(
            url.contains("invocable_only=true"),
            "without invocable_only the copy-source specs are written to disk: {url}"
        );
        assert!(url.contains(&format!("limit={FETCH_LIMIT}")), "{url}");
    }

    // -- cache ---------------------------------------------------------------

    /// Cache round-trip, plus the rejections that keep a cache from being
    /// served across backends or across cache versions.
    #[test]
    fn cache_round_trips_and_refuses_foreign_records() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("nested").join(CACHE_FILE);
        let skills = vec![simple_unit("coord-revive", "# cached\n")];

        write_cache_at(&path, "https://api.example", &skills);
        assert_eq!(
            read_cache_at(&path, "https://api.example").expect("cache round-trips"),
            skills
        );

        // A different backend must not be served this cache.
        assert!(read_cache_at(&path, "http://127.0.0.1:8000").is_none());

        // A different cache version must not be parsed on a guess.
        let bumped = serde_json::json!({
            "cache_version": CACHE_VERSION + 1,
            "backend_url": "https://api.example",
            "fetched_at": "2026-08-24T00:00:00Z",
            "skills": [],
        });
        std::fs::write(&path, serde_json::to_vec(&bumped).unwrap()).unwrap();
        assert!(read_cache_at(&path, "https://api.example").is_none());

        // Garbage is a miss, not a panic.
        std::fs::write(&path, b"{not json").unwrap();
        assert!(read_cache_at(&path, "https://api.example").is_none());

        // Clearing is idempotent.
        clear_cache_at(&path);
        clear_cache_at(&path);
        assert!(!path.exists());
    }

    /// A cache read against a path that does not exist is a miss, never an
    /// error — the offline-first-run case.
    #[test]
    fn missing_cache_is_a_miss() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(read_cache_at(&tmp.path().join("absent.json"), "https://api.example").is_none());
    }
}
