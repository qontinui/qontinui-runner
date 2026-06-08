use crate::settings::{self, AccountSelectionMode};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Cached resolved config dir for least-usage account selection.
/// Set by `set_resolved_config_dir` at startup or when usage is checked.
static RESOLVED_CONFIG_DIR: Mutex<Option<String>> = Mutex::new(None);

/// Per-account cooldown tracking. When an account hits a rate limit, it is
/// marked with `(when_marked, cooldown_duration)`. Each account carries its
/// own duration so that API providers can honour the precise `Retry-After`
/// returned by the server (e.g. 600s for a daily-quota 429), while CLI
/// subprocess paths fall back to the default [`RATE_LIMIT_COOLDOWN_SECS`].
static ACCOUNT_COOLDOWNS: Mutex<Option<HashMap<String, (Instant, Duration)>>> = Mutex::new(None);

/// Default cooldown for rate-limited accounts when no precise `Retry-After`
/// is available (e.g. the Claude CLI subprocess prints a "rate limit" message
/// on stdout without a header). Five minutes.
const RATE_LIMIT_COOLDOWN_SECS: u64 = 300;

/// One account's weekly-usage sample, captured by the usage probe
/// (`commands::ai_settings::probe_account_usage`). `usage_delta` is the
/// account's actual 7-day utilization minus its *expected* linear utilization
/// at this point in the billing window (negative = under projected pace);
/// `None` when the probe couldn't compute it (e.g. the 7d-reset header was
/// absent). `utilization` is the raw 0.0–1.0 weekly fraction, used as the
/// fallback ranking key when `usage_delta` is unavailable — mirroring the
/// frontend `compareByUsageHeadroom` (`src/components/settings/types.ts`).
///
/// `exhausted` marks an account that **won't serve a request right now** — at
/// or over its weekly cap, server-reported rejected, or the probe call itself
/// failed (the probe hits the same per-account quota the CLI uses, so this
/// also catches a spend-limited account whose weekly token utilization still
/// looks low). Exhausted accounts are deprioritized in selection regardless of
/// how favourable their `usage_delta` is: a fully-used account that is "under
/// projection" still has no tokens left, so a less-favourable but *usable*
/// account must win. Computed at probe time by
/// `commands::ai_settings::record_usage_snapshot`.
#[derive(Clone, Copy, Debug)]
struct UsageSample {
    captured_at: Instant,
    usage_delta: Option<f64>,
    utilization: f64,
    exhausted: bool,
}

/// Latest weekly-usage snapshot per account, keyed by config dir. Populated
/// off the selection hot path by the usage-probe callers (the periodic
/// startup refresh, the Settings/Terminal `check_accounts_usage` command, and
/// the `/analytics/account-usage` route) so [`usage_rank`] can rank
/// accounts without making its own HTTP calls. The probe endpoint
/// self-rate-limits, so the hot path must never probe inline — it reads this
/// cache instead.
static USAGE_SNAPSHOT: Mutex<Option<HashMap<String, UsageSample>>> = Mutex::new(None);

/// Maximum age of a usage sample before [`usage_rank`] treats it as stale and
/// returns `None` (so selection falls back to cooldown ordering). Fifteen
/// minutes — comfortably longer than the startup refresh cadence.
const USAGE_SNAPSHOT_TTL: Duration = Duration::from_secs(15 * 60);

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

/// Mark an account as rate-limited with the default cooldown duration.
///
/// Used by paths that don't have access to a precise `Retry-After` value
/// (e.g. the Claude CLI subprocess). API providers should prefer
/// [`mark_account_rate_limited_with_duration`].
pub fn mark_account_rate_limited(config_dir: &str) {
    mark_account_rate_limited_with_duration(
        config_dir,
        Duration::from_secs(RATE_LIMIT_COOLDOWN_SECS),
    );
}

