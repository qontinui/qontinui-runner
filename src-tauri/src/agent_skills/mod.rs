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
//! this module never shortens its name, and its provenance enum is
//! [`AgentSkillSource`] rather than a second `SkillSource`
//! (`crate::skills::SkillSource` already exists and means something else
//! entirely — the capability manifest imports both).
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
//! on-disk cache keyed by backend URL, a `validate_override`-style rejection
//! that falls back one rung and warns, and one [`AgentSkillSource`] variant per
//! ARM of [`resolve_registry`] so a served answer and a cache replay are
//! distinguishable from outside (the collapse `agent_commands` retired in plan
//! `2026-08-31-published-build-parity-check` Phase 3).
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
//! step down the chain and warns. The floor is the embedded bundle
//! (`crate::fleet_skills::FLEET_SKILLS`, an `include_dir!` tree), which is
//! byte-identically what a device with no account receives — and what every
//! device received before this module existed.
//!
//! ## `invocable_only=true` is mandatory on this fetch
//!
//! The corpus carries underscore-prefixed **copy-source specs**
//! (`_gate-registration`, `_plan-corpus`) that other units paste from and that
//! must never become invocable. Anything this module fetches is written to
//! disk, so the fetch passes `invocable_only=true` and [`validate_override`]
//! refuses a non-invocable unit a second time — the query parameter is the
//! server's job and the check is ours, and a fleet device must not depend on a
//! backend it cannot audit to get that right.
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
//! * **Served content is never written with an executable bit** — see
//!   [`crate::fleet_skills`]. The embedded tree keeps its `0o755` on `.sh`,
//!   because that content is reviewed source in this repository rather than
//!   account-supplied text.
//!
//! On top of those, [`self_path`] refuses a bundle that cannot reach its own
//! files once provisioned.
//!
//! ## Where the server half lives
//!
//! `GET {api_base}/api/v1/agent-text-units?kind=skill&invocable_only=true`,
//! authenticated with the user access token — the same credential
//! [`crate::agent_commands`] uses. It landed in qontinui-web#1071 (merged
//! 2026-09-02). This module is the runner half of plan
//! `2026-08-20-fleet-served-agent-skills`, Phases 4 and 6.

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
/// tooling. The skills half is also the cheap half, which is the whole reason
/// this is a `kind`-filtered request rather than a shared one.
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

/// Ceiling on the fetched body, in bytes. Generous against the real corpus
/// (~200 KB, measured 2026-08-22) and far below what `FETCH_LIMIT` units at
/// `MAX_UNIT_BYTES` each could theoretically be — the point is a bound that
/// exists at all, not a tight one. See [`fetch_skills_async`].
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Resolved skills
// ---------------------------------------------------------------------------

/// Where a resolved skill's files came from — **one variant per arm of
/// [`resolve_registry`]'s resolution order**, which is the property that makes
/// this type usable as provenance rather than merely as a label.
///
/// Named `AgentSkillSource`, not `SkillSource`: `crate::skills::SkillSource`
/// already exists and describes automation templates, and
/// [`crate::capability_manifest`] imports both.
///
/// The three-variant shape is deliberate and is `agent_commands`'
/// `CommandSource` after plan `2026-08-31-published-build-parity-check` Phase 3.
/// A two-variant `Builtin`/`Account` enum cannot say whether a published install
/// with no network resolved from its own cache or a dev box resolved off the
/// wire — and that difference IS the parity measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSkillSource {
    /// Embedded in this binary (`crate::fleet_skills::FLEET_SKILLS`, an
    /// `include_dir!` tree) — the floor, present wherever the binary is.
    Builtin,
    /// Fetched over the network from the signed-in account THIS RUN
    /// ([`FetchOutcome::Fresh`]).
    Served,
    /// Read from this device's own `agent-skills-cache.json`, written by an
    /// earlier successful fetch. Never carried by the build, and reached only
    /// when the fetch was [`FetchOutcome::Unavailable`].
    DiskCache,
}

