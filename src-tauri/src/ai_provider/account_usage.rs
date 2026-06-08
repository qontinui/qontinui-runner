//! Weekly-usage-aware Claude account selection.
//!
//! [`pick_best_account`] picks an effective Claude config dir from the
//! configured `claude_config_dirs`. Among accounts that are **not** in a
//! rate-limit cooldown, it ranks by [`super::config::usage_rank`] —
//! `(exhausted, headroom)`. `headroom` is the weekly-usage signal (actual
//! 7-day utilization minus the expected linear utilization at this point in
//! the billing window; negative = under projected pace), mirroring the
//! Terminal "best account" picker (`compareByUsageHeadroom` in
//! `src/components/settings/types.ts`). `exhausted` is the dominating tier: an
//! out-of-tokens / rejected account is deprioritized no matter how favourable
//! its headroom (a fully-used account that is "under projection" still has no
//! tokens left). The co-pilot relays its planning prompt through this picker,
//! so it chooses the same account a human would when opening a new session.
//!
//! Selection order:
//! 1. Among non-cooled accounts with a fresh usage sample, the best by
//!    `(exhausted, headroom)` ascending: a usable account beats an exhausted
//!    one, then most-under-projection wins within a tier.
//! 2. If no non-cooled account has a fresh sample, the first non-cooled
//!    account (legacy cooldown-only behaviour — preserved for cold start and
//!    for the single-account case, where the one account is simply pinned).
//! 3. If every account is cooled, the one with the **shortest remaining
//!    cooldown** (closest to expiry).
//!
//! The usage signal comes from a cached snapshot, never an inline probe: the
//! probe endpoint (`commands::ai_settings::probe_account_usage`) self-rate-
//! limits, so probing on the hot path would give the worst signal exactly
//! when it matters most. The snapshot is refreshed off the hot path — at
//! startup and periodically (see `main.rs`), and opportunistically whenever
//! the Settings/Terminal `check_accounts_usage` command or the
//! `/analytics/account-usage` route runs — via
//! [`super::config::record_account_usage`].
//!
//! Cooldown state is mutated by the inference path itself: when a 429 hits,
//! `claude_api`/`claude_api_warm` parse the `Retry-After` header and call
//! [`super::config::mark_account_rate_limited_with_duration`]. The CLI
//! subprocess path uses [`super::config::mark_account_rate_limited`] with
//! the default 5-minute cooldown (no header source on stdout).
//!
//! [`pick_best_account`] is a no-op when `account_selection_mode != LeastUsage`
//! or no accounts are configured.

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
///   - no accounts are configured
///
/// With a single configured account it simply pins that account (so the
/// effective config dir resolves even for users who never set a manual
/// `config_dir`). With several, selection prefers the non-cooled, non-
/// exhausted account with the most weekly-usage headroom; falls back to
/// first-non-cooled (cold start), then to the soonest-to-expire cooldown when
/// every account is rate-limited.
pub fn pick_best_account() {
    let ai_settings = settings::get_ai_settings();
    if ai_settings.claude_cli.account_selection_mode != AccountSelectionMode::LeastUsage {
        return;
    }

    let config_dirs = settings::get_claude_config_dirs();
    if config_dirs.is_empty() {
        return;
    }

    let chosen = pick_from(
        &config_dirs,
        |d| super::config::is_account_cooled_down(d),
        |d| super::config::usage_rank(d),
        |d| super::config::time_until_cooled_down(d),
    );

    if let Some(dir) = chosen {
        let current = super::config::get_resolved_config_dir();
        if current.as_deref() != Some(dir.as_str()) {
            info!("pick_best_account: selecting '{}'", short_label(&dir));
            super::config::set_resolved_config_dir(Some(dir));
        }
    }
}