/// Mark an account as rate-limited with a specific cooldown duration.
///
/// API providers should call this with the parsed `Retry-After` header so
/// the cooldown matches the server's actual recovery window. If the duration
/// is unknown, callers should fall back to [`mark_account_rate_limited`].
pub fn mark_account_rate_limited_with_duration(config_dir: &str, cooldown: Duration) {
    if let Ok(mut cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        let map = cooldowns.get_or_insert_with(HashMap::new);
        info!(
            "Marking account '{}' as rate-limited for {}s",
            short_label(config_dir),
            cooldown.as_secs()
        );
        map.insert(config_dir.to_string(), (Instant::now(), cooldown));
    }
}

/// Check if an account is currently in cooldown.
pub(super) fn is_account_cooled_down(config_dir: &str) -> bool {
    if let Ok(cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        if let Some(map) = cooldowns.as_ref() {
            if let Some((marked_at, cooldown)) = map.get(config_dir) {
                return marked_at.elapsed() < *cooldown;
            }
        }
    }
    false
}

/// How long until this specific account's cooldown expires.
///
/// Returns `Some(remaining)` if the account is still cooled down, or `None`
/// if it is available (either never marked, or the cooldown has expired).
///
/// Companion to [`is_account_cooled_down`]; reads `ACCOUNT_COOLDOWNS` under
/// a single lock acquisition for consistency with other readers in this
/// module.
pub fn time_until_cooled_down(config_dir: &str) -> Option<Duration> {
    let cooldowns = ACCOUNT_COOLDOWNS.lock().ok()?;
    let map = cooldowns.as_ref()?;
    let (marked_at, cooldown) = map.get(config_dir)?;
    let remaining = cooldown.saturating_sub(marked_at.elapsed());
    if remaining.is_zero() {
        None
    } else {
        Some(remaining)
    }
}

/// Record weekly-usage samples from a probe into the selection snapshot.
///
/// Called by the usage-probe paths (startup periodic refresh, the
/// `check_accounts_usage` command, the `/analytics/account-usage` route) so
/// the selection hot path has fresh headroom data without issuing its own
/// HTTP calls. Each tuple is `(config_dir, utilization, usage_delta,
/// exhausted)`.
pub fn record_account_usage(samples: &[(String, f64, Option<f64>, bool)]) {
    if let Ok(mut snap) = USAGE_SNAPSHOT.lock() {
        let map = snap.get_or_insert_with(HashMap::new);
        let now = Instant::now();
        for (dir, utilization, usage_delta, exhausted) in samples {
            map.insert(
                dir.clone(),
                UsageSample {
                    captured_at: now,
                    usage_delta: *usage_delta,
                    utilization: *utilization,
                    exhausted: *exhausted,
                },
            );
        }
    }
}

/// Selection rank for an account from the latest snapshot, if a fresh sample
/// exists: `(exhausted, headroom)`.
///
/// `exhausted` is the primary tier — an exhausted account (out of tokens /
/// rejected) always sorts after a usable one, no matter how favourable its
/// headroom. `headroom` is the tie-breaker within a tier: `usage_delta` when
/// available (most-negative = furthest under projected weekly pace = best),
/// else the raw `utilization` — mirroring the frontend
/// `compareByUsageHeadroom`. Both keys are ascending (lower is better).
///
/// Returns `None` when there is no sample or it is older than
/// [`USAGE_SNAPSHOT_TTL`], so callers fall back to cooldown-only ordering.
pub fn usage_rank(config_dir: &str) -> Option<(bool, f64)> {
    let snap = USAGE_SNAPSHOT.lock().ok()?;
    let map = snap.as_ref()?;
    let sample = map.get(config_dir)?;
    if sample.captured_at.elapsed() > USAGE_SNAPSHOT_TTL {
        return None;
    }
    Some((
        sample.exhausted,
        sample.usage_delta.unwrap_or(sample.utilization),
    ))
}

