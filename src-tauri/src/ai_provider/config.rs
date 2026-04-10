#![allow(dead_code)]

use crate::settings::{self, AccountSelectionMode};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;
use tracing::{info, warn};

/// Cached resolved config dir for least-usage account selection.
/// Set by `set_resolved_config_dir` at startup or when usage is checked.
static RESOLVED_CONFIG_DIR: Mutex<Option<String>> = Mutex::new(None);

/// Per-account cooldown tracking. When an account hits a rate limit,
/// it is marked with a cooldown timestamp. The account is skipped until
/// the cooldown expires.
static ACCOUNT_COOLDOWNS: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

/// How long to cool down a rate-limited account before trying it again (5 minutes).
const RATE_LIMIT_COOLDOWN_SECS: u64 = 300;

/// Set the resolved config directory (called after usage check).
pub fn set_resolved_config_dir(dir: Option<String>) {
    if let Ok(mut cached) = RESOLVED_CONFIG_DIR.lock() {
        info!("Setting resolved config dir: {:?}", dir);
        *cached = dir;
    }
}

/// Get the current resolved config directory (for display/status).
pub fn get_resolved_config_dir() -> Option<String> {
    RESOLVED_CONFIG_DIR
        .lock()
        .ok()
        .and_then(|cached| cached.clone())
}

/// Get the effective config directory, considering account selection mode.
pub fn get_effective_config_dir(cli_settings: &settings::ClaudeCliSettings) -> Option<String> {
    match cli_settings.account_selection_mode {
        AccountSelectionMode::LeastUsage => {
            if let Ok(cached) = RESOLVED_CONFIG_DIR.lock() {
                if cached.is_some() {
                    return cached.clone();
                }
            }
            // Fallback to manual config_dir if no resolved dir
            cli_settings.config_dir.clone()
        }
        AccountSelectionMode::Manual => cli_settings.config_dir.clone(),
    }
}

/// Mark an account as rate-limited. It will be skipped for `RATE_LIMIT_COOLDOWN_SECS`.
pub fn mark_account_rate_limited(config_dir: &str) {
    if let Ok(mut cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        let map = cooldowns.get_or_insert_with(HashMap::new);
        info!(
            "Marking account '{}' as rate-limited for {}s",
            short_label(config_dir),
            RATE_LIMIT_COOLDOWN_SECS
        );
        map.insert(config_dir.to_string(), Instant::now());
    }
}

/// Check if an account is currently in cooldown.
fn is_account_cooled_down(config_dir: &str) -> bool {
    if let Ok(cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        if let Some(map) = cooldowns.as_ref() {
            if let Some(marked_at) = map.get(config_dir) {
                return marked_at.elapsed().as_secs() < RATE_LIMIT_COOLDOWN_SECS;
            }
        }
    }
    false
}

/// Rotate to the next available account after a rate-limit hit.
///
/// Marks the current account as rate-limited, then picks the first
/// non-cooled-down account from the configured `claude_config_dirs`.
/// Returns `true` if a switch happened, `false` if no alternative is available.
pub fn rotate_account_on_rate_limit() -> bool {
    let config_dirs = settings::get_claude_config_dirs();
    if config_dirs.len() < 2 {
        // Nothing to rotate to
        return false;
    }

    // Mark the current account as rate-limited
    let current = get_resolved_config_dir();
    if let Some(ref dir) = current {
        mark_account_rate_limited(dir);
    }

    // Find the first account that is not in cooldown
    for dir in &config_dirs {
        if !is_account_cooled_down(dir) {
            if current.as_ref() == Some(dir) {
                continue; // Skip the one we just marked (race-free: it's now cooled down)
            }
            info!(
                "Rotating account: '{}' -> '{}'",
                current.as_deref().map(short_label).unwrap_or("none"),
                short_label(dir)
            );
            set_resolved_config_dir(Some(dir.clone()));
            return true;
        }
    }

    // All accounts are in cooldown — pick the one closest to expiry
    if let Ok(cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        if let Some(map) = cooldowns.as_ref() {
            let best = config_dirs
                .iter()
                .filter(|d| current.as_ref() != Some(*d))
                .max_by_key(|d| {
                    map.get(*d)
                        .map(|t| t.elapsed().as_secs())
                        .unwrap_or(u64::MAX)
                });
            if let Some(dir) = best {
                warn!(
                    "All accounts rate-limited, switching to least-recently-limited: '{}'",
                    short_label(dir)
                );
                // Drop the cooldowns lock before calling set_resolved_config_dir
                let dir = dir.clone();
                drop(cooldowns);
                set_resolved_config_dir(Some(dir));
                return true;
            }
        }
    }

    false
}

/// Get a short label for a config dir (last path component).
fn short_label(config_dir: &str) -> &str {
    std::path::Path::new(config_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(config_dir)
}

/// Get account status info for all configured accounts.
/// Returns (config_dir, label, is_active, is_cooled_down) for each.
pub fn get_account_statuses() -> Vec<(String, String, bool, bool)> {
    let config_dirs = settings::get_claude_config_dirs();
    let current = get_resolved_config_dir();

    config_dirs
        .into_iter()
        .map(|dir| {
            let label = short_label(&dir).to_string();
            let is_active = current.as_ref() == Some(&dir);
            let cooled = is_account_cooled_down(&dir);
            (dir, label, is_active, cooled)
        })
        .collect()
}

/// Manually switch to a specific account by config dir path.
/// Clears any cooldown on the target account.
/// Returns true if the switch was valid, false if the dir isn't in the configured list.
pub fn switch_to_account(config_dir: &str) -> bool {
    let config_dirs = settings::get_claude_config_dirs();
    if !config_dirs.contains(&config_dir.to_string()) {
        warn!(
            "Cannot switch to '{}': not in configured claude_config_dirs",
            config_dir
        );
        return false;
    }

    // Clear cooldown on the target account
    if let Ok(mut cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        if let Some(map) = cooldowns.as_mut() {
            map.remove(config_dir);
        }
    }

    info!("Manually switching to account '{}'", short_label(config_dir));
    set_resolved_config_dir(Some(config_dir.to_string()));
    true
}
