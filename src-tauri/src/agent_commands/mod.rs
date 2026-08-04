//! Resolution of the agent commands provisioned into a spawned session's
//! `.claude/commands/` — **embedded defaults, optionally overridden by the
//! signed-in account.**
//!
//! The defaults ship inside this binary (`crate::fleet_commands`), so an
//! unauthenticated, offline, or first-run device always resolves to a working
//! command set and the network is never on the critical path. This module adds
//! the optional account layer on top:
//!
//! ```text
//! resolution order (per command name):
//!     fresh fetch (qontinui-web)  →  disk cache  →  embedded default
//! ```
//!
//! ## Override-by-name, NOT concatenation
//!
//! [`AgentCommandRegistry`] deliberately diverges from its closest sibling,
//! `crate::skills::SkillRegistry`, whose `all()` is
//! `builtin.iter().chain(user.iter())`. Concatenation is wrong here: two
//! entries cannot both become `.claude/commands/vet-plan.md`. An account
//! command named `vet-plan` **replaces** the embedded `vet-plan`; it never
//! coexists with it. An account command whose name matches no embedded default
//! is additive — refusing to write it would discard user content silently.
//!
//! ## Fail-soft at every layer
//!
//! `provision_fleet_commands_for_session` is fail-soft by contract: a
//! provisioning failure must never abort an otherwise-launchable spawn. This
//! module preserves that end to end — no fetch failure, auth failure,
//! malformed body, or cache IO error can produce an error value that reaches a
//! spawn path. Every failure degrades one step down the resolution order and
//! warns.
//!
//! ## Why HTTP and not Postgres
//!
//! The runner's other user-content path (`database/pg/skills.rs`) is
//! Clorinde-generated and assumes the device holds database credentials — true
//! on the operator's dev box, false on a fleet device. Agent-command overrides
//! are fetched over the qontinui-web HTTP API instead, so org scoping is
//! enforced *server-side* by `check_organization_membership` rather than by a
//! client-side tenant filter on a device that should hold no DB credentials.
//!
//! ## The body is untrusted remote content
//!
//! An override body is markdown rather than code, but it is *instructions to an
//! agent* and it is written into a session's working directory. Names are
//! therefore restricted to a strict slug charset (no path separators, no `..`)
//! and bodies are size-capped; anything failing validation falls back to the
//! embedded default and warns.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use qontinui_types::agent_commands::AgentCommand;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Wall-clock budget for the whole override fetch. Provisioning runs on a
/// spawn path, so the network layer is hard-bounded — a slow or black-holed
/// backend degrades to the cache rather than delaying a session launch.
const FETCH_TIMEOUT: Duration = Duration::from_secs(4);

/// Page size for the list endpoint. The bundle is N commands and nothing here
/// may hardcode two; 500 is the endpoint's documented `limit` ceiling.
const FETCH_LIMIT: u32 = 500;

/// Largest override body accepted, in bytes. A command procedure is a few tens
/// of KB at most (the embedded defaults are ~60 KB combined); this cap exists
/// so a malformed or hostile row cannot write an unbounded file into a session
/// cwd.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Longest accepted command name.
const MAX_NAME_LEN: usize = 64;

/// Filename of the on-disk override cache, under the runner's per-instance
/// config dir (`dirs::config_dir()/com.qontinui.runner[/instance-<name>]`) —
/// the same convention `prompts.rs` and `backup.rs` already use for per-device
/// state. Deliberately NOT a new path scheme.
const CACHE_FILE: &str = "agent-commands-cache.json";

/// Schema version of [`CachedOverrides`]. A cache written by a different
/// version is ignored (and overwritten on the next successful fetch) rather
/// than parsed on a guess.
const CACHE_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Resolved commands
// ---------------------------------------------------------------------------

/// Where a resolved command's body came from. Mirrors the `source` provenance
/// field `SkillDefinition` carries (`skills/mod.rs`), narrowed to the two
/// layers that exist here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSource {
    /// Embedded in this binary via `include_str!`.
    Builtin,
    /// Fetched from (or cached from) the signed-in account.
    Account,
}

impl CommandSource {
    pub fn as_str(self) -> &'static str {
        match self {
            CommandSource::Builtin => "builtin",
            CommandSource::Account => "account",
        }
    }
}

/// One command as it will actually be written to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCommand {
    /// Slug — the slash-command name and the filename stem (`vet-plan` →
    /// `.claude/commands/vet-plan.md` → `/vet-plan`).
    pub name: String,
    /// Full markdown body.
    pub body: String,
    /// Which layer supplied [`body`](Self::body).
    pub source: CommandSource,
}

