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
        return false;
    }

    let current = get_resolved_config_dir();

    // Single lock acquisition: mark current as rate-limited + find best alternative
    let next_dir = if let Ok(mut cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        let map = cooldowns.get_or_insert_with(HashMap::new);

        // Mark the current account as rate-limited
        if let Some(ref dir) = current {
            info!(
                "Marking account '{}' as rate-limited for {}s",
                short_label(dir),
                RATE_LIMIT_COOLDOWN_SECS
            );
            map.insert(dir.clone(), Instant::now());
        }

        // Find the first account that is not in cooldown
        let available = config_dirs.iter().find(|d| {
            current.as_ref() != Some(*d)
                && map
                    .get(*d).is_none_or(|t| t.elapsed().as_secs() >= RATE_LIMIT_COOLDOWN_SECS)
        });

        if let Some(dir) = available {
            Some(dir.clone())
        } else {
            // All accounts are in cooldown — pick the one closest to expiry
            // (most elapsed time since it was marked)
            let best = config_dirs
                .iter()
                .filter(|d| current.as_ref() != Some(*d))
                .max_by_key(|d| {
                    map.get(*d)
                        .map(|t| t.elapsed().as_secs())
                        .unwrap_or(u64::MAX) // never rate-limited = best candidate
                });
            best.cloned()
        }
    } else {
        return false;
    };
    // Lock is dropped here before calling set_resolved_config_dir

    if let Some(dir) = next_dir {
        let was_all_limited = is_account_cooled_down(&dir);
        if was_all_limited {
            warn!(
                "All accounts rate-limited, switching to least-recently-limited: '{}'",
                short_label(&dir)
            );
        } else {
            info!(
                "Rotating account: '{}' -> '{}'",
                current.as_deref().map(short_label).unwrap_or("none"),
                short_label(&dir)
            );
        }
        set_resolved_config_dir(Some(dir));
        true
    } else {
        false
    }
}

/// How long until the next account becomes available (cooldown expires).
///
/// Returns `None` if any account is already available, or if there are
/// fewer than 2 accounts configured. Returns `Some(duration)` with the
/// wait time until the earliest cooldown expires.
///
/// Uses a single lock acquisition to avoid TOCTOU races.
pub fn time_until_next_account_available() -> Option<std::time::Duration> {
    let config_dirs = settings::get_claude_config_dirs();
    if config_dirs.len() < 2 {
        return None;
    }

    if let Ok(cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        let map = cooldowns.as_ref()?;

        // Check all accounts under one lock: compute remaining cooldown per account
        let mut all_in_cooldown = true;
        let mut earliest_remaining: Option<u64> = None;

        for dir in &config_dirs {
            if let Some(marked_at) = map.get(dir) {
                let elapsed = marked_at.elapsed().as_secs();
                if elapsed >= RATE_LIMIT_COOLDOWN_SECS {
                    // This account's cooldown has expired — no waiting needed
                    all_in_cooldown = false;
                    break;
                }
                let remaining = RATE_LIMIT_COOLDOWN_SECS - elapsed;
                earliest_remaining = Some(
                    earliest_remaining.map_or(remaining, |prev: u64| prev.min(remaining)),
                );
            } else {
                // Account was never rate-limited — it's available
                all_in_cooldown = false;
                break;
            }
        }

        if all_in_cooldown {
            earliest_remaining.map(std::time::Duration::from_secs)
        } else {
            None
        }
    } else {
        None
    }
}

/// Clear the cooldown for the account that has been cooling the longest,
/// switch to it, and return true. Returns false if no accounts are configured.
pub fn force_unlock_earliest_account() -> bool {
    let config_dirs = settings::get_claude_config_dirs();
    if config_dirs.is_empty() {
        return false;
    }

    let best = if let Ok(mut cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        if let Some(map) = cooldowns.as_mut() {
            // Find account with the oldest cooldown (most elapsed time)
            let best = config_dirs
                .iter()
                .max_by_key(|d| {
                    map.get(*d)
                        .map(|t| t.elapsed().as_secs())
                        .unwrap_or(u64::MAX)
                })
                .cloned();
            // Clear its cooldown
            if let Some(ref dir) = best {
                map.remove(dir);
                info!(
                    "Force-unlocked account '{}' after cooldown wait",
                    short_label(dir)
                );
            }
            best
        } else {
            config_dirs.into_iter().next()
        }
    } else {
        return false;
    };

    if let Some(dir) = best {
        set_resolved_config_dir(Some(dir));
        true
    } else {
        false
    }
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
