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

/// Largest override body accepted, in bytes. A command procedure runs to tens
/// of KB — the seven embedded defaults are ~316 KB combined and the largest
/// single body ~94 KB, measured in BYTES, which is what this cap counts. This
/// cap exists so a malformed or hostile row cannot write an unbounded file
/// into a session cwd.
const MAX_BODY_BYTES: usize = 1024 * 1024;

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

/// Where a resolved command's body came from — **one variant per arm of
/// [`resolve_registry`]'s resolution order**, which is the property that makes
/// this type usable as provenance rather than merely as a label.
///
/// It used to carry two variants (`Builtin` / `Account`) against a resolver
/// with three arms, so a body that came off the wire and a body that came out
/// of `agent-commands-cache.json` were indistinguishable. That collapse is
/// exactly the parity difference plan
/// `2026-08-31-published-build-parity-check` measures: **a published install
/// with no network resolves cached-or-embedded where a dev box resolves
/// served**, and one `Account` variant reports both as the same fact.
/// The old `Account` variant is therefore gone rather than deprecated — a third variant
/// added alongside it would have left the ambiguous one still constructible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSource {
    /// Embedded in this binary via `include_str!` — the floor, present wherever
    /// the binary is.
    Builtin,
    /// Fetched over the network from the signed-in account THIS RUN
    /// (`FetchOutcome::Fresh`).
    Served,
    /// Read from this device's own `agent-commands-cache.json`, written by an
    /// earlier successful fetch. Never carried by the build, and reached only
    /// when the fetch was `FetchOutcome::Unavailable`.
    DiskCache,
}

impl CommandSource {
    /// The stable wire string. Consumed by
    /// [`crate::capability_manifest::CapabilityObservation::from_command_source`]
    /// and by log lines; the retired `"account"` string was consumed by nothing
    /// that pinned it.
    pub fn as_str(self) -> &'static str {
        match self {
            CommandSource::Builtin => "builtin",
            CommandSource::Served => "served",
            CommandSource::DiskCache => "disk_cache",
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
/// Delegates to [`qontinui_types::agent_commands::validate_agent_command_name`],
/// the CANONICAL rule shared by every surface that writes a command name (this
/// registry, the qontinui-web service, the frontend editor). Do not re-implement
/// it here: `name` becomes a path component under `.claude/commands/`, and three
/// independent notions of "valid" is exactly the drift that definition exists to
/// prevent.
///
/// Two properties matter for THIS caller specifically:
///
/// * The charset (lowercase ASCII alphanumerics plus `-`) excludes `/`, `\`, `:`
///   and — critically — `.`, so `..` can never appear and a name can never escape
///   `.claude/commands/`.
/// * It rejects the **Windows reserved device stems** (`nul`, `con`, `aux`,
///   `com1`, …). That arm is not theoretical here: the fleet runs on Windows,
///   where `fs::write` to `nul.md` SUCCEEDS and silently discards, and
///   provisioning is fail-soft and only warns on `Err` — so an override named
///   `nul` would have logged a clean success for a command that does not exist.
fn validate_name(name: &str) -> Result<(), String> {
    qontinui_types::agent_commands::validate_agent_command_name(name).map_err(|e| e.to_string())
}

/// Validate one fetched override into a [`ResolvedCommand`], or explain why it
/// is unusable. A rejected override falls back to the embedded default.
///
/// `source` is the arm that supplied `cmd` — [`CommandSource::Served`] for a
/// live fetch, [`CommandSource::DiskCache`] for a cache replay. It is passed in
/// rather than assumed because this function cannot tell them apart and the
/// difference is the measurement.
fn validate_override(
    cmd: &AgentCommand,
    source: CommandSource,
) -> Result<ResolvedCommand, String> {
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
        source,
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
    /// Which arm of [`resolve_registry`]'s three-rung order actually answered.
    /// See [`AgentCommandRegistry::resolution_arm`].
    resolution_arm: CommandSource,
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
            // Nothing has been layered on yet, so the embedded floor is what
            // answered. `set_overrides` moves this up when an arm supplies one.
            resolution_arm: CommandSource::Builtin,
        }
    }

