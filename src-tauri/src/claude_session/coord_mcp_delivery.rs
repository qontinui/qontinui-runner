//! The coord-mcp a stream-json [`super::ClaudeSession`] child is given, and the
//! lifetime of what that delivery minted (plan
//! `2026-11-21-sessions-spawn-takes-the-parent-mcp-json-and-acts-as-the-default-tenant`,
//! Phase 2).
//!
//! ## The defect this closes
//!
//! `ClaudeSession::spawn` used to pass no `--mcp-config` and write no
//! `.mcp.json`. Claude Code resolves a project `.mcp.json` from an ANCESTOR of
//! its cwd, so a session spawned into a worktree with none loaded the
//! workspace-root file — whose key the proxy resolves to the device's DEFAULT
//! tenant. Measured 2026-11-21 (finding `9ab6bae0`): a session in a
//! bauhaus-b2b worktree answered `coord_query_identity` as qontinui and was
//! served qontinui's policy, with no notice.
//!
//! ## What the session gets now
//!
//! The terminal seam's own delivery
//! ([`crate::coord_mcp::deliver_session_coord_mcp`]), keyed by the AI session
//! id instead of a terminal id, rendered into argv here because a direct-exec
//! child has no identity shim to render it:
//!
//! | Delivery | argv | Why |
//! |---|---|---|
//! | `Provisioned` | `--mcp-config <runner file>` | A `--mcp-config` `coord-mcp` REPLACES an ancestor `.mcp.json`'s same-name entry (finding `a1a66bfe` case b, 3 of 3 runs), so the additive flag is enough and every other server the session would load survives. |
//! | `WorkdirDeclared` | nothing | The cwd's own `.mcp.json` beats the ancestor's (case f), so the declared key is the session's, exactly as on the terminal path. |
//! | `Unprovisioned` | `--strict-mcp-config --mcp-config <carry-through file>` | Fails closed to NO coord-mcp, never to the parent's: the file carries every non-coord server the session would otherwise have loaded, and no `coord-mcp`. |
//!
//! The runner-file nonce is minted with NO spawn tenant, so the proxy decides
//! the tenant per request — and for a cwd that declares a tenant this runner
//! cannot act as, every coord call is the proxy's typed refusal instead of a
//! write into the default tenant.
//!
//! ## Lifetime
//!
//! The mint is a PERSISTENT device nonce, evicted only by a re-mint of the
//! same (workdir, key) — and an AI session's key (its task-run id) is never
//! reused. So every delivery is paired with a [`CoordMcpLease`] that revokes
//! the nonce, deletes the files and drops the delivery record when the child
//! exits (the waiter thread owns it), when the session is closed
//! ([`retire_session_coord_mcp`]), or when the spawn fails before a waiter
//! exists (the lease is dropped). Every retire is idempotent and touches only
//! what THIS delivery minted, so a successor delivered under the same key (a
//! rate-limit restart) is never retired by its predecessor.
//!
//! ## What this does not reach yet
//!
//! Until Phase 4 of the plan lands (`qontinui-pr` reads `QONTINUI_MCP_CONFIG`
//! before any walk-up), the shell-side doors that find a credential by walking
//! up for a `.mcp.json` — `qontinui-pr`, `coord-revive.sh` — still present the
//! ancestor's key from inside such a session, whatever its own coord-mcp is.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use tracing::{info, warn};

use crate::coord_mcp::{CoordMcpDelivery, SpawnTenantRefusal};

/// The AI session id the child exports, so the bundled SessionStart policy
/// hook can address `GET /sessions/{id}/policy-context` with the key its nonce
/// was recorded under (plan D3).
pub(crate) const AI_SESSION_ID_ENV: &str = "QONTINUI_AI_SESSION_ID";

/// The runner API port the bundled hooks call. Without it the policy hook
/// exits before it asks anything (`claude_policy_hook.sh`: no port, exit 0).
pub(crate) const RUNNER_API_PORT_ENV: &str = "QONTINUI_RUNNER_API_PORT";

/// The terminal id a TERMINAL's hooks key on. A stream-json child has none; an
/// inherited value (a runner launched from another runner's terminal) would
/// address the policy route as that terminal, so the child never carries one.
const TERMINAL_ID_ENV: &str = "QONTINUI_TERMINAL_ID";