/// Pure selection core, parameterised over the cooldown/usage/expiry lookups
/// so it can be unit-tested without the global settings + state singletons.
///
/// `is_cooled` / `usage` / `remaining` mirror the `super::config` helpers.
/// `usage` returns `(exhausted, headroom)` for accounts with a fresh sample.
/// Returns the chosen config dir, or `None` only when `config_dirs` is empty.
fn pick_from<'a>(
    config_dirs: &'a [String],
    is_cooled: impl Fn(&str) -> bool,
    usage: impl Fn(&str) -> Option<(bool, f64)>,
    remaining: impl Fn(&str) -> Option<Duration>,
) -> Option<String> {
    let available: Vec<&'a String> = config_dirs.iter().filter(|d| !is_cooled(d)).collect();

    if !available.is_empty() {
        // Among available accounts that have a fresh usage sample, pick the
        // best by `(exhausted, headroom)` ascending: a usable account always
        // beats an exhausted one (out of tokens / rejected) regardless of
        // headroom, and within a tier the most-under-projection wins. If no
        // available account has a fresh sample, fall back to the first
        // available (legacy cooldown-only pick / cold start).
        let best = available
            .iter()
            .filter_map(|d| usage(d).map(|rank| (*d, rank)))
            .min_by(|a, b| {
                // (exhausted, headroom) — false < true, then headroom ascending.
                a.1 .0.cmp(&b.1 .0).then(
                    a.1 .1
                        .partial_cmp(&b.1 .1)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
            })
            .map(|(d, _)| d.clone());
        return best.or_else(|| available.first().map(|d| (*d).clone()));
    }

    // All cooled: pick the one with the shortest remaining cooldown.
    config_dirs
        .iter()
        .min_by_key(|d| remaining(d).unwrap_or(Duration::ZERO))
        .cloned()
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

    // --- pick_from: the pure selection core ---------------------------------
    //
    // `pick_from` is parameterised over the cooldown/usage/expiry lookups, so
    // these tests inject closures and assert the ranking directly — no global
    // settings/state needed. `usage` returns `(exhausted, headroom)`: a usable
    // account beats an exhausted one regardless of headroom, and within a tier
    // the lowest `usage_delta` (else utilization) wins — mirroring the frontend
    // `compareByUsageHeadroom` plus the out-of-tokens guard.

    fn dirs(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn headroom_wins_among_available() {
        let d = dirs(&["/a", "/b", "/c"]);
        // /b is furthest under projected pace (most-negative delta) → chosen,
        // even though /a sorts first by position. None exhausted.
        let chosen = pick_from(
            &d,
            |_| false, // none cooled
            |x| match x {
                "/a" => Some((false, 0.10)),
                "/b" => Some((false, -0.30)),
                "/c" => Some((false, 0.05)),
                _ => None,
            },
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/b"));
    }

    #[test]
    fn usable_account_beats_exhausted_with_better_headroom() {
        let d = dirs(&["/full", "/usable"]);
        // /full is fully used (100%) when 95% expected → favourable delta
        // (-... no, +0.05) BUT exhausted. /usable is 80% used / 60% expected
        // (+0.20, worse delta) but has tokens left → must win.
        let chosen = pick_from(
            &d,
            |_| false,
            |x| match x {
                "/full" => Some((true, 0.05)),    // exhausted, better headroom
                "/usable" => Some((false, 0.20)), // usable, worse headroom
                _ => None,
            },
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/usable"));
    }

    #[test]
    fn all_exhausted_picks_best_headroom_among_them() {
        let d = dirs(&["/a", "/b"]);
        // Both exhausted → least-bad (lowest headroom) is the fallback choice.
        let chosen = pick_from(
            &d,
            |_| false,
            |x| match x {
                "/a" => Some((true, 0.30)),
                "/b" => Some((true, 0.10)),
                _ => None,
            },
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/b"));
    }

    #[test]
    fn cooled_account_excluded_even_with_best_headroom() {
        let d = dirs(&["/a", "/b"]);
        // /a has the best headroom but is cooled → must not be picked.
        let chosen = pick_from(
            &d,
            |x| x == "/a",
            |x| match x {
                "/a" => Some((false, -0.90)),
                "/b" => Some((false, 0.20)),
                _ => None,
            },
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/b"));
    }

    #[test]
    fn falls_back_to_first_available_when_no_usage_data() {
        let d = dirs(&["/a", "/b", "/c"]);
        // No fresh samples → legacy "first non-cooled" behaviour. /a is cooled,
        // so /b (first available) wins.
        let chosen = pick_from(&d, |x| x == "/a", |_| None, |_| None);
        assert_eq!(chosen.as_deref(), Some("/b"));
    }

    #[test]
    fn ranks_only_accounts_with_samples_then_keeps_them() {
        let d = dirs(&["/a", "/b"]);
        // Only /b has a sample → it is selected (a known-rank account is
        // preferred over an unprobed one).
        let chosen = pick_from(
            &d,
            |_| false,
            |x| if x == "/b" { Some((false, 0.40)) } else { None },
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/b"));
    }

    #[test]
    fn single_account_is_pinned() {
        // One configured account, no usage sample, not cooled → it is selected
        // (the single-account case must resolve, not no-op).
        let d = dirs(&["/only"]);
        let chosen = pick_from(&d, |_| false, |_| None, |_| None);
        assert_eq!(chosen.as_deref(), Some("/only"));
    }

    #[test]
    fn single_exhausted_account_still_pinned() {
        // One account, exhausted → still the only option, so still selected.
        let d = dirs(&["/only"]);
        let chosen = pick_from(&d, |_| false, |_| Some((true, 1.0)), |_| None);
        assert_eq!(chosen.as_deref(), Some("/only"));
    }

    #[test]
    fn all_cooled_picks_soonest_to_expire() {
        let d = dirs(&["/a", "/b"]);
        let chosen = pick_from(
            &d,
            |_| true, // all cooled
            |_| None,
            |x| match x {
                "/a" => Some(Duration::from_secs(600)),
                "/b" => Some(Duration::from_secs(30)),
                _ => None,
            },
        );
        assert_eq!(chosen.as_deref(), Some("/b"));
    }

    #[test]
    fn empty_config_dirs_returns_none() {
        let d: Vec<String> = Vec::new();
        let chosen = pick_from(&d, |_| false, |_| None, |_| None);
        assert_eq!(chosen, None);
    }
}