impl ResolvedCommand {
    /// The `.claude/commands/` filename for this command.
    pub fn file_name(&self) -> String {
        format!("{}.md", self.name)
    }
}

/// Reject anything that is not a safe, provisionable command slug.
///
/// Restricted to ASCII alphanumerics plus `-` and `_`. That excludes `/`, `\`,
/// `:` and — critically — `.`, so `..` can never appear and the name can never
/// escape `.claude/commands/`.
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("empty command name".to_string());
    }
    if name.len() > MAX_NAME_LEN {
        return Err(format!(
            "command name is {} bytes, over the {MAX_NAME_LEN}-byte limit",
            name.len()
        ));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        return Err(format!(
            "command name contains {bad:?}; only ASCII letters, digits, '-' and '_' are allowed"
        ));
    }
    Ok(())
}

/// Validate one fetched override into a [`ResolvedCommand`], or explain why it
/// is unusable. A rejected override falls back to the embedded default.
fn validate_override(cmd: &AgentCommand) -> Result<ResolvedCommand, String> {
    let name = cmd.name.trim();
    validate_name(name)?;
    if cmd.body.trim().is_empty() {
        return Err("override body is empty".to_string());
    }
    if cmd.body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "override body is {} bytes, over the {MAX_BODY_BYTES}-byte limit",
            cmd.body.len()
        ));
    }
    Ok(ResolvedCommand {
        name: name.to_string(),
        body: cmd.body.clone(),
        source: CommandSource::Account,
    })
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Embedded defaults plus the account's overrides, layered **by name**.
///
/// Shape mirrors `crate::skills::SkillRegistry` (a `builtin` vec, a
/// user-supplied vec, a setter, and an `all()`), with the one deliberate
/// divergence documented at the module level: `all()` overrides rather than
/// concatenates.
#[derive(Debug, Clone)]
pub struct AgentCommandRegistry {
    builtin: Vec<ResolvedCommand>,
    overrides: Vec<ResolvedCommand>,
}

impl Default for AgentCommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentCommandRegistry {
    /// A registry holding only the binary's embedded defaults — exactly what a
    /// device with no account resolves to.
    pub fn new() -> Self {
        Self {
            builtin: crate::fleet_commands::FLEET_COMMANDS
                .iter()
                .map(|(name, body)| ResolvedCommand {
                    name: (*name).to_string(),
                    body: (*body).to_string(),
                    source: CommandSource::Builtin,
                })
                .collect(),
            overrides: Vec::new(),
        }
    }

    /// Install the account layer, dropping (and warning about) any entry that
    /// fails validation. Returns the number of overrides accepted.
    ///
    /// Never fails: a wholly malformed payload yields zero overrides, which is
    /// the embedded-default state.
    pub fn set_overrides(&mut self, commands: Vec<AgentCommand>) -> usize {
        let mut accepted: Vec<ResolvedCommand> = Vec::with_capacity(commands.len());
        let mut seen: HashSet<String> = HashSet::new();
        for cmd in &commands {
            match validate_override(cmd) {
                Ok(resolved) => {
                    if !seen.insert(resolved.name.clone()) {
                        warn!(
                            "agent_commands: duplicate override for {:?} — keeping the first \
                             and ignoring the rest (the account layer is unique per name)",
                            resolved.name
                        );
                        continue;
                    }
                    accepted.push(resolved);
                }
                Err(why) => {
                    warn!(
                        "agent_commands: ignoring malformed override {:?} ({why}) — \
                         falling back to the embedded default for it",
                        cmd.name
                    );
                }
            }
        }
        self.overrides = accepted;
        self.overrides.len()
    }

    /// The resolved command set, in a stable order: every embedded default (in
    /// bundle order), replaced in place by a same-named account override,
    /// followed by account commands that have no embedded counterpart.
    pub fn all(&self) -> Vec<&ResolvedCommand> {
        let mut out: Vec<&ResolvedCommand> =
            Vec::with_capacity(self.builtin.len() + self.overrides.len());
        for b in &self.builtin {
            match self.overrides.iter().find(|o| o.name == b.name) {
                Some(o) => out.push(o),
                None => out.push(b),
            }
        }
        let builtin_names: HashSet<&str> =
            self.builtin.iter().map(|c| c.name.as_str()).collect();
        for o in &self.overrides {
            if !builtin_names.contains(o.name.as_str()) {
                out.push(o);
            }
        }
        out
    }