/// The MCP server name the runner owns.
const COORD_MCP_SERVER: &str = "coord-mcp";

/// Where the unprovisioned arm's carry-through files live, under
/// [`crate::session::claude_hook::session_restore_dir`]. Not the `coord-mcp/`
/// dir: those files carry nonces and have reapers of their own; these carry
/// none.
const STRICT_CONFIG_DIR: &str = "claude-session-mcp";

/// The MCP half of the template flags a stream-json spawn OWNS on every
/// coord-mcp arm (the full set is `launch_extras::OWNED_TEMPLATE_FLAGS`)
/// ([`super::launch_spec::LaunchSpec::owned_flags`]): a template's copy never
/// reaches the argv. On the provisioned and strict arms the runner supplies its
/// own; on the declared arm it supplies none because the cwd's `.mcp.json` IS
/// the session's config (a cwd file beats the ancestor, finding `a1a66bfe`
/// case f) — and a template `--mcp-config` or `--strict-mcp-config` would
/// override or discard it.
pub(crate) const OWNED_MCP_TEMPLATE_FLAGS: [&str; 2] = ["--mcp-config", "--strict-mcp-config"];

/// What one stream-json spawn was given for coord-mcp.
#[derive(Debug)]
pub(crate) struct AiSessionCoordMcp {
    delivery: CoordMcpDelivery,
    /// The MCP flags for the child's argv (see the module table).
    argv: Vec<String>,
    /// The config file the child loads, exported as
    /// [`crate::coord_mcp::MCP_CONFIG_ENV`] (`None` on the declared arm, where
    /// the cwd's own file is the session's).
    config_file: Option<PathBuf>,
    lease: CoordMcpLease,
}

impl AiSessionCoordMcp {
    /// The per-session outcome the briefing is gated on.
    pub(crate) fn delivery(&self) -> CoordMcpDelivery {
        self.delivery
    }

    /// The MCP flags for the child's argv.
    pub(crate) fn argv(&self) -> &[String] {
        &self.argv
    }

    /// The env the child needs beside the argv: `(name, Some(value))` to set,
    /// `(name, None)` to remove. Set BEFORE `ClaudeSession::finalize_child_env`,
    /// which is the last env write and whose scrub keeps all of these.
    pub(crate) fn child_env(
        &self,
        session_id: &str,
        api_port: u16,
    ) -> Vec<(&'static str, Option<String>)> {
        vec![
            (
                crate::coord_mcp::MCP_CONFIG_ENV,
                self.config_file
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
            ),
            (RUNNER_API_PORT_ENV, Some(api_port.to_string())),
            (AI_SESSION_ID_ENV, Some(session_id.to_string())),
            (TERMINAL_ID_ENV, None),
        ]
    }

    /// Hand the lease to whatever owns the child's lifetime (the waiter).
    pub(crate) fn into_lease(self) -> CoordMcpLease {
        self.lease
    }
}

