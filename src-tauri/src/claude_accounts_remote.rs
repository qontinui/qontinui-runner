//! Fetches the server-side Claude account roster (qontinui-web
//! `GET /api/v1/claude-accounts`) and merges the resolved local directories
//! into the machine-global `claude-accounts.json` roster
//! (`crate::claude_accounts`), which the existing account-rotation/spawn
//! logic (`ai_provider::account_usage::resolve_requested_account`) already
//! reads unchanged — this module only changes where the roster of account
//! NAMES comes from.
//!
//! Mirrors `agent_commands::mod`'s fetch shape (bearer from
//! `crate::auth::AuthManager`, base URL from `crate::api_config`), fail-soft
//! at every step: a network/auth/parse error just skips the merge this run,
//! leaving whatever local roster (installer-generated, or a prior fetch)
//! already in place. Never blocks startup, never panics.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use tracing::{debug, info, warn};

/// Wall-clock budget for the whole fetch. This runs once, off a boot
/// background task (see `mcp_api::start_server`), so nobody is blocked on
/// it — the bound exists only so a black-holed backend does not leak the
/// task forever.
const FETCH_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Debug, Deserialize)]
struct RemoteAccountEntry {
    account_key: String,
    #[serde(default)]
    dir_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RosterResponse {
    #[serde(default)]
    accounts: Vec<RemoteAccountEntry>,
}

/// Root directory containing the per-account `.claude-<id>` dirs on this
/// machine. Mirrors `install-claude-accounts.sh`'s own resolution exactly
/// (`QONTINUI_CLAUDE_ACCOUNTS_ROOT` override, else a platform default) so
/// both mechanisms agree about where an account dir lives.
fn accounts_root() -> PathBuf {
    if let Ok(dir) = std::env::var("QONTINUI_CLAUDE_ACCOUNTS_ROOT") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    if cfg!(windows) {
        PathBuf::from("C:\\claude")
    } else {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
    }
}

/// GET `{base}/api/v1/claude-accounts` with the stored device bearer.
/// `None` on any failure — no usable token, transport error, non-2xx, or an
/// unparseable body.
async fn fetch_roster_async(base_url: &str) -> Option<Vec<RemoteAccountEntry>> {
    let auth = crate::auth::AuthManager::new();
    let token = match auth.get_access_token() {
        Ok(t) if !t.trim().is_empty() => t,
        Ok(_) | Err(_) => {
            debug!(
                "claude_accounts_remote: no stored access token — skipping the server \
                 roster fetch this run"
            );
            return None;
        }
    };

    let client = match reqwest::Client::builder().timeout(FETCH_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            warn!("claude_accounts_remote: could not build an HTTP client: {e}");
            return None;
        }
    };
    let url = format!("{base_url}/api/v1/claude-accounts");
    let resp = match client.get(&url).bearer_auth(&token).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("claude_accounts_remote: GET {url} failed: {e}");
            return None;
        }
    };
    let status = resp.status();
    if !status.is_success() {
        warn!("claude_accounts_remote: GET {url} returned HTTP {status}");
        return None;
    }
    match resp.json::<RosterResponse>().await {
        Ok(body) => Some(body.accounts),
        Err(e) => {
            warn!("claude_accounts_remote: GET {url} returned an unreadable body: {e}");
            None
        }
    }
}

/// Fetch the server roster and merge it into the local `claude-accounts.json`
/// — only the `claude_config_dirs` field; every other field is left as-is.
/// Fail-soft throughout: any error just skips the merge this run.
///
/// Called once per runner process, off the boot background-task block in
/// `mcp_api::start_server`, the same spot `publish_defaults` hangs off.
pub async fn sync_roster_from_web_once() {
    let base_url = crate::api_config::get_api_base_url();
    let Some(entries) = fetch_roster_async(&base_url).await else {
        return;
    };
    let fetched_count = entries.len();

    let root = accounts_root();
    let mut resolved_dirs = Vec::with_capacity(entries.len());
    for entry in &entries {
        let dir_name = entry
            .dir_name
            .clone()
            .unwrap_or_else(|| format!(".claude-{}", entry.account_key));
        let dir = root.join(dir_name);
        if dir.is_dir() {
            resolved_dirs.push(dir.to_string_lossy().to_string());
        } else {
            debug!(
                "claude_accounts_remote: skipping account '{}' — {} does not exist on this \
                 machine",
                entry.account_key,
                dir.display()
            );
        }
    }

    if resolved_dirs.is_empty() {
        info!(
            "claude_accounts_remote: server roster fetched ({fetched_count} account(s)) but \
             none resolve to an existing local directory — leaving claude-accounts.json \
             unchanged"
        );
        return;
    }

    let n = resolved_dirs.len();
    match crate::claude_accounts::update(|file| {
        file.claude_config_dirs = resolved_dirs;
    }) {
        Ok(()) => info!(
            "claude_accounts_remote: merged {n} account directories from qontinui-web into \
             claude-accounts.json"
        ),
        Err(e) => warn!("claude_accounts_remote: could not update claude-accounts.json: {e}"),
    }
}
