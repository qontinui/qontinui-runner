//! Cooldown-driven Claude account selection.
//!
//! [`pick_best_account`] picks an effective Claude config dir from the
//! configured `claude_config_dirs` based purely on local cooldown state —
//! no HTTP calls on the hot path. The selection is:
//!
//! 1. The first account that is not currently in cooldown.
//! 2. If every account is cooled, the one with the **shortest remaining
//!    cooldown** (closest to expiry), so the next attempt happens as soon
//!    as possible.
//!
//! Cooldown state is mutated by the inference path itself: when a 429 hits,
//! `claude_api`/`claude_api_warm` parse the `Retry-After` header and call
//! [`super::config::mark_account_rate_limited_with_duration`]. The CLI
//! subprocess path uses [`super::config::mark_account_rate_limited`] with
//! the default 5-minute cooldown (no header source on stdout).
//!
//! Authoritative server-side utilization (`/api/oauth/usage` and the
//! `anthropic-ratelimit-unified-*` response headers) is a **calibration**
//! signal, surfaced via `commands::ai_settings::probe_account_usage` for the
//! Settings UI and the `/analytics/account-usage` HTTP route. It is never
//! used on the selection hot path because the probe endpoint itself
//! self-rate-limits and gives the worst signal exactly when it matters most.
//!
//! [`pick_best_account`] is a no-op when `account_selection_mode != LeastUsage`
//! or fewer than two accounts are configured.

use crate::settings::{self, AccountSelectionMode};
use std::path::Path;
use std::time::Duration;
use tracing::info;

/// Pin the most-available Claude config dir as the effective account for the
/// current process. Call this once at the start of each logical unit of AI
/// work (workflow run, prompt-home submission, AI chat session) — *not* per
/// call, so warm-provider prompt-cache locality is preserved within a unit.
///
/// No-op when:
///   - account selection mode is `Manual`
///   - fewer than two accounts are configured
///
/// Selection is cooldown-driven: prefers an account with no active cooldown,
/// otherwise picks the one whose cooldown will expire soonest.
pub fn pick_best_account() {
    let ai_settings = settings::get_ai_settings();
    if ai_settings.claude_cli.account_selection_mode != AccountSelectionMode::LeastUsage {
        return;
    }

    let config_dirs = settings::get_claude_config_dirs();
    if config_dirs.len() < 2 {
        return;
    }

    let chosen = config_dirs
        .iter()
        .find(|d| !super::config::is_account_cooled_down(d))
        .cloned()
        .or_else(|| {
            // All cooled: pick the one with the shortest remaining cooldown.
            config_dirs
                .iter()
                .min_by_key(|d| super::config::time_until_cooled_down(d).unwrap_or(Duration::ZERO))
                .cloned()
        });

    if let Some(dir) = chosen {
        let current = super::config::get_resolved_config_dir();
        if current.as_deref() != Some(dir.as_str()) {
            info!(
                "pick_best_account: selecting '{}' (cooldown-driven)",
                short_label(&dir)
            );
            super::config::set_resolved_config_dir(Some(dir));
        }
    }
}

fn short_label(config_dir: &str) -> &str {
    Path::new(config_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(config_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // The pick logic in `pick_best_account` reads global settings
    // (`settings::get_ai_settings`, `settings::get_claude_config_dirs`),
    // which are not test-mode-injectable in this codebase. The selection
    // *math* — "first non-cooled, else closest-to-expiry" — is exercised
    // directly against `super::config` helpers below so we still get
    // regression coverage on the algorithm without a settings shim.
    //
    // These tests share the `ACCOUNT_COOLDOWNS` static with the config
    // tests, so use unique dir paths and clear only the entries this test
    // touches via `clear_test_dir`.

    fn clear_test_dir(dir: &str) {
        // Switch-to-account clears the cooldown entry for the target dir
        // without touching anything else. But it requires the dir to be in
        // the configured list, which it isn't in tests — so we go through
        // the map directly via a brand-new helper. The simplest reliable
        // path is to mark with zero duration: `time_until_cooled_down`
        // saturates to None and `is_account_cooled_down` reports false
        // (since elapsed >= ZERO immediately). Subsequent assertions in
        // *other* tests use different dir names, so they aren't affected.
        super::super::config::mark_account_rate_limited_with_duration(dir, Duration::ZERO);
    }

    #[test]
    fn closest_to_expiry_wins_when_all_cooled() {
        // Unique paths per test to avoid cross-test pollution of the
        // shared `ACCOUNT_COOLDOWNS` static.
        let dir_long = "/test/account_usage/closest_long";
        let dir_short = "/test/account_usage/closest_short";
        clear_test_dir(dir_long);
        clear_test_dir(dir_short);

        super::super::config::mark_account_rate_limited_with_duration(
            dir_long,
            Duration::from_secs(600),
        );
        super::super::config::mark_account_rate_limited_with_duration(
            dir_short,
            Duration::from_secs(30),
        );

        // Mirrors the "all cooled → min remaining" branch of `pick_best_account`.
        let chosen = [dir_long, dir_short]
            .into_iter()
            .min_by_key(|d| {
                super::super::config::time_until_cooled_down(d).unwrap_or(Duration::ZERO)
            })
            .unwrap();

        assert_eq!(
            chosen, dir_short,
            "should pick the account with the shortest remaining cooldown"
        );

        clear_test_dir(dir_long);
        clear_test_dir(dir_short);
    }

    #[test]
    fn first_uncooled_wins_when_any_available() {
        let dir_cooled = "/test/account_usage/first_cooled";
        let dir_available = "/test/account_usage/first_available";
        clear_test_dir(dir_cooled);
        clear_test_dir(dir_available);

        super::super::config::mark_account_rate_limited_with_duration(
            dir_cooled,
            Duration::from_secs(600),
        );
        // Leave dir_available unmarked → it should be selected first.

        // Mirrors the "first non-cooled" branch of `pick_best_account`.
        let chosen = [dir_cooled, dir_available]
            .into_iter()
            .find(|d| !super::super::config::is_account_cooled_down(d));

        assert_eq!(chosen, Some(dir_available));

        clear_test_dir(dir_cooled);
    }

    #[test]
    fn short_label_returns_basename() {
        assert_eq!(short_label("/home/josh/.claude-hotmail"), ".claude-hotmail");
        assert_eq!(short_label("relative/path/.claude"), ".claude");
        // No separators → returns the whole string.
        assert_eq!(short_label("name-only"), "name-only");
    }
}