/// Give the stream-json session `session_key` (its session id) in `cwd` its
/// coord-mcp — see the module table.
///
/// `config_dir` is the account config dir the child runs under (`None`: the
/// ambient default), read only on the unprovisioned arm, for the user- and
/// local-scope servers the strict file carries. `bound_port` is injected
/// (production passes [`crate::coord_mcp::resolve_bound_api_port`]).
///
/// `Err` refuses the spawn before the child exists: a spawn-tenant refusal
/// (unreachable with the `None` spawn tenant this passes, and kept typed so a
/// future caller that names a tenant cannot lose it), or an unprovisioned arm
/// whose strict file could not be written — a child with no `--mcp-config`
/// would load the ancestor's coord-mcp, which is the defect itself.
pub(crate) fn deliver_for_ai_session(
    cwd: &str,
    session_key: &str,
    config_dir: Option<&str>,
    bound_port: Option<u16>,
) -> Result<AiSessionCoordMcp, String> {
    let delivered = crate::coord_mcp::deliver_session_coord_mcp(cwd, session_key, None, bound_port)
        .map_err(|refusal: SpawnTenantRefusal| {
            warn!(session_id = %session_key, cwd = %cwd, "{refusal}");
            refusal.to_string()
        })?;
    let mut record = LeaseRecord {
        generation: delivered.generation,
        cwd: cwd.to_string(),
        nonce: None,
        runner_config: None,
        strict_config: None,
    };
    let (argv, config_file) = match (delivered.delivery, delivered.config_path) {
        (CoordMcpDelivery::WorkdirDeclared, _) => (Vec::new(), None),
        (_, Some(path)) if !delivered.strict => {
            // The nonce the mint returned — never re-read from the file, which a
            // successor under the same key may already have rewritten.
            record.nonce = delivered.nonce.clone();
            record.runner_config = Some(path.clone());
            (
                vec![
                    "--mcp-config".to_string(),
                    path.to_string_lossy().into_owned(),
                ],
                Some(path),
            )
        }
        _ => {
            // Unprovisioned (the delivery has already left the NOT PROVISIONED
            // breadcrumb in the cwd). Register the lease first, so a failed
            // write below still releases the delivery record on drop.
            let mut lease = CoordMcpLease::register(session_key, record);
            let path = write_strict_config(cwd, session_key, lease.generation(), config_dir)
                .map_err(|e| {
                    format!(
                        "coord-mcp: session {session_key}: no coord-mcp could be provisioned \
                         and the fail-closed config could not be written ({e}) — refusing the \
                         spawn, because a child with no --mcp-config would load an ancestor \
                         .mcp.json's coord-mcp, which acts as the device's default tenant"
                    )
                })?;
            lease.set_strict_config(&path);
            warn!(
                session_id = %session_key,
                cwd = %cwd,
                "coord-mcp: no coord-mcp provisioned — the session starts with \
                 --strict-mcp-config and NO coord-mcp (never an ancestor .mcp.json's)"
            );
            return Ok(AiSessionCoordMcp {
                delivery: delivered.delivery,
                argv: vec![
                    "--strict-mcp-config".to_string(),
                    "--mcp-config".to_string(),
                    path.to_string_lossy().into_owned(),
                ],
                config_file: Some(path),
                lease,
            });
        }
    };
    info!(
        session_id = %session_key,
        delivery = ?delivered.delivery,
        "coord-mcp: stream-json session coord-mcp delivered"
    );
    Ok(AiSessionCoordMcp {
        delivery: delivered.delivery,
        argv,
        config_file,
        lease: CoordMcpLease::register(session_key, record),
    })
}

/// The working directory the coord-mcp delivery recorded for the live AI
/// session `session_key`, or `None` when no delivery is live under that key.
///
/// The SessionStart policy route's AI-session leg
/// ([`crate::mcp::policy_tenant`]): it keys on exactly what the nonce was
/// recorded under (the session id the child exports), which a task-run-keyed
/// registry would miss for a suffixed phase-session id.
pub(crate) fn session_workdir(session_key: &str) -> Option<String> {
    leases()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(session_key)
        .map(|r| r.cwd.clone())
}

/// Retire whatever coord-mcp delivery is live under `session_key` — the
/// session-close half. Idempotent: a key with nothing live is a no-op.
/// Returns whether a delivery was retired.
pub(crate) fn retire_session_coord_mcp(session_key: &str) -> bool {
    let record = leases()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(session_key)
        .cloned();
    match record {
        Some(record) => {
            retire(session_key, &record);
            true
        }
        None => false,
    }
}

// ===========================================================================
// The lease
// ===========================================================================

/// What one delivery minted and wrote, so it can be undone exactly. `Debug` is
/// hand-written so the nonce never reaches a log line.
#[derive(Clone)]
struct LeaseRecord {
    generation: u64,
    cwd: String,
    /// The runner-file nonce (`None` on the declared and unprovisioned arms).
    nonce: Option<String>,
    /// The app-data `--mcp-config` file carrying [`Self::nonce`].
    runner_config: Option<PathBuf>,
    /// The unprovisioned arm's carry-through file.
    strict_config: Option<PathBuf>,
}

impl std::fmt::Debug for LeaseRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseRecord")
            .field("generation", &self.generation)
            .field("cwd", &self.cwd)
            .field("nonce", &self.nonce.as_ref().map(|_| "<redacted>"))
            .field("runner_config", &self.runner_config)
            .field("strict_config", &self.strict_config)
            .finish()
    }
}

/// The live deliveries, by session key. One entry per live AI session.
fn leases() -> &'static Mutex<HashMap<String, LeaseRecord>> {
    static LEASES: OnceLock<Mutex<HashMap<String, LeaseRecord>>> = OnceLock::new();
    LEASES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Ownership of one delivery. Dropping it retires the delivery — the nonce