/// Rotate to the next available account after a rate-limit hit.
///
/// Marks the current account as rate-limited (with the default cooldown,
/// **but only if it isn't already marked** — so a precise `Retry-After`
/// duration set by [`mark_account_rate_limited_with_duration`] survives),
/// then picks the first non-cooled-down account from the configured
/// `claude_config_dirs`. Returns `true` if a switch happened, `false` if
/// no alternative is available.
pub fn rotate_account_on_rate_limit() -> bool {
    let config_dirs = settings::get_claude_config_dirs();
    if config_dirs.len() < 2 {
        return false;
    }

    let current = get_resolved_config_dir();
    let default_cooldown = Duration::from_secs(RATE_LIMIT_COOLDOWN_SECS);

    // Single lock acquisition: mark current as rate-limited + find best alternative
    let next_dir = if let Ok(mut cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        let map = cooldowns.get_or_insert_with(HashMap::new);

        // Mark the current account as rate-limited — but only if it isn't
        // already in the map. API providers may have already inserted a
        // precise `Retry-After`-derived duration; don't clobber that with
        // the shorter default.
        if let Some(ref dir) = current {
            if !map.contains_key(dir) {
                info!(
                    "Marking account '{}' as rate-limited for {}s",
                    short_label(dir),
                    default_cooldown.as_secs()
                );
                map.insert(dir.clone(), (Instant::now(), default_cooldown));
            }
        }

        // Find the first account that is not in cooldown
        let available = config_dirs.iter().find(|d| {
            current.as_ref() != Some(*d)
                && map
                    .get(*d)
                    .is_none_or(|(marked_at, cooldown)| marked_at.elapsed() >= *cooldown)
        });

        if let Some(dir) = available {
            Some(dir.clone())
        } else {
            // All accounts are in cooldown — pick the one closest to expiry
            // (smallest remaining = `cooldown - elapsed`).
            let best = config_dirs
                .iter()
                .filter(|d| current.as_ref() != Some(*d))
                .min_by_key(|d| {
                    map.get(*d)
                        .map(|(marked_at, cooldown)| cooldown.saturating_sub(marked_at.elapsed()))
                        .unwrap_or(Duration::ZERO) // never rate-limited = best candidate
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
                "All accounts rate-limited, switching to closest-to-expiry: '{}'",
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
pub fn time_until_next_account_available() -> Option<Duration> {
    let config_dirs = settings::get_claude_config_dirs();
    if config_dirs.len() < 2 {
        return None;
    }

    if let Ok(cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        let map = cooldowns.as_ref()?;

        // Check all accounts under one lock: compute remaining cooldown per account
        let mut all_in_cooldown = true;
        let mut earliest_remaining: Option<Duration> = None;

        for dir in &config_dirs {
            if let Some((marked_at, cooldown)) = map.get(dir) {
                let elapsed = marked_at.elapsed();
                if elapsed >= *cooldown {
                    // This account's cooldown has expired — no waiting needed
                    all_in_cooldown = false;
                    break;
                }
                let remaining = *cooldown - elapsed;
                earliest_remaining =
                    Some(earliest_remaining.map_or(remaining, |prev| prev.min(remaining)));
            } else {
                // Account was never rate-limited — it's available
                all_in_cooldown = false;
                break;
            }
        }

        if all_in_cooldown {
            earliest_remaining
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
            // Find account with the oldest cooldown (most elapsed time since marked).
            // Accounts not in the map count as "infinitely old" — i.e. never cooled,
            // so they're the best candidates to unlock-and-switch-to.
            let best = config_dirs
                .iter()
                .max_by_key(|d| {
                    map.get(*d)
                        .map(|(marked_at, _)| marked_at.elapsed())
                        .unwrap_or(Duration::MAX)
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

    info!(
        "Manually switching to account '{}'",
        short_label(config_dir)
    );
    set_resolved_config_dir(Some(config_dir.to_string()));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    /// Remove a specific dir from the static `ACCOUNT_COOLDOWNS` map.
    ///
    /// Tests share this static, so each test uses a unique dir path and
    /// clears only its own entries. Wiping the entire map would race with
    /// concurrent tests in `account_usage::tests` that mark their own dirs.
    fn clear_dir(dir: &str) {
        if let Ok(mut cooldowns) = ACCOUNT_COOLDOWNS.lock() {
            if let Some(map) = cooldowns.as_mut() {
                map.remove(dir);
            }
        }
    }

    #[test]
    fn mark_with_duration_then_is_cooled_then_expires() {
        let dir = "/test/config/mark_with_duration";
        clear_dir(dir);
        mark_account_rate_limited_with_duration(dir, Duration::from_millis(50));
        assert!(
            is_account_cooled_down(dir),
            "account should be cooled immediately after marking"
        );
        sleep(Duration::from_millis(80));
        assert!(
            !is_account_cooled_down(dir),
            "account should no longer be cooled after the duration elapses"
        );
        clear_dir(dir);
    }

    #[test]
    fn time_until_cooled_down_returns_remaining() {
        let dir = "/test/config/time_remaining";
        clear_dir(dir);
        let total = Duration::from_secs(120);
        mark_account_rate_limited_with_duration(dir, total);
        let remaining = time_until_cooled_down(dir).expect("should be cooled");
        // Within a few seconds tolerance — the test exec time is the only delta.
        assert!(
            remaining <= total && remaining + Duration::from_secs(5) >= total,
            "expected ~{}s remaining, got {}s",
            total.as_secs(),
            remaining.as_secs()
        );
        clear_dir(dir);
    }

    #[test]
    fn time_until_cooled_down_returns_none_when_unmarked() {
        let dir = "/test/config/never_marked";
        clear_dir(dir);
        assert!(time_until_cooled_down(dir).is_none());
    }

    #[test]
    fn time_until_cooled_down_returns_none_after_expiry() {
        let dir = "/test/config/expired";
        clear_dir(dir);
        mark_account_rate_limited_with_duration(dir, Duration::from_millis(30));
        sleep(Duration::from_millis(60));
        assert!(
            time_until_cooled_down(dir).is_none(),
            "expired cooldown should report None"
        );
        clear_dir(dir);
    }

    #[test]
    fn usage_rank_prefers_delta_then_falls_back_to_utilization() {
        let dir_delta = "/test/config/headroom_delta";
        let dir_util = "/test/config/headroom_util";
        // Fresh sample with a delta → headroom key is the delta, not exhausted.
        record_account_usage(&[(dir_delta.to_string(), 0.80, Some(-0.25), false)]);
        assert_eq!(usage_rank(dir_delta), Some((false, -0.25)));
        // Fresh sample without a delta → headroom falls back to utilization.
        record_account_usage(&[(dir_util.to_string(), 0.42, None, false)]);
        assert_eq!(usage_rank(dir_util), Some((false, 0.42)));
    }

    #[test]
    fn usage_rank_carries_exhausted_flag() {
        let dir = "/test/config/headroom_exhausted";
        // Exhausted even though the delta looks favourable (under projection).
        record_account_usage(&[(dir.to_string(), 1.0, Some(-0.05), true)]);
        assert_eq!(usage_rank(dir), Some((true, -0.05)));
    }

    #[test]
    fn usage_rank_none_when_unrecorded() {
        assert_eq!(usage_rank("/test/config/headroom_never"), None);
    }

    #[test]
    fn long_cooldown_survives_rotate_default_remark() {
        // Regression: if a 600s cooldown is set by an API provider via
        // `mark_account_rate_limited_with_duration`, calling
        // `rotate_account_on_rate_limit` must NOT overwrite it with the
        // shorter default (300s).
        //
        // This test only exercises the cooldown map itself — we can't invoke
        // `rotate_account_on_rate_limit` without setting up `settings`. The
        // protected invariant is the `map.contains_key` branch in
        // `rotate_account_on_rate_limit`; simulate that branch directly.
        let dir = "/test/config/long_cooldown";
        clear_dir(dir);
        let long = Duration::from_secs(600);
        mark_account_rate_limited_with_duration(dir, long);

        // Simulate the guarded re-mark logic from `rotate_account_on_rate_limit`.
        let already_marked = ACCOUNT_COOLDOWNS
            .lock()
            .ok()
            .and_then(|c| c.as_ref().map(|m| m.contains_key(dir)))
            .unwrap_or(false);
        assert!(
            already_marked,
            "account should be in the cooldown map before the rotate guard fires"
        );

        // Verify the original long duration is still in effect (would be the
        // case after `rotate_account_on_rate_limit` no-ops the re-mark).
        let remaining = time_until_cooled_down(dir).expect("still cooled");
        assert!(
            remaining > Duration::from_secs(500),
            "long cooldown should still be in effect, got {}s",
            remaining.as_secs()
        );
        clear_dir(dir);
    }
}