    /// Resolve one command by name (override wins).
    pub fn get(&self, name: &str) -> Option<&ResolvedCommand> {
        self.overrides
            .iter()
            .find(|c| c.name == name)
            .or_else(|| self.builtin.iter().find(|c| c.name == name))
    }

    /// How many defaults are embedded in this binary.
    pub fn builtin_count(&self) -> usize {
        self.builtin.len()
    }

    /// How many account overrides are installed.
    pub fn override_count(&self) -> usize {
        self.overrides.len()
    }
}

// ---------------------------------------------------------------------------
// Disk cache
// ---------------------------------------------------------------------------

/// The on-disk override cache.
///
/// `backend_url` is part of the record and is checked on read: a cache written
/// against one backend must never be served to a session pointed at another
/// (switching between a local backend and prod would otherwise cross accounts).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedOverrides {
    cache_version: u32,
    backend_url: String,
    fetched_at: String,
    commands: Vec<AgentCommand>,
}

/// Absolute path of the override cache for this runner instance, or `None`
/// when the platform has no config dir.
fn cache_path() -> Option<PathBuf> {
    let base = dirs::config_dir()?.join("com.qontinui.runner");
    Some(crate::instance::scope_path(&base).join(CACHE_FILE))
}

/// Read the cache at `path`, accepting it only when it was written by this
/// cache version against `backend_url`. Any IO/parse failure is a miss, never
/// an error — a corrupt cache degrades to the embedded defaults.
fn read_cache_at(path: &Path, backend_url: &str) -> Option<Vec<AgentCommand>> {
    let raw = std::fs::read_to_string(path).ok()?;
    let cached: CachedOverrides = match serde_json::from_str(&raw) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "agent_commands: override cache at {} is unparseable ({e}) — ignoring it",
                path.display()
            );
            return None;
        }
    };
    if cached.cache_version != CACHE_VERSION {
        debug!(
            "agent_commands: override cache version {} != {CACHE_VERSION} — ignoring it",
            cached.cache_version
        );
        return None;
    }
    if cached.backend_url != backend_url {
        debug!(
            "agent_commands: override cache was written against {:?} but this session \
             resolves {:?} — ignoring it rather than crossing backends",
            cached.backend_url, backend_url
        );
        return None;
    }
    Some(cached.commands)
}

/// Persist `commands` as the cache at `path`. Best-effort: a write failure is
/// warned and swallowed.
fn write_cache_at(path: &Path, backend_url: &str, commands: &[AgentCommand]) {
    let record = CachedOverrides {
        cache_version: CACHE_VERSION,
        backend_url: backend_url.to_string(),
        fetched_at: now_rfc3339(),
        commands: commands.to_vec(),
    };
    let bytes = match serde_json::to_vec_pretty(&record) {
        Ok(b) => b,
        Err(e) => {
            warn!("agent_commands: could not serialize override cache ({e})");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(
                "agent_commands: could not create cache dir {} ({e}) — continuing without a cache",
                parent.display()
            );
            return;
        }
    }
    if let Err(e) = crate::fs_atomic::atomic_write(path, &bytes) {
        warn!(
            "agent_commands: could not write override cache {} ({e}) — continuing",
            path.display()
        );
    }
}