/// revoked, the files deleted, the delivery record dropped — so every path
/// that loses the session (a spawn error, the child's exit, a close) undoes
/// what the spawn minted, with no path that forgets to.
///
/// It carries its own copy of the record, so a predecessor superseded under
/// the same key (whose table entry its successor replaced) still revokes the
/// nonce IT minted.
#[derive(Debug)]
pub(crate) struct CoordMcpLease {
    key: String,
    record: LeaseRecord,
}

impl CoordMcpLease {
    fn register(key: &str, record: LeaseRecord) -> Self {
        leases()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string(), record.clone());
        Self {
            key: key.to_string(),
            record,
        }
    }

    fn generation(&self) -> u64 {
        self.record.generation
    }

    fn set_strict_config(&mut self, path: &Path) {
        self.record.strict_config = Some(path.to_path_buf());
        if let Some(record) = leases()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&self.key)
            .filter(|r| r.generation == self.record.generation)
        {
            record.strict_config = Some(path.to_path_buf());
        }
    }

    /// Retire now (the same as dropping it; named for call sites where the
    /// ORDER matters — the waiter retires before a rate-limit restart
    /// re-delivers under the same key).
    pub(crate) fn retire(self) {
        drop(self);
    }
}

impl Drop for CoordMcpLease {
    fn drop(&mut self) {
        // Idempotent: after a close already retired it, every step finds
        // nothing of this delivery's left.
        retire(&self.key, &self.record);
    }
}

/// Undo one delivery. Every step touches only what `record` minted:
/// - the nonce is revoked by VALUE (a successor's nonce under the same key is
///   another value);
/// - the runner file is deleted only while it still carries that nonce (a
///   successor in the same cwd rewrites the same path);
/// - the strict file's name carries the generation, so it is this delivery's;
/// - the delivery record and the lease entry go only if they are still this
///   generation's.
fn retire(key: &str, record: &LeaseRecord) {
    if let Some(nonce) = &record.nonce {
        crate::coord_mcp::revoke_proxy_nonce(nonce);
    }
    if let Some(path) = &record.strict_config {
        remove_quietly(path);
    }
    // The runner file's compare-and-remove runs under the SAME lock as every
    // write of such a file (`coord_mcp::app_data_config_lock`). A successor
    // under this key in the same cwd rewrites this exact path; with the lock,
    // its write lands either before the read below (the file then carries the
    // successor's nonce, which is not ours, and is kept) or after the unlink
    // (and recreates it). So a retire only ever deletes a file still carrying
    // the nonce THIS delivery minted.
    if let (Some(path), Some(nonce)) = (&record.runner_config, &record.nonce) {
        if let Err(e) =
            crate::coord_mcp::remove_config_if_nonce_unchanged(path, Some(nonce.as_str()))
        {
            warn!(
                "coord-mcp: could not remove {} on retire: {e}",
                path.display()
            );
        }
    }
    let forgot = crate::coord_mcp::forget_session_coord_mcp_delivery(key, record.generation);
    let mut leases = leases().lock().unwrap_or_else(|e| e.into_inner());
    if leases
        .get(key)
        .is_some_and(|r| r.generation == record.generation)
    {
        leases.remove(key);
    }
    drop(leases);
    if forgot {
        info!(session_id = %key, "coord-mcp: stream-json session coord-mcp retired");
    }
}

fn remove_quietly(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!(
            "coord-mcp: could not remove {} on retire: {e}",
            path.display()
        ),
    }
}

// ===========================================================================
// The unprovisioned arm's carry-through file
// ===========================================================================

/// Write the strict arm's config: every non-coord MCP server the session would
/// load without `--strict-mcp-config`, and no `coord-mcp`. Owner-only, because
/// a server entry can carry headers or env secrets.
fn write_strict_config(
    cwd: &str,
    session_key: &str,
    generation: u64,
    config_dir: Option<&str>,
) -> std::io::Result<PathBuf> {
    let servers = carry_through_servers(
        Path::new(cwd),
        user_config_file(config_dir).as_deref(),
        user_settings_file(config_dir).as_deref(),
    );
    let dir = crate::session::claude_hook::session_restore_dir().join(STRICT_CONFIG_DIR);
    std::fs::create_dir_all(&dir)?;
    if let Err(e) = crate::fs_perms::restrict_dir_to_owner(&dir) {
        warn!(
            "coord-mcp: could not restrict {} to owner-only: {e}",
            dir.display()
        );
    }
    let path = dir.join(strict_config_file_name(session_key, generation));
    let doc = serde_json::json!({ "mcpServers": serde_json::Value::Object(servers) });
    crate::fs_perms::write_owner_only(
        &path,
        serde_json::to_string_pretty(&doc)
            .unwrap_or_default()
            .as_bytes(),
    )?;
    Ok(path)
}