    /// Install the account layer, dropping (and warning about) any entry that
    /// fails validation. Returns the number of overrides accepted.
    ///
    /// `source` names the arm that supplied `commands` —
    /// [`CommandSource::Served`] for a live fetch, [`CommandSource::DiskCache`]
    /// for a cache replay. It is a required argument rather than a default
    /// because the two are the parity difference this type exists to report,
    /// and a default would silently pick one of them.
    ///
    /// It is recorded as the registry's [`resolution_arm`](Self::resolution_arm)
    /// even when zero overrides survive validation: an arm that answered and
    /// supplied nothing usable is a different (and more interesting) fact than
    /// an arm that was never reached, and collapsing them would re-create the
    /// blindness this signature change removed.
    ///
    /// Never fails: a wholly malformed payload yields zero overrides, which is
    /// the embedded-default state for the BODIES while still recording which
    /// arm produced them.
    pub fn set_overrides(&mut self, commands: Vec<AgentCommand>, source: CommandSource) -> usize {
        debug_assert!(
            source != CommandSource::Builtin,
            "the embedded floor is not an override layer; pass Served or DiskCache"
        );
        let mut accepted: Vec<ResolvedCommand> = Vec::with_capacity(commands.len());
        let mut seen: HashSet<String> = HashSet::new();
        for cmd in &commands {
            match validate_override(cmd, source) {
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
        self.resolution_arm = source;
        self.overrides.len()
    }

    /// Which arm of [`resolve_registry`]'s `fresh fetch → disk cache → embedded
    /// default` order answered for this registry.
    ///
    /// This is the value the capability manifest carries for
    /// `agent_commands_registry`, and the three arms map to three DISTINCT
    /// rungs — `served`, `disk_cache`, `embedded`. Read it together with
    /// [`override_count`](Self::override_count): a [`CommandSource::Served`]
    /// arm with zero overrides means the account authoritatively has none, so
    /// every BODY is still the embedded default even though the served arm is
    /// what established that.
    #[must_use]
    pub fn resolution_arm(&self) -> CommandSource {
        self.resolution_arm
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
        let builtin_names: HashSet<&str> = self.builtin.iter().map(|c| c.name.as_str()).collect();
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
        Ok(h) => h
            .join()
            .unwrap_or_else(|_| FetchOutcome::Unavailable("the fetch thread panicked".to_string())),
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
            let n = registry.set_overrides(commands.clone(), CommandSource::Served);
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
                    let n = registry.set_overrides(commands, CommandSource::DiskCache);
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
///
/// **Which of the three arms answered is now a value**, not just a log line:
/// read it off the returned registry with
/// [`AgentCommandRegistry::resolution_arm`]. The caller
/// (`fleet_commands::provision_fleet_commands_for_session`) turns it into the
/// capability manifest's `agent_commands_registry` row. Before plan
/// `2026-08-31-published-build-parity-check` Phase 3 nothing reported it at
/// all, so a published install falling back to its cache and a dev box
/// resolving off the network were indistinguishable from outside.
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
    info!(
        "agent_commands: resolved via the {} arm ({} override(s) over {} embedded default(s))",
        registry.resolution_arm().as_str(),
        registry.override_count(),
        registry.builtin_count(),
    );
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
        // A LIVE fetch, so the body is `served` — not the same fact as the
        // cached arm below, which the retired `Account` variant could not say.
        assert_eq!(hit.source, CommandSource::Served);
        assert_eq!(registry.resolution_arm(), CommandSource::Served);
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
        // The DISK CACHE answered, not the network — the distinction a
        // published install with no backend depends on being able to state.
        assert_eq!(registry.get(first).unwrap().source, CommandSource::DiskCache);
        assert_eq!(registry.resolution_arm(), CommandSource::DiskCache);
    }

    /// Unavailable with nothing cached is the embedded-default floor.
    #[test]
    fn unavailable_with_no_cache_falls_back_to_defaults() {
        let first = crate::fleet_commands::FLEET_COMMANDS[0].0;
        let (registry, action) =
            resolve_with(FetchOutcome::Unavailable("dns failure".to_string()), None);
        assert_eq!(action, CacheAction::Keep);
        assert_eq!(registry.override_count(), 0);
        assert_eq!(registry.get(first).unwrap().source, CommandSource::Builtin);
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
            // Windows reserved device stems. NOT theoretical: the fleet runs on
            // Windows, where fs::write to `nul.md` SUCCEEDS and discards, and
            // provisioning is fail-soft (warns on Err only) — so accepting one
            // would log a clean success for a command that does not exist.
            "nul",
            "NUL",
            "con",
            "aux",
            "com1",
            "prn",
            "lpt1",
            // Rejected by the canonical slug rule: uppercase and '_' are not
            // part of the shared charset, so a name that resolves on one
            // surface can never be rejected by another.
            "Vet-Plan",
            "vet_plan",
        ] {
            assert!(
                validate_name(bad).is_err(),
                "{bad:?} must be rejected as a command name"
            );
        }
        // `my_command` was accepted before this module delegated to the
        // canonical validator; the shared charset is lowercase alnum + '-'
        // only, so an underscore is now correctly rejected (asserted above).
        for good in ["vet-plan", "implement-plan", "my-command", "cmd2"] {
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

    /// Gate for plan `2026-08-31-published-build-parity-check` Phase 3: the
    /// THREE arms of `resolve_registry` report THREE distinct sources, and the
    /// capability manifest turns each into a distinct rung.
    ///
    /// This is the whole point of retiring `CommandSource::Account`: before it,
    /// the first two arms below were the same value, so "a published install
    /// with no network resolved from its cache" and "a dev box resolved from
    /// the network" were the same reading.
    #[test]
    fn the_three_resolution_arms_report_three_distinct_sources() {
        use crate::capability_manifest::Rung;

        let first = crate::fleet_commands::FLEET_COMMANDS[0].0;

        // Arm 1 — a live fetch answered.
        let (served, _) = resolve_with(FetchOutcome::Fresh(vec![cmd(first, "# wire\n")]), None);
        // Arm 2 — the fetch failed and the on-disk cache answered.
        let (cached, _) = resolve_with(
            FetchOutcome::Unavailable("connection refused".to_string()),
            Some(vec![cmd(first, "# cached\n")]),
        );
        // Arm 3 — nothing above answered; the embedded floor did.
        let (embedded, _) = resolve_with(FetchOutcome::NoAccount, None);

        let arms = [
            served.resolution_arm(),
            cached.resolution_arm(),
            embedded.resolution_arm(),
        ];
        assert_eq!(
            arms,
            [
                CommandSource::Served,
                CommandSource::DiskCache,
                CommandSource::Builtin
            ]
        );

        let rungs: Vec<Rung> = arms.iter().map(|s| Rung::from(*s)).collect();
        assert_eq!(rungs, vec![Rung::Served, Rung::DiskCache, Rung::Embedded]);
        let distinct: HashSet<&'static str> = rungs.iter().map(|r| r.wire()).collect();
        assert_eq!(
            distinct.len(),
            3,
            "the three arms must not collapse onto one rung — that collapse IS \
             the parity blindness this phase removed"
        );
    }

    /// An `Unavailable` fetch with NO usable cache falls all the way to the
    /// embedded floor, and says so — it must not be reported as `disk_cache`
    /// merely because the cache was the arm that was tried.
    #[test]
    fn unavailable_with_no_cache_reports_the_embedded_arm() {
        let (registry, _) =
            resolve_with(FetchOutcome::Unavailable("dns failure".to_string()), None);
        assert_eq!(registry.resolution_arm(), CommandSource::Builtin);
    }

    /// An authoritative `Fresh(vec![])` records the SERVED arm even though every
    /// resolved body is the embedded default — "the account has no overrides"
    /// is a network reading, not an absence of one.
    #[test]
    fn an_empty_fresh_fetch_still_records_the_served_arm() {
        let (registry, _) = resolve_with(FetchOutcome::Fresh(vec![]), None);
        assert_eq!(registry.override_count(), 0);
        assert_eq!(registry.resolution_arm(), CommandSource::Served);
        // ...and the BODIES are still stated, per command, as embedded.
        let first = crate::fleet_commands::FLEET_COMMANDS[0].0;
        assert_eq!(registry.get(first).unwrap().source, CommandSource::Builtin);
    }

    /// Every `CommandSource` round-trips its wire string, and the three strings
    /// are distinct. The wire values are read by the capability manifest, so a
    /// silent rename would diff two builds as different when they are not.
    #[test]
    fn command_source_round_trips_its_wire_strings() {
        let all = [
            CommandSource::Builtin,
            CommandSource::Served,
            CommandSource::DiskCache,
        ];
        let wires: Vec<&'static str> = all.iter().map(|s| s.as_str()).collect();
        assert_eq!(wires, vec!["builtin", "served", "disk_cache"]);
        assert_eq!(
            wires.iter().collect::<HashSet<_>>().len(),
            all.len(),
            "two variants sharing a wire string would be indistinguishable on the wire"
        );
        for source in all {
            // Exhaustive by construction: a new variant fails to compile here
            // rather than silently escaping the round-trip.
            let round_tripped = match source.as_str() {
                "builtin" => CommandSource::Builtin,
                "served" => CommandSource::Served,
                "disk_cache" => CommandSource::DiskCache,
                other => panic!("unmapped CommandSource wire string {other:?}"),
            };
            assert_eq!(round_tripped, source);
        }
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