/// Remove the cache at `path`, ignoring a missing file.
fn clear_cache_at(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => info!(
            "agent_commands: cleared the override cache at {} (no account for this device)",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!(
            "agent_commands: could not clear override cache {} ({e})",
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

/// The list endpoint's envelope. Only `items` is consumed — pagination is
/// irrelevant at N-commands scale and unknown fields are ignored.
#[derive(Debug, Deserialize)]
struct AgentCommandListResponse {
    #[serde(default)]
    items: Vec<AgentCommand>,
}

/// What one attempt at the account layer established.
///
/// The three arms are deliberately distinct: **absent** and **unknown** are not
/// the same fact. `NoAccount` is an authoritative "this device has no account
/// layer" and therefore *invalidates* a cache; `Unavailable` is "could not
/// tell", which is exactly when the cache is the right answer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FetchOutcome {
    /// Authenticated fetch succeeded; this is the account's complete override
    /// set (possibly empty, meaning "no overrides", which is authoritative).
    Fresh(Vec<AgentCommand>),
    /// No usable credential, or the backend rejected it (401/403). The account
    /// layer definitively does not apply to this device right now.
    NoAccount,
    /// Transport error, server error, or an unparseable body — the account
    /// layer is UNKNOWN.
    Unavailable(String),
}

/// Perform the fetch on a dedicated thread with its own current-thread tokio
/// runtime.
///
/// Callers reach this from `provision_fleet_commands_for_session`, which is a
/// *sync* function invoked from *async* spawn paths. `Handle::block_on` panics
/// when called from inside a runtime worker, so the work is moved onto its own
/// thread instead.
///
/// The join is bounded by the reqwest client's own [`FETCH_TIMEOUT`], which
/// covers connect + read for the single request this makes; everything else on
/// that thread is local file IO. There is no separate join deadline, so a
/// hypothetical hang inside reqwest would block the caller — accepted because
/// the alternative (detaching the thread) leaks it on every spawn.
fn fetch_overrides_blocking(base_url: &str) -> FetchOutcome {
    let url = base_url.to_string();
    let handle = std::thread::Builder::new()
        .name("agent-commands-fetch".to_string())
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
            rt.block_on(fetch_overrides_async(&url))
        });
    match handle {
        Ok(h) => h.join().unwrap_or_else(|_| {
            FetchOutcome::Unavailable("the fetch thread panicked".to_string())
        }),
        Err(e) => FetchOutcome::Unavailable(format!("could not spawn a fetch thread: {e}")),
    }
}