/// `strict-<hash(key)>-<generation>.json`: short on Windows, and the
/// generation makes it this delivery's alone.
fn strict_config_file_name(session_key: &str, generation: u64) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    session_key.hash(&mut h);
    format!("strict-{:016x}-{generation}.json", h.finish())
}

/// The account config file the child reads its user- and local-scope servers
/// from: `<config_dir>/.claude.json`, else the ambient default
/// (`~/.claude.json`) — the same resolution the workspace pre-trust writes to.
fn user_config_file(config_dir: Option<&str>) -> Option<PathBuf> {
    let file = super::workspace_trust::CONFIG_FILE;
    match config_dir {
        Some(dir) => Some(Path::new(dir).join(file)),
        None => dirs::home_dir().map(|home| home.join(file)),
    }
}

/// The user-scope settings file Claude Code reads approvals from:
/// `<config_dir>/settings.json`, else `~/.claude/settings.json`.
fn user_settings_file(config_dir: Option<&str>) -> Option<PathBuf> {
    match config_dir {
        Some(dir) => Some(Path::new(dir).join("settings.json")),
        None => dirs::home_dir().map(|home| home.join(".claude").join("settings.json")),
    }
}

/// The project-scope settings files that can carry approvals for `cwd`, as
/// `(local, project)`: `.claude/settings.local.json` and `.claude/settings.json`
/// in the cwd and in its project root (the enclosing git root), each read once,
/// nearest directory first.
fn project_settings_files(cwd: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut dirs = vec![cwd.to_path_buf()];
    if let Some(root) = std::fs::canonicalize(cwd)
        .ok()
        .and_then(|c| super::workspace_trust::git_root(&c))
    {
        let canonical_cwd = std::fs::canonicalize(cwd).ok();
        if canonical_cwd.as_ref() != Some(&root) {
            dirs.push(root);
        }
    }
    (
        dirs.iter()
            .map(|d| d.join(".claude").join("settings.local.json"))
            .collect(),
        dirs.iter()
            .map(|d| d.join(".claude").join("settings.json"))
            .collect(),
    )
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

/// Every non-coord MCP server a session in `cwd` loads without
/// `--strict-mcp-config`, in Claude Code's scope precedence: local
/// (`projects[<project>].mcpServers` in the account file) over project (the
/// `.mcp.json` chain, nearest directory first) over user (the account file's
/// top-level `mcpServers`). The first scope that names a server wins.
fn carry_through_servers(
    cwd: &Path,
    user_config: Option<&Path>,
    user_settings: Option<&Path>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    let account: Option<serde_json::Value> = user_config
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok());
    let keys = local_scope_keys(cwd);
    let projects = account
        .as_ref()
        .and_then(|a| a.get(super::workspace_trust::PROJECTS_KEY));
    // This project's entry: what local-scope servers it adds, and which
    // project-scope (`.mcp.json`) servers the account approved for it.
    let project = projects.and_then(|p| keys.iter().find_map(|k| p.get(k)));
    // Highest precedence first: local settings, project settings, the account
    // file's projects entry, user settings. (The projects entry's place between
    // project and user is an assumption, unmeasured — see `ProjectApproval`.
    // The command-line `--settings` layer the runner itself passes is NOT
    // read here, so an approval only a folded template layer carries is
    // missed, so the strict file can differ from what that layer approves in
    // either direction: it can omit a server only that layer enables, or
    // carry one only that layer disables. Tracked as a plan follow-up.)
    let (local_files, project_files) = project_settings_files(cwd);
    let local: Vec<serde_json::Value> = local_files.iter().filter_map(|p| read_json(p)).collect();
    let shared: Vec<serde_json::Value> =
        project_files.iter().filter_map(|p| read_json(p)).collect();
    let user: Option<serde_json::Value> = user_settings.and_then(read_json);
    let approval = ProjectApproval::of(
        local
            .iter()
            .chain(shared.iter())
            .chain(project)
            .chain(user.iter()),
    );
    let mut take = |servers: Option<&serde_json::Value>, admit: &dyn Fn(&str) -> bool| {
        let Some(servers) = servers.and_then(|v| v.as_object()) else {
            return;
        };
        for (name, entry) in servers {
            if name != COORD_MCP_SERVER && !out.contains_key(name) && admit(name) {
                out.insert(name.clone(), entry.clone());
            }
        }
    };
    if let Some(projects) = projects {
        for key in &keys {
            take(
                projects.get(key).and_then(|p| p.get("mcpServers")),
                &|_: &str| true,
            );
        }
    }
    for dir in cwd.ancestors() {
        let doc: Option<serde_json::Value> = std::fs::read_to_string(dir.join(".mcp.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        take(
            doc.as_ref().and_then(|d| d.get("mcpServers")),
            &|name: &str| approval.admits(name, &dir.join(".mcp.json")),
        );
    }
    take(
        account.as_ref().and_then(|a| a.get("mcpServers")),
        &|_: &str| true,
    );
    out
}

/// The approval state for one project's `.mcp.json` servers, from the layers
/// this function reads: `.claude/settings.local.json`, `.claude/settings.json`,
/// the account config's `projects[<key>]` entry and the user's `settings.json`,
/// in that order of precedence. Local > project > user is Claude Code's
/// documented settings order; the account file's projects entry is placed
/// between project and user by assumption, unmeasured. A server named in
/// `disabledMcpjsonServers` in
/// ANY layer never loads (disabled wins over any enable). Otherwise it loads
/// when it is named in `enabledMcpjsonServers` in any layer, or when
/// `enableAllProjectMcpServers` is true in the HIGHEST-precedence layer that
/// sets it — a `false` there beats a `true` below it. The strict file carries
/// exactly what was approved.
struct ProjectApproval {
    enable_all: bool,
    enabled: Vec<String>,
    disabled: Vec<String>,
}

impl ProjectApproval {
    /// `layers` highest precedence first.
    fn of<'a>(layers: impl Iterator<Item = &'a serde_json::Value> + Clone) -> Self {
        let names = |key: &str| -> Vec<String> {
            layers
                .clone()
                .filter_map(|l| l.get(key).and_then(|v| v.as_array()))
                .flatten()
                .filter_map(|n| n.as_str().map(str::to_string))
                .collect()
        };
        Self {
            // The first (highest-precedence) layer that sets it decides.
            enable_all: layers
                .clone()
                .find_map(|l| {
                    l.get("enableAllProjectMcpServers")
                        .and_then(|v| v.as_bool())
                })
                .unwrap_or(false),
            enabled: names("enabledMcpjsonServers"),
            disabled: names("disabledMcpjsonServers"),
        }
    }

    /// Does the approval state admit `name`, a server `source` declares?
    /// Every exclusion is logged at info: a project server the strict file
    /// leaves out is a capability the session will not have.
    fn admits(&self, name: &str, source: &Path) -> bool {
        if self.disabled.iter().any(|d| d == name) {
            info!(
                "coord-mcp: strict carry-through excludes project MCP server {name} ({}): it is \
                 disabled",
                source.display()
            );
            return false;
        }
        let approved = self.enable_all || self.enabled.iter().any(|e| e == name);
        if !approved {
            info!(
                "coord-mcp: strict carry-through excludes project MCP server {name} ({}): it has \
                 no approval (not in enabledMcpjsonServers, and enableAllProjectMcpServers is not \
                 set)",
                source.display()
            );
        }
        approved
    }
}

/// The `projects` keys a local-scope server for `cwd` can sit under: the key
/// the CLI uses for its project (the enclosing git root), then the cwd itself.
fn local_scope_keys(cwd: &Path) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(key) = super::workspace_trust::project_key(cwd) {
        keys.push(key);
    }
    if let Ok(resolved) = std::fs::canonicalize(cwd) {
        let key = super::workspace_trust::to_key_string(&resolved);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

#[cfg(test)]
#[path = "coord_mcp_delivery_tests.rs"]
mod tests;