impl AgentSkillSource {
    /// The stable wire string, consumed by
    /// [`crate::capability_manifest::Rung::from_agent_skill_source`] and by log
    /// lines. Matches `agent_commands::CommandSource::as_str` value for value
    /// so the two registry rows read alike.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentSkillSource::Builtin => "builtin",
            AgentSkillSource::Served => "served",
            AgentSkillSource::DiskCache => "disk_cache",
        }
    }

    /// Whether content from this layer is **account-supplied**, i.e. text this
    /// device fetched rather than text this binary carries.
    ///
    /// The provisioner reads this to decide the executable bit: a `.sh` from
    /// the embedded tree is reviewed source in this repository and keeps its
    /// `0o755`, while a `.sh` that arrived over the wire (or out of a cache
    /// written from the wire) is written non-executable and run as
    /// `bash <path>`. Turning account-supplied text into an account-supplied
    /// program is the one step that cannot be undone by a later fetch.
    pub fn is_account_supplied(self) -> bool {
        match self {
            AgentSkillSource::Builtin => false,
            AgentSkillSource::Served | AgentSkillSource::DiskCache => true,
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
    pub source: AgentSkillSource,
}

impl ResolvedSkill {
    /// The `.claude/skills/` subdirectory name for this skill.
    pub fn dir_name(&self) -> &str {
        &self.name
    }
}

/// Why this `files` map cannot be written as a directory tree, or `None`.
///
/// Two shapes, both of which pass every PER-KEY validator and are nonetheless
/// jointly unwritable — neither the Rust validators nor qontinui-web's mirror
/// looks at one key in the light of another:
///
/// 1. **A key that is also another key's directory prefix**, e.g. `{"a": …,
///    "a/b.md": …}`. `a` is created as a regular file and `create_dir_all("a")`
///    then fails `EEXIST`. Because `AgentTextUnitFiles` is a `BTreeMap`, `a` is
///    always reached FIRST, so the failure lands mid-bundle with half the skill
///    on disk.
/// 2. **The same, or a bare duplicate, once case is folded** — `{"A": …,
///    "a/b.md": …}` or `{"a.md": …, "A.md": …}`. Byte-distinct, so (1) misses
///    them, and unwritable on a case-INSENSITIVE destination: `A` lands as a
///    file and `create_dir_all` on `a` hits an existing non-directory
///    (`std` only swallows `AlreadyExists` when the path `is_dir()`), while two
///    keys differing only in case silently race to write one file and are
///    counted twice.
///
/// **The case arm is enforced on every platform, not just Windows.** A bundle
/// that provisions on Linux and half-writes on Windows is the worst of both —
/// and it is the same reason `validate_agent_text_unit_file_path` already
/// refuses backslashes, trailing dots and `nul.md` fleet-wide: the strictest
/// destination governs. This fleet runs on Windows.
///
/// Refusing the whole unit is what makes "a skill with any bad path is skipped
/// ENTIRELY, never partially written" true for these input classes. The
/// provisioner re-checks it for the same reason it re-checks every other path
/// rule.
///
/// The exact arm uses `range` rather than a neighbour scan: keys sorting
/// between `a` and `a/b.md` are possible (`a!x`, since `!` < `/`), so
/// contiguity cannot be assumed. The folded arm is O(n^2) over at most
/// `MAX_FILES_PER_UNIT` keys, and iterates in `BTreeMap` order so its verdict
/// is deterministic.
pub(crate) fn unwritable_key_conflict(files: &AgentTextUnitFiles) -> Option<String> {
    for key in files.keys() {
        let prefix = format!("{key}/");
        if let Some((other, _)) = files.range(prefix.clone()..).next() {
            if other.starts_with(&prefix) {
                return Some(format!(
                    "file {key:?} is also the directory prefix of {other:?} — one of the \
                     two cannot exist, so the bundle is unwritable"
                ));
            }
        }
    }

    let folded: Vec<(String, &String)> = files.keys().map(|k| (k.to_lowercase(), k)).collect();
    for (i, (lower, key)) in folded.iter().enumerate() {
        let prefix = format!("{lower}/");
        for (j, (other_lower, other)) in folded.iter().enumerate() {
            if i == j {
                continue;
            }
            if other_lower == lower {
                if i < j {
                    return Some(format!(
                        "files {key:?} and {other:?} differ only in case — on a \
                         case-insensitive filesystem they are one file, which two entries \
                         would race to write"
                    ));
                }
                continue;
            }
            if other_lower.starts_with(&prefix) {
                return Some(format!(
                    "file {key:?} is also the directory prefix of {other:?} once case is \
                     folded — unwritable on a case-insensitive filesystem, and a bundle \
                     must not provision on one platform and half-write on another"
                ));
            }
        }
    }
    None
}

/// Validate one fetched unit into a [`ResolvedSkill`], or explain why it is
/// unusable. A rejected unit falls back to the embedded default for its name.
///
/// `source` is the arm that supplied `unit` — [`AgentSkillSource::Served`] for
/// a live fetch, [`AgentSkillSource::DiskCache`] for a cache replay. It is
/// passed in rather than assumed because this function cannot tell them apart
/// and the difference is the measurement.
///
/// The order matters and is the order the failures are cheapest to explain in:
/// wrong kind, bad name, not invocable, bad files, then the self-path shape.
pub(crate) fn validate_override(
    unit: &AgentTextUnit,
    source: AgentSkillSource,
) -> Result<ResolvedSkill, String> {
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
    if let Some(why) = unwritable_key_conflict(&unit.files) {
        return Err(why);
    }

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
        source,
    })
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// One embedded default as a test fixture: a name and its bundle, as
/// `(relative path, text)` pairs.
///
/// The SHIPPED bundle is not a slice of these — it is
/// `crate::fleet_skills::FLEET_SKILLS`, an `include_dir!` tree, so adding a
/// skill stays "add a directory and nothing else". This type exists so the
/// layering rules can be proved against a fixed bundle rather than against
/// whatever this binary happens to embed today.
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
    /// Which arm of [`resolve_registry`]'s three-rung order actually answered.
    /// See [`AgentSkillRegistry::resolution_arm`].
    resolution_arm: AgentSkillSource,
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
        Self::from_builtins(crate::fleet_skills::embedded_skills())
    }

    /// A registry over an arbitrary embedded bundle. Tests pass their own, so
    /// the layering rules are proved against a bundle rather than against
    /// whatever this binary happens to embed today.
    pub fn from_embedded(bundle: &[EmbeddedSkill]) -> Self {
        Self::from_builtins(
            bundle
                .iter()
                .map(|skill| ResolvedSkill {
                    name: skill.name.to_string(),
                    files: skill
                        .files
                        .iter()
                        .map(|(path, text)| ((*path).to_string(), (*text).to_string()))
                        .collect(),
                    source: AgentSkillSource::Builtin,
                })
                .collect(),
        )
    }

    /// A registry over an already-built builtin layer.
    fn from_builtins(builtin: Vec<ResolvedSkill>) -> Self {
        Self {
            builtin,
            overrides: Vec::new(),
            // Nothing has been layered on yet, so the embedded floor is what
            // answered. `set_overrides` moves this up when an arm supplies one.
            resolution_arm: AgentSkillSource::Builtin,
        }
    }

    /// Install the account layer, dropping (and warning about) any unit that
    /// fails validation. Returns the number of units accepted.
    ///
    /// `source` names the arm that supplied `units` —
    /// [`AgentSkillSource::Served`] for a live fetch,
    /// [`AgentSkillSource::DiskCache`] for a cache replay. It is a required
    /// argument rather than a default because the two are the parity difference
    /// this type exists to report.
    ///
    /// It is recorded as the registry's [`resolution_arm`](Self::resolution_arm)
    /// even when zero units survive validation: an arm that answered and
    /// supplied nothing usable is a different (and more interesting) fact than
    /// an arm that was never reached.
    ///
    /// Never fails: a wholly malformed payload yields zero overrides, which is
    /// the embedded-default state for the FILES while still recording which arm
    /// produced them.
    pub fn set_overrides(&mut self, units: Vec<AgentTextUnit>, source: AgentSkillSource) -> usize {
        debug_assert!(
            source != AgentSkillSource::Builtin,
            "the embedded floor is not an override layer; pass Served or DiskCache"
        );
        let mut accepted: Vec<ResolvedSkill> = Vec::with_capacity(units.len());
        let mut seen: HashSet<String> = HashSet::new();
        for unit in &units {
            match validate_override(unit, source) {
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
        self.resolution_arm = source;
        self.overrides.len()
    }

    /// Which arm of [`resolve_registry`]'s `fresh fetch → disk cache → embedded
    /// default` order answered for this registry.
    ///
    /// This is the value the capability manifest carries for
    /// `agent_skills_registry`. Read it together with
    /// [`override_count`](Self::override_count): a [`AgentSkillSource::Served`]
    /// arm with zero overrides means the account authoritatively has none, so
    /// every FILE is still the embedded default even though the served arm is
    /// what established that.
    #[must_use]
    pub fn resolution_arm(&self) -> AgentSkillSource {
        self.resolution_arm
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

    /// How many FILES the resolved set would write. Not the same number as
    /// [`all`](Self::all)`.len()`: a skill is a directory.
    pub fn resolved_file_count(&self) -> usize {
        self.all().iter().map(|s| s.files.len()).sum()
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

/// Append `chunk` to `body`, or refuse with the size the body WOULD have
/// reached.
///
/// Split out of [`fetch_skills_async`] so the ceiling has a falsification test:
/// the async loop cannot be driven without an HTTP mock, and every other guard
/// added alongside it is pinned at its boundary. Refusing leaves `body`
/// untouched — the caller abandons it, but a partial append would make the
/// error message's size claim wrong.
fn push_bounded(body: &mut Vec<u8>, chunk: &[u8], max: usize) -> Result<(), usize> {
    let would_be = body.len().saturating_add(chunk.len());
    if would_be > max {
        return Err(would_be);
    }
    body.extend_from_slice(chunk);
    Ok(())
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
    let mut resp = match client.get(&url).bearer_auth(&token).send().await {
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

    // Read the body in CHUNKS against a ceiling rather than `resp.json()`,
    // which buffers whatever arrives. The per-unit caps are enforced after
    // parsing, so without this a backend answering with an unbounded body
    // exhausts memory before any validator runs — on the thread a session spawn
    // is blocked on. `Content-Length` is not the gate: a wrong or absent one is
    // exactly the case this has to survive, so the accumulated length is.
    let mut body: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if let Err(would_be) = push_bounded(&mut body, &chunk, MAX_RESPONSE_BYTES) {
                    return FetchOutcome::Unavailable(format!(
                        "GET {url} would exceed the {MAX_RESPONSE_BYTES}-byte ceiling \
                         ({would_be} bytes and still arriving) — refusing to buffer it"
                    ));
                }
            }
            Ok(None) => break,
            Err(e) => return FetchOutcome::Unavailable(format!("GET {url} body read failed: {e}")),
        }
    }
    match serde_json::from_slice::<AgentTextUnitListResponse>(&body) {
        Ok(parsed) => FetchOutcome::Fresh(parsed.items),
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
    resolve_over(AgentSkillRegistry::new(), outcome, cached)
}

/// [`resolve_with`] over an explicit embedded floor. The shipped floor is
/// whatever `include_dir!` embedded; tests pass a fixed bundle so the layering
/// rules are proved rather than sampled.
pub(crate) fn resolve_over(
    mut registry: AgentSkillRegistry,
    outcome: FetchOutcome,
    cached: Option<Vec<AgentTextUnit>>,
) -> (AgentSkillRegistry, CacheAction) {
    match outcome {
        FetchOutcome::Fresh(units) => {
            let n = registry.set_overrides(units.clone(), AgentSkillSource::Served);
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
                    let n = registry.set_overrides(units, AgentSkillSource::DiskCache);
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
///
/// **Which of the three arms answered is a value**, not just a log line: read it
/// off the returned registry with [`AgentSkillRegistry::resolution_arm`]. The
/// caller (`fleet_skills::provision_fleet_skills_for_session`) turns it into the
/// capability manifest's `agent_skills_registry` row.
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
    info!(
        "agent_skills: resolved via the {} arm ({} account skill(s) over {} embedded default(s))",
        registry.resolution_arm().as_str(),
        registry.override_count(),
        registry.builtin_count(),
    );
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

    /// A bundle from `(relative path, text)` pairs.
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

    pub(crate) fn registry() -> AgentSkillRegistry {
        AgentSkillRegistry::from_embedded(TEST_BUNDLE)
    }

    fn resolve_over_test_bundle(
        outcome: FetchOutcome,
        cached: Option<Vec<AgentTextUnit>>,
    ) -> (AgentSkillRegistry, CacheAction) {
        resolve_over(registry(), outcome, cached)
    }

    // -- the resolution chain -----------------------------------------------

    /// A device with no account resolves the embedded defaults, byte
    /// identically.
    #[test]
    fn no_account_resolves_embedded_defaults_byte_identically() {
        let (reg, action) = resolve_over_test_bundle(FetchOutcome::NoAccount, None);
        assert_eq!(action, CacheAction::Clear);
        assert_eq!(reg.override_count(), 0);
        assert_eq!(reg.resolution_arm(), AgentSkillSource::Builtin);

        let resolved = reg.all();
        assert_eq!(resolved.len(), TEST_BUNDLE.len());
        for (i, embedded) in TEST_BUNDLE.iter().enumerate() {
            assert_eq!(resolved[i].name, embedded.name);
            assert_eq!(resolved[i].source, AgentSkillSource::Builtin);
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
        // A LIVE fetch, so the files are `served` — not the same fact as the
        // cached arm below.
        assert_eq!(hit.source, AgentSkillSource::Served);
        assert_eq!(reg.resolution_arm(), AgentSkillSource::Served);
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
            AgentSkillSource::Served
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
        // The DISK CACHE answered, not the network.
        assert_eq!(hit.source, AgentSkillSource::DiskCache);
        assert_eq!(reg.resolution_arm(), AgentSkillSource::DiskCache);
    }

    /// Unavailable with nothing cached is the embedded-default floor, and it
    /// SAYS so — it must not be reported as `disk_cache` merely because the
    /// cache was the arm that was tried.
    #[test]
    fn unavailable_with_no_cache_falls_back_to_defaults() {
        let (reg, action) =
            resolve_over_test_bundle(FetchOutcome::Unavailable("dns failure".to_string()), None);
        assert_eq!(action, CacheAction::Keep);
        assert_eq!(reg.override_count(), 0);
        assert_eq!(
            reg.get("preflight").unwrap().source,
            AgentSkillSource::Builtin
        );
        assert_eq!(reg.resolution_arm(), AgentSkillSource::Builtin);
    }

    /// An authoritative empty account means "no skills", and it replaces the
    /// cache rather than leaving a stale one in place — while still recording
    /// the SERVED arm, because "the account has none" is a network reading.
    #[test]
    fn empty_fresh_fetch_clears_the_override_layer() {
        let (reg, action) = resolve_over_test_bundle(FetchOutcome::Fresh(vec![]), None);
        assert_eq!(reg.override_count(), 0);
        assert_eq!(action, CacheAction::Store(vec![]));
        assert_eq!(reg.resolution_arm(), AgentSkillSource::Served);
        assert_eq!(
            reg.get("preflight").unwrap().source,
            AgentSkillSource::Builtin
        );
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
            crate::fleet_skills::embedded_skill_count()
        );
        assert!(
            shipped.builtin_count() >= 1,
            "the bundle must ship at least one skill"
        );
        assert_eq!(registry().builtin_count(), TEST_BUNDLE.len());
    }

    /// The THREE arms report THREE distinct sources, and the capability
    /// manifest turns each into a distinct rung — the same gate
    /// `agent_commands` carries, because the collapse it removed is exactly the
    /// one a two-variant enum would re-introduce here.
    #[test]
    fn the_three_resolution_arms_report_three_distinct_rungs() {
        use crate::capability_manifest::Rung;

        let (served, _) = resolve_over_test_bundle(
            FetchOutcome::Fresh(vec![simple_unit("coord-revive", "# wire\n")]),
            None,
        );
        let (cached, _) = resolve_over_test_bundle(
            FetchOutcome::Unavailable("connection refused".to_string()),
            Some(vec![simple_unit("coord-revive", "# cached\n")]),
        );
        let (embedded, _) = resolve_over_test_bundle(FetchOutcome::NoAccount, None);

        let arms = [
            served.resolution_arm(),
            cached.resolution_arm(),
            embedded.resolution_arm(),
        ];
        assert_eq!(
            arms,
            [
                AgentSkillSource::Served,
                AgentSkillSource::DiskCache,
                AgentSkillSource::Builtin
            ]
        );

        let rungs: Vec<Rung> = arms.iter().map(|s| Rung::from(*s)).collect();
        assert_eq!(rungs, vec![Rung::Served, Rung::DiskCache, Rung::Embedded]);
        let distinct: HashSet<&'static str> = rungs.iter().map(|r| r.wire()).collect();
        assert_eq!(distinct.len(), 3);
    }

    /// Every `AgentSkillSource` round-trips its wire string, and only the two
    /// network-fed arms count as account-supplied — the predicate the
    /// executable-bit rule reads.
    #[test]
    fn agent_skill_source_wire_strings_and_account_predicate() {
        let all = [
            AgentSkillSource::Builtin,
            AgentSkillSource::Served,
            AgentSkillSource::DiskCache,
        ];
        let wires: Vec<&'static str> = all.iter().map(|s| s.as_str()).collect();
        assert_eq!(wires, vec!["builtin", "served", "disk_cache"]);
        assert_eq!(wires.iter().collect::<HashSet<_>>().len(), all.len());
        assert!(!AgentSkillSource::Builtin.is_account_supplied());
        assert!(AgentSkillSource::Served.is_account_supplied());
        assert!(AgentSkillSource::DiskCache.is_account_supplied());
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
                validate_override(&unit, AgentSkillSource::Served).is_err(),
                "{bad:?} must be rejected as a skill file path"
            );
        }
        // And the rejection reaches the registry, not just the validator.
        let mut reg = registry();
        let n = reg.set_overrides(
            vec![skill_unit(
                "coord-revive",
                bundle(&[("SKILL.md", "# x\n"), ("../../evil.md", "pwn\n")]),
            )],
            AgentSkillSource::Served,
        );
        assert_eq!(n, 0);
        assert_eq!(
            reg.get("coord-revive").unwrap().source,
            AgentSkillSource::Builtin
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
                validate_override(&unit, AgentSkillSource::Served).is_ok(),
                "{good:?} should be accepted: {:?}",
                validate_override(&unit, AgentSkillSource::Served).err()
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
                validate_override(&unit, AgentSkillSource::Served).is_err(),
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
        assert!(validate_override(&unit, AgentSkillSource::Served)
            .unwrap_err()
            .contains("too large"));

        // Whole bundle: each file inside the per-file cap, the sum over the
        // unit cap. Per-file caps alone do not bound a bundle.
        let chunk = "y".repeat(MAX_FILE_BYTES);
        let n = MAX_UNIT_BYTES / MAX_FILE_BYTES + 1;
        let mut files = bundle(&[("SKILL.md", "# ok\n")]);
        for i in 0..n {
            files.insert(format!("chunk{i}.md"), chunk.clone());
        }
        let unit = skill_unit("probe", files);
        let err = validate_override(&unit, AgentSkillSource::Served).unwrap_err();
        assert!(err.contains("unit is too large"), "{err}");

        // File count.
        let mut files = bundle(&[("SKILL.md", "# ok\n")]);
        for i in 0..=MAX_FILES_PER_UNIT {
            files.insert(format!("f{i}.md"), "x\n".to_string());
        }
        let unit = skill_unit("probe", files);
        assert!(validate_override(&unit, AgentSkillSource::Served)
            .unwrap_err()
            .contains("too many files"));
    }

    /// An empty bundle, a blank file, and a bundle with no `SKILL.md` are all
    /// unusable — a blank override shadowing a working default is the exact
    /// failure the fail-soft chain exists to avoid.
    #[test]
    fn empty_blank_and_entrypointless_bundles_are_rejected() {
        assert!(validate_override(
            &skill_unit("probe", AgentTextUnitFiles::new()),
            AgentSkillSource::Served
        )
        .is_err());
        assert!(validate_override(
            &skill_unit("probe", bundle(&[("SKILL.md", "  \n")])),
            AgentSkillSource::Served
        )
        .is_err());
        assert!(validate_override(
            &skill_unit("probe", bundle(&[("readme.md", "# not an entrypoint\n")])),
            AgentSkillSource::Served
        )
        .is_err());
    }

    /// A non-invocable unit is refused even if the server sent it — the
    /// `invocable_only=true` query parameter is the server's job and this is
    /// ours.
    #[test]
    fn non_invocable_units_are_never_provisioned() {
        let mut unit = simple_unit("_gate-registration", "# copy-source spec\n");
        unit.is_invocable = false;
        assert!(validate_override(&unit, AgentSkillSource::Served)
            .unwrap_err()
            .contains("not invocable"));

        // And the underscore/invocability pairing is refused from the other
        // side too: an underscore unit claiming to be invocable is malformed.
        let mut lying = simple_unit("_gate-registration", "# copy-source spec\n");
        lying.is_invocable = true;
        assert!(validate_override(&lying, AgentSkillSource::Served).is_err());
    }

    /// A `command` row must never be provisioned as a skill.
    #[test]
    fn a_non_skill_kind_is_refused() {
        let mut unit = simple_unit("vet-plan", "# /vet-plan\n");
        unit.kind = AgentTextUnitKind::command();
        assert!(validate_override(&unit, AgentSkillSource::Served)
            .unwrap_err()
            .contains("kind"));
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
        let err = validate_override(&broken, AgentSkillSource::Served).unwrap_err();
        assert!(err.contains("cannot reach its own files"), "{err}");

        let mut reg = registry();
        assert_eq!(reg.set_overrides(vec![broken], AgentSkillSource::Served), 0);
        assert_eq!(
            reg.get("coord-revive").unwrap().source,
            AgentSkillSource::Builtin
        );
    }

    /// **A `files` key that is also another key's directory prefix is refused
    /// whole.** Both keys pass every per-key validator, and the pair is
    /// unwritable: `a` lands as a regular file and `create_dir_all("a")` then
    /// fails `EEXIST` — mid-bundle, because a `BTreeMap` always reaches `a`
    /// first.
    #[test]
    fn a_file_that_is_also_a_directory_prefix_is_rejected() {
        for (parent, child) in [("a", "a/b.md"), ("dir", "dir/sub/deep.md")] {
            let unit = skill_unit(
                "probe",
                bundle(&[("SKILL.md", "# probe\n"), (parent, "x\n"), (child, "y\n")]),
            );
            let err = match validate_override(&unit, AgentSkillSource::Served) {
                Ok(_) => panic!("{parent:?} + {child:?} must be refused"),
                Err(e) => e,
            };
            assert!(
                err.contains("directory prefix"),
                "{parent:?} + {child:?}: {err}"
            );
        }
    }

    /// The detector does not assume the colliding keys are ADJACENT in sort
    /// order. `a!x` sorts between `a` and `a/b.md` (`!` is 0x21, `/` is 0x2F),
    /// so a neighbour-only scan would miss the pair entirely.
    #[test]
    fn the_conflict_detector_does_not_assume_adjacency() {
        let why = unwritable_key_conflict(&bundle(&[
            ("SKILL.md", "# probe\n"),
            ("a", "x\n"),
            ("a!x", "y\n"),
            ("a/b.md", "z\n"),
        ]))
        .expect("a non-adjacent prefix collision must still be found");
        assert!(why.contains("directory prefix"), "{why}");
        assert!(why.contains("\"a\"") && why.contains("\"a/b.md\""), "{why}");
    }

    /// **The negative half.** A detector that refused ordinary bundles would
    /// take every skill in the corpus down with it, and the corpus cannot
    /// falsify that on its own — so state it on the near-misses: a key that
    /// merely SHARES a prefix (`ab` vs `a/b.md`), an ordinary nested layout, and
    /// two files whose names differ by more than case.
    #[test]
    fn ordinary_bundles_report_no_conflict() {
        assert_eq!(
            unwritable_key_conflict(&bundle(&[
                ("SKILL.md", "# probe\n"),
                ("ab", "x\n"),
                ("a/b.md", "y\n"),
                ("reference/one.md", "z\n"),
                ("reference/two.md", "z\n"),
                ("Reference.md", "z\n"),
            ])),
            None
        );
        // And every skill this binary actually ships.
        for skill in crate::fleet_skills::embedded_skills() {
            assert_eq!(
                unwritable_key_conflict(&skill.files),
                None,
                "embedded skill {:?} must not be refused by this detector",
                skill.name
            );
        }
    }

    /// **The case-folded arm, enforced on every platform.** `A` + `a/b.md` is
    /// byte-distinct, so the exact arm misses it, and it is unwritable on a
    /// case-insensitive destination — which is what the Windows half of this
    /// fleet runs on. A bundle that provisions on Linux and half-writes on
    /// Windows is the outcome this refuses.
    #[test]
    fn case_folded_collisions_are_rejected_on_every_platform() {
        let why = unwritable_key_conflict(&bundle(&[
            ("SKILL.md", "# probe\n"),
            ("A", "x\n"),
            ("a/b.md", "y\n"),
        ]))
        .expect("a case-folded prefix collision must be refused");
        assert!(why.contains("case is folded"), "{why}");

        // Two keys differing ONLY in case are one file on such a destination,
        // and two entries would race to write it.
        let why = unwritable_key_conflict(&bundle(&[
            ("SKILL.md", "# probe\n"),
            ("a.md", "x\n"),
            ("A.md", "y\n"),
        ]))
        .expect("a case-only duplicate must be refused");
        assert!(why.contains("differ only in case"), "{why}");

        // The verdict is deterministic: same input, same answer, and it names
        // the pair in `BTreeMap` order rather than whichever was seen first.
        let files = bundle(&[("SKILL.md", "# probe\n"), ("a.md", "x\n"), ("A.md", "y\n")]);
        assert_eq!(
            unwritable_key_conflict(&files),
            unwritable_key_conflict(&files)
        );

        // And it reaches the resolver, not just the helper.
        let mut reg = registry();
        assert_eq!(
            reg.set_overrides(
                vec![skill_unit(
                    "coord-revive",
                    bundle(&[("SKILL.md", "# x\n"), ("A", "y\n"), ("a/b.md", "z\n")])
                )],
                AgentSkillSource::Served
            ),
            0
        );
        assert_eq!(
            reg.get("coord-revive").unwrap().source,
            AgentSkillSource::Builtin
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

    /// The fetched body is bounded, at exactly the stated ceiling.
    ///
    /// The per-unit caps are enforced only AFTER parsing, so without this an
    /// unbounded body exhausts memory before any validator runs — on the thread
    /// a session spawn is blocked on.
    #[test]
    fn the_response_body_is_bounded_at_its_stated_ceiling() {
        // Exactly the ceiling is accepted, in one chunk and across chunks.
        let mut body = Vec::new();
        assert_eq!(push_bounded(&mut body, &vec![b'x'; 10], 10), Ok(()));
        assert_eq!(body.len(), 10);

        let mut body = Vec::new();
        assert_eq!(push_bounded(&mut body, &vec![b'x'; 6], 10), Ok(()));
        assert_eq!(push_bounded(&mut body, &vec![b'x'; 4], 10), Ok(()));
        assert_eq!(body.len(), 10);

        // One byte over is refused, and the refusal reports what the body WOULD
        // have reached rather than what it holds.
        let mut body = Vec::new();
        assert_eq!(push_bounded(&mut body, &vec![b'x'; 11], 10), Err(11));
        assert!(
            body.is_empty(),
            "a refused chunk must not be partially appended"
        );

        // The refusal is what bounds a stream that never ends: once full, the
        // next chunk is rejected however many follow it.
        let mut body = Vec::new();
        assert_eq!(push_bounded(&mut body, &vec![b'x'; 10], 10), Ok(()));
        assert_eq!(push_bounded(&mut body, &[b'x'], 10), Err(11));
        assert_eq!(body.len(), 10);

        assert!(
            MAX_RESPONSE_BYTES > 0,
            "a zero ceiling would refuse every fetch"
        );
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

    /// A broken cache degrades to the embedded floor rather than to an error —
    /// the "disk cache is unusable" rung of the chain, proved end to end
    /// through the same pure resolver the live path uses.
    #[test]
    fn a_broken_cache_degrades_to_the_embedded_floor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(CACHE_FILE);
        std::fs::write(&path, b"{not json").unwrap();

        let cached = read_cache_at(&path, "https://api.example");
        assert!(cached.is_none(), "an unparseable cache is a MISS");

        let (reg, action) = resolve_over_test_bundle(
            FetchOutcome::Unavailable("connection refused".to_string()),
            cached,
        );
        assert_eq!(action, CacheAction::Keep);
        assert_eq!(reg.override_count(), 0);
        assert_eq!(reg.resolution_arm(), AgentSkillSource::Builtin);
        assert_eq!(
            reg.get("coord-revive").unwrap().source,
            AgentSkillSource::Builtin
        );
    }

    /// A cache read against a path that does not exist is a miss, never an
    /// error — the offline-first-run case.
    #[test]
    fn missing_cache_is_a_miss() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(read_cache_at(&tmp.path().join("absent.json"), "https://api.example").is_none());
    }
}