/// GET `{base}/api/v1/agent-commands` with the stored bearer.
async fn fetch_overrides_async(base_url: &str) -> FetchOutcome {
    let auth = crate::auth::AuthManager::new();
    let token = match auth.get_access_token() {
        Ok(t) if !t.trim().is_empty() => t,
        Ok(_) | Err(_) => {
            debug!(
                "agent_commands: no stored access token — resolving the embedded defaults \
                 (sign in to use account overrides)"
            );
            return FetchOutcome::NoAccount;
        }
    };

    let client = match reqwest::Client::builder().timeout(FETCH_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => return FetchOutcome::Unavailable(format!("could not build an HTTP client: {e}")),
    };
    let url = format!("{base_url}/api/v1/agent-commands?limit={FETCH_LIMIT}");
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
    match resp.json::<AgentCommandListResponse>().await {
        Ok(body) => FetchOutcome::Fresh(body.items),
        Err(e) => FetchOutcome::Unavailable(format!("GET {url} returned an unreadable body: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Pure resolver: turn one [`FetchOutcome`] plus whatever the cache held into
/// the registry to provision. Split out from [`resolve_registry`] so every
/// resolution gate is testable without a backend or a filesystem.
///
/// Returns the registry and, when the caller should persist something, the
/// cache action to take.
pub(crate) fn resolve_with(
    outcome: FetchOutcome,
    cached: Option<Vec<AgentCommand>>,
) -> (AgentCommandRegistry, CacheAction) {
    let mut registry = AgentCommandRegistry::new();
    match outcome {
        FetchOutcome::Fresh(commands) => {
            let n = registry.set_overrides(commands.clone());
            debug!("agent_commands: fetched {n} account override(s)");
            (registry, CacheAction::Store(commands))
        }
        FetchOutcome::NoAccount => {
            // Authoritative absence: a stale cache from a previous sign-in must
            // not keep overriding the defaults after sign-out.
            (registry, CacheAction::Clear)
        }
        FetchOutcome::Unavailable(why) => {
            match cached {
                Some(commands) => {
                    let n = registry.set_overrides(commands);
                    warn!(
                        "agent_commands: account overrides unavailable ({why}) — serving \
                         {n} cached override(s)"
                    );
                }
                None => {
                    warn!(
                        "agent_commands: account overrides unavailable ({why}) and no usable \
                         cache — serving the embedded defaults"
                    );
                }
            }
            // Never overwrite or drop a cache on an inconclusive fetch.
            (registry, CacheAction::Keep)
        }
    }
}

/// What [`resolve_with`] decided should happen to the on-disk cache.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CacheAction {
    /// Persist this override set as the new cache.
    Store(Vec<AgentCommand>),
    /// Delete any cache — the account layer authoritatively does not apply.
    Clear,
    /// Leave the cache exactly as it is.
    Keep,
}

/// Resolve the command set to provision: fresh fetch → disk cache → embedded
/// defaults.
///
/// Never fails and never panics. Every layer degrades to the next one; the
/// floor is the embedded defaults, which is byte-identically what a device
/// with no account has always received.
pub fn resolve_registry() -> AgentCommandRegistry {
    let base_url = crate::api_config::get_api_base_url();
    let path = cache_path();
    let outcome = fetch_overrides_blocking(&base_url);
    let cached = match (&outcome, &path) {
        // Only pay for the cache read when it can actually be used.
        (FetchOutcome::Unavailable(_), Some(p)) => read_cache_at(p, &base_url),
        _ => None,
    };
    let (registry, action) = resolve_with(outcome, cached);
    if let Some(p) = &path {
        match action {
            CacheAction::Store(commands) => write_cache_at(p, &base_url, &commands),
            CacheAction::Clear => clear_cache_at(p),
            CacheAction::Keep => {}
        }
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(name: &str, body: &str) -> AgentCommand {
        AgentCommand {
            id: format!("id-{name}"),
            organization_id: Some("org-1".to_string()),
            created_by_user_id: Some("user-1".to_string()),
            name: name.to_string(),
            body: body.to_string(),
            checksum: None,
            is_shared: false,
            current_version: 1,
            created_at: "2026-08-04T00:00:00Z".to_string(),
            updated_at: "2026-08-04T00:00:00Z".to_string(),
        }
    }

    /// Gate: a device with no account resolves the embedded defaults, and the
    /// bodies are byte-identical to what `include_str!` embedded.
    #[test]
    fn no_account_resolves_embedded_defaults_byte_identically() {
        let (registry, action) = resolve_with(FetchOutcome::NoAccount, None);
        assert_eq!(action, CacheAction::Clear);
        assert_eq!(registry.override_count(), 0);

        let resolved = registry.all();
        assert_eq!(resolved.len(), crate::fleet_commands::FLEET_COMMANDS.len());
        for (i, (name, body)) in crate::fleet_commands::FLEET_COMMANDS.iter().enumerate() {
            assert_eq!(resolved[i].name, *name);
            assert_eq!(
                resolved[i].body, *body,
                "embedded default {name} must be served byte-identically"
            );
            assert_eq!(resolved[i].source, CommandSource::Builtin);
        }
    }

    /// Gate: with an override, the session gets the override — and the override
    /// REPLACES the default rather than coexisting with it (the deliberate
    /// divergence from `SkillRegistry::all()`'s concatenation).
    #[test]
    fn override_replaces_the_default_by_name() {
        let first = crate::fleet_commands::FLEET_COMMANDS[0].0;
        let (registry, action) =
            resolve_with(FetchOutcome::Fresh(vec![cmd(first, "# mine\n")]), None);
        assert!(matches!(action, CacheAction::Store(_)));

        let resolved = registry.all();
        assert_eq!(
            resolved.len(),
            crate::fleet_commands::FLEET_COMMANDS.len(),
            "an override must REPLACE the default, not be appended alongside it"
        );
        let hit = registry.get(first).expect("override resolves by name");
        assert_eq!(hit.body, "# mine\n");
        assert_eq!(hit.source, CommandSource::Account);
        assert_eq!(hit.file_name(), format!("{first}.md"));

        // Exactly one entry carries that name.
        assert_eq!(resolved.iter().filter(|c| c.name == first).count(), 1);
    }

    /// An account command with no embedded counterpart is additive, not
    /// discarded.
    #[test]
    fn account_only_command_is_additive() {
        let (registry, _) = resolve_with(
            FetchOutcome::Fresh(vec![cmd("my-command", "# mine\n")]),
            None,
        );
        let resolved = registry.all();
        assert_eq!(
            resolved.len(),
            crate::fleet_commands::FLEET_COMMANDS.len() + 1
        );
        assert_eq!(registry.get("my-command").unwrap().body, "# mine\n");
    }

    /// Gate: an override cached with the network down still wins over the
    /// default — and the cache is NOT clobbered by the failed fetch.
    #[test]
    fn cached_override_survives_an_unavailable_backend() {
        let first = crate::fleet_commands::FLEET_COMMANDS[0].0;
        let (registry, action) = resolve_with(
            FetchOutcome::Unavailable("connection refused".to_string()),
            Some(vec![cmd(first, "# cached\n")]),
        );
        assert_eq!(action, CacheAction::Keep);
        assert_eq!(registry.get(first).unwrap().body, "# cached\n");
        assert_eq!(registry.get(first).unwrap().source, CommandSource::Account);
    }

    /// Unavailable with nothing cached is the embedded-default floor.
    #[test]
    fn unavailable_with_no_cache_falls_back_to_defaults() {
        let first = crate::fleet_commands::FLEET_COMMANDS[0].0;
        let (registry, action) =
            resolve_with(FetchOutcome::Unavailable("dns failure".to_string()), None);
        assert_eq!(action, CacheAction::Keep);
        assert_eq!(registry.override_count(), 0);
        assert_eq!(
            registry.get(first).unwrap().source,
            CommandSource::Builtin
        );
    }

    /// Gate: a malformed override falls back to the embedded default (and the
    /// warning is emitted by `set_overrides`).
    #[test]
    fn malformed_override_falls_back_to_the_default() {
        let first = crate::fleet_commands::FLEET_COMMANDS[0].0;
        let default_body = crate::fleet_commands::FLEET_COMMANDS[0].1;

        // Empty body.
        let (registry, _) = resolve_with(FetchOutcome::Fresh(vec![cmd(first, "   \n")]), None);
        assert_eq!(registry.override_count(), 0);
        assert_eq!(registry.get(first).unwrap().body, default_body);
        assert_eq!(registry.get(first).unwrap().source, CommandSource::Builtin);

        // Oversized body.
        let huge = "x".repeat(MAX_BODY_BYTES + 1);
        let (registry, _) = resolve_with(FetchOutcome::Fresh(vec![cmd(first, &huge)]), None);
        assert_eq!(registry.override_count(), 0);
        assert_eq!(registry.get(first).unwrap().body, default_body);
    }

    /// A path-traversal name must never reach the filesystem layer.
    #[test]
    fn traversal_and_separator_names_are_rejected() {
        for bad in [
            "../../evil",
            "..",
            ".",
            "a/b",
            "a\\b",
            "C:evil",
            "vet plan",
            "vet.plan",
            "",
        ] {
            assert!(
                validate_name(bad).is_err(),
                "{bad:?} must be rejected as a command name"
            );
        }
        for good in ["vet-plan", "implement-plan", "my_command", "cmd2"] {
            assert!(validate_name(good).is_ok(), "{good:?} should be accepted");
        }
        // And the rejection reaches the registry, not just the validator.
        let (registry, _) = resolve_with(
            FetchOutcome::Fresh(vec![cmd("../../evil", "# pwn\n")]),
            None,
        );
        assert_eq!(registry.override_count(), 0);
        assert_eq!(
            registry.all().len(),
            crate::fleet_commands::FLEET_COMMANDS.len()
        );
    }

    /// Duplicate names in one payload keep the first and drop the rest — the
    /// resolved set can never contain two entries writing the same file.
    #[test]
    fn duplicate_override_names_collapse() {
        let (registry, _) = resolve_with(
            FetchOutcome::Fresh(vec![cmd("dupe", "# first\n"), cmd("dupe", "# second\n")]),
            None,
        );
        assert_eq!(registry.override_count(), 1);
        assert_eq!(registry.get("dupe").unwrap().body, "# first\n");
    }

    /// An authoritative empty account (`Fresh(vec![])`) means "no overrides",
    /// and it replaces the cache rather than leaving a stale one in place.
    #[test]
    fn empty_fresh_fetch_clears_the_override_layer() {
        let (registry, action) = resolve_with(FetchOutcome::Fresh(vec![]), None);
        assert_eq!(registry.override_count(), 0);
        assert_eq!(action, CacheAction::Store(vec![]));
    }

    /// Cache round-trip, plus the two rejections that keep a cache from being
    /// served across backends or across cache versions.
    #[test]
    fn cache_round_trips_and_refuses_foreign_records() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("nested").join(CACHE_FILE);
        let commands = vec![cmd("vet-plan", "# cached\n")];

        write_cache_at(&path, "https://api.example", &commands);
        let read = read_cache_at(&path, "https://api.example").expect("cache round-trips");
        assert_eq!(read, commands);

        // A different backend must not be served this cache.
        assert!(read_cache_at(&path, "http://127.0.0.1:8000").is_none());

        // A different cache version must not be parsed on a guess.
        let bumped = serde_json::json!({
            "cache_version": CACHE_VERSION + 1,
            "backend_url": "https://api.example",
            "fetched_at": "2026-08-04T00:00:00Z",
            "commands": [],
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

    /// Nothing in the resolved set may assume the bundle is two commands.
    #[test]
    fn registry_does_not_assume_two_commands() {
        let registry = AgentCommandRegistry::new();
        assert_eq!(
            registry.builtin_count(),
            crate::fleet_commands::FLEET_COMMANDS.len()
        );
        assert!(
            registry.builtin_count() >= 1,
            "the bundle must ship at least one command"
        );
    }
}
