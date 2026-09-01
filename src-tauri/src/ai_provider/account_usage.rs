//! Weekly-usage-aware Claude account selection.
//!
//! [`pick_best_account`] picks an effective Claude config dir from the
//! configured `claude_config_dirs`. Among accounts that are **not** in a
//! rate-limit cooldown, it ranks by [`super::config::usage_rank`] and
//! [`super::config::cmp_rank`], mirroring the Terminal "best account" picker
//! (`compareByUsageHeadroom` in `src/components/settings/types.ts`). The
//! co-pilot relays its planning prompt through this picker, so it chooses the
//! same account a human would when opening a new session.
//!
//! **The rule is use-it-or-lose-it.** Unused weekly capacity expires at the
//! account's reset and does not roll over, so the account worth burning is the
//! one whose spare capacity is about to be lost — *not* the emptiest one,
//! whose runway is in no danger. Concretely: among accounts under their
//! projected pace, the one whose 7-day window is furthest along wins.
//!
//! Ranking key, in order (see [`super::config::PaceRank`] for the tier keys):
//! 1. `exhausted` — the dominating tier. An out-of-tokens / rejected account
//!    is deprioritized no matter how favourable its pace key; a fully-used
//!    account whose window is nearly over still has nothing left to burn.
//! 2. Pace **tier**: under-pace (`usage_delta < 0`) before unknown (no usable
//!    pace signal) before over-pace (`usage_delta >= 0`).
//! 3. Within a tier only, that tier's own key in its own direction:
//!    - under-pace → `expected_utilization` **DESCENDING** (use-it-or-lose-it);
//!    - unknown → raw `utilization` ascending (unchanged behaviour for the
//!      population that never had a pace signal);
//!    - over-pace → the **ratio** `utilization / expected_utilization`
//!      ASCENDING. A ratio, not a difference: a difference is not comparable
//!      across accounts at different points in their windows, since +5 points
//!      over at 10% expected is far more over-pace than +5 points over at 80%
//!      expected, yet a difference scores the two identically.
//!
//! Selection order:
//! 1. Among non-cooled accounts with a fresh usage sample, the best by the
//!    ranking key above.
//! 2. If no non-cooled account has a fresh sample (cold start / stale
//!    snapshot), the first non-cooled account that is not *known-exhausted*
//!    (per the last sample, with a longer staleness tolerance); if every
//!    available account is known-exhausted, the first non-cooled. The
//!    single-account case resolves here — the one account is simply pinned.
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

use super::config::{cmp_rank, UsageRank};
use crate::claude_session::federation::derive_account_name;
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
/// `config_dir`). With several, selection prefers the non-cooled,
/// non-exhausted account whose spare weekly capacity is closest to expiring —
/// among accounts under their projected pace, the one furthest through its
/// 7-day window, because unused capacity does not roll over past the reset.
/// Falls back to first-non-cooled (cold start), then to the soonest-to-expire
/// cooldown when every account is rate-limited. Full key: the module doc.
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
        |d| super::oauth_refresh::has_valid_credentials(d),
        |d| super::config::is_account_cooled_down(d),
        |d| super::config::usage_rank(d),
        |d| super::config::account_known_exhausted(d),
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
/// `has_valid_creds` / `is_cooled` / `usage` / `known_exhausted` / `remaining`
/// mirror the `super::oauth_refresh` + `super::config` helpers.
/// `has_valid_creds` is the highest-precedence filter — an account without
/// live credentials is never selectable (it would 401 the moment a `claude`
/// subprocess spawns under it), so it is excluded from BOTH the non-cooled
/// ranking and the all-cooled fallback. `usage` returns the
/// [`UsageRank`] — `exhausted` plus the account's pace tier and that tier's own
/// key — for accounts with a *fresh* sample, and the ranked arm orders them
/// with [`cmp_rank`], never field by field. `known_exhausted` reports the
/// last-seen exhaustion with a longer staleness tolerance, used only by the
/// cold-start fallback. Returns the chosen config dir, or `None` when
/// `config_dirs` is empty OR no dir has valid credentials.
fn pick_from<'a>(
    config_dirs: &'a [String],
    has_valid_creds: impl Fn(&str) -> bool,
    is_cooled: impl Fn(&str) -> bool,
    usage: impl Fn(&str) -> Option<UsageRank>,
    known_exhausted: impl Fn(&str) -> bool,
    remaining: impl Fn(&str) -> Option<Duration>,
) -> Option<String> {
    // Validity filter ABOVE cooldown: never select an account without live
    // credentials. With none valid, return `None` — the caller leaves the
    // resolved dir unchanged and the spawn path fails loud (see
    // `agent_runtime::run_continuation_terminal` / `spawn_claude_child`).
    let valid: Vec<&'a String> = config_dirs.iter().filter(|d| has_valid_creds(d)).collect();
    if valid.is_empty() {
        return None;
    }

    let available: Vec<&'a String> = valid.iter().filter(|d| !is_cooled(d)).copied().collect();

    if !available.is_empty() {
        // Among available accounts that have a fresh usage sample, pick the
        // best by `cmp_rank`: a usable account always beats an exhausted one
        // (out of tokens / rejected) regardless of pace, then the pace tier
        // (under-pace → unknown → over-pace), then that tier's own key.
        // Within under-pace that key is `expected` DESCENDING — the window
        // furthest along wins, because its unused capacity expires at the
        // reset and does not roll over. Within over-pace it is the RATIO
        // `utilization / expected` ascending, not the difference: a difference
        // is not comparable across accounts at different points in their
        // windows.
        let best = available
            .iter()
            .filter_map(|d| usage(d).map(|rank| (*d, rank)))
            .min_by(|a, b| cmp_rank(&a.1, &b.1))
            .map(|(d, _)| d.clone());
        return best
            // Cold start / stale snapshot (no fresh ranks): prefer the first
            // available account NOT known-exhausted, so we don't pin a
            // recently-maxed-out account just because it isn't in cooldown
            // (the usage probe's 429 doesn't set an inference cooldown).
            .or_else(|| {
                available
                    .iter()
                    .find(|d| !known_exhausted(d))
                    .map(|d| (*d).clone())
            })
            // Everything available is known-exhausted (or unknown): fall back
            // to the first available so we always return something.
            .or_else(|| available.first().map(|d| (*d).clone()));
    }

    // All (credential-valid) accounts cooled: pick the valid one with the
    // shortest remaining cooldown. Restricting to `valid` matters — a
    // credential-less dir has no cooldown entry (`remaining` → `None` →
    // `Duration::ZERO`) and would otherwise win the `min` precisely when every
    // authenticated account is rate-limited.
    valid
        .iter()
        .min_by_key(|d| remaining(d).unwrap_or(Duration::ZERO))
        .map(|d| (*d).clone())
}

/// Pick the account a token-exhausted session should MIGRATE to: the best
/// [`cmp_rank`]-ranked account among the configured dirs, excluding the
/// exhausted source dir. Same use-it-or-lose-it rule as spawn-time selection
/// — among accounts under their projected pace, the one furthest through its
/// 7-day window, whose spare capacity expires soonest — so a migration does
/// not undo the choice [`pick_best_account`] would have made.
///
/// Unlike [`pick_best_account`] (spawn-time selection, which must always pin
/// *something*), migration is optional work — moving a session onto another
/// exhausted or cooldown'd account gains nothing, so this returns `None`
/// instead of a least-bad fallback. Does NOT touch the global resolved dir;
/// the caller pins the choice per-spawn via `CLAUDE_CONFIG_DIR`.
///
/// Also unlike [`pick_best_account`], this is NOT gated on
/// `account_selection_mode == LeastUsage`: the migration feature has its own
/// settings switch (`claude_cli.auto_migrate_on_token_exhaustion`), and a
/// Manual-mode operator who triggers a manual migration still wants a target.
pub fn pick_migration_target(exclude_dir: &str) -> Option<String> {
    let config_dirs: Vec<String> = settings::get_claude_config_dirs()
        .into_iter()
        .filter(|d| d != exclude_dir)
        .collect();

    pick_target_from(
        &config_dirs,
        |d| super::oauth_refresh::has_valid_credentials(d),
        |d| super::config::is_account_cooled_down(d),
        |d| super::config::usage_rank(d),
        |d| super::config::account_known_exhausted(d),
    )
}

/// Pure core of [`pick_migration_target`], parameterised like [`pick_from`]
/// so it can be unit-tested without the settings/state singletons. Requires
/// the chosen dir to have live credentials, be out of cooldown, and not be
/// known-exhausted; ranks the survivors with a fresh usage sample through the
/// **one shared** [`cmp_rank`] — under-pace first, ordered by `expected`
/// descending so the capacity closest to expiring is the capacity spent, then
/// unknown by raw `utilization`, then over-pace by the ratio
/// `utilization / expected` ascending (a ratio, not a difference, so accounts
/// at different points in their windows stay comparable). Excludes
/// fresh-sample-exhausted dirs the staleness-tolerant `known_exhausted` filter
/// missed. Both pickers must keep calling that one comparator — two copies is
/// how the tiers drift apart.
fn pick_target_from<'a>(
    config_dirs: &'a [String],
    has_valid_creds: impl Fn(&str) -> bool,
    is_cooled: impl Fn(&str) -> bool,
    usage: impl Fn(&str) -> Option<UsageRank>,
    known_exhausted: impl Fn(&str) -> bool,
) -> Option<String> {
    let candidates: Vec<&'a String> = config_dirs
        .iter()
        .filter(|d| has_valid_creds(d) && !is_cooled(d) && !known_exhausted(d))
        .collect();
    if candidates.is_empty() {
        return None;
    }

    // Best fresh-sample rank wins; with no fresh samples (cold start), take
    // the first surviving candidate — it passed the credential / cooldown /
    // known-exhaustion filters, which is the strongest signal available.
    candidates
        .iter()
        .filter_map(|d| usage(d).map(|rank| (*d, rank)))
        .filter(|(_, rank)| !rank.exhausted)
        .min_by(|a, b| cmp_rank(&a.1, &b.1))
        .map(|(d, _)| d.clone())
        .or_else(|| candidates.first().map(|d| (*d).clone()))
}

fn short_label(config_dir: &str) -> &str {
    Path::new(config_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(config_dir)
}

// ============================================================================
// Per-request account selection (explicit account override)
// ============================================================================

/// Why a caller-supplied `account` could not be resolved to a spawnable
/// config dir. Both variants map to a 4xx at the HTTP layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountSelectError {
    /// The requested account matched no roster entry — neither an exact
    /// `config_dir` path nor a `derive_account_name` friendly name.
    NotInRoster {
        requested: String,
        roster: Vec<String>,
    },
    /// The requested account is in the roster but has no live credentials
    /// (logged out / unrefreshable) — it would 401 the moment a `claude`
    /// subprocess spawned under it.
    NotLoggedIn { config_dir: String },
}

impl AccountSelectError {
    /// A clear, caller-facing message suitable for a 4xx body.
    pub fn message(&self) -> String {
        match self {
            AccountSelectError::NotInRoster { requested, roster } => {
                format!(
                    "account '{}' not in roster; available: {}",
                    requested,
                    roster.join(", ")
                )
            }
            AccountSelectError::NotLoggedIn { config_dir } => {
                format!(
                    "account at {} has no valid credentials (logged out)",
                    config_dir
                )
            }
        }
    }
}

impl std::fmt::Display for AccountSelectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for AccountSelectError {}

/// The config dir's last path segment — the `label` the per-device account
/// feed puts on the wire.
///
/// Splits on BOTH separators rather than going through `std::path` for the
/// same reason [`derive_account_name`] does: on a Linux host a Windows-style
/// `C:\claude\.claude-hotmail` has no `file_name()` and collapses to one
/// segment, so a roster written on Windows would stop matching its own
/// published labels.
fn roster_basename(config_dir: &str) -> &str {
    config_dir.rsplit(['/', '\\']).next().unwrap_or(config_dir)
}

/// A caller-requested account resolved to a validated, spawnable config dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAccount {
    /// The roster config dir to pin as `CLAUDE_CONFIG_DIR`.
    pub config_dir: String,
    /// The friendly account name (`derive_account_name(config_dir)`).
    pub account_name: String,
    /// `Some(secs)` when the account is currently rate-limited — the caller
    /// asked for it explicitly, so we spawn anyway but surface the risk.
    pub cooldown_remaining_secs: Option<u64>,
}

/// Resolve a caller-supplied `account` (a friendly name like `"hotmail"` OR a
/// full roster `config_dir` path) to a validated config dir. Does NOT consult
/// `account_selection_mode` — this is an explicit per-request override.
///
/// Only roster dirs (`settings::get_claude_config_dirs()`) are selectable: an
/// arbitrary path that happens to hold credentials is rejected with
/// `NotInRoster` so the API cannot point a spawn at an off-roster directory.
///
/// Errors:
/// - [`AccountSelectError::NotInRoster`] — no roster match (→ 400).
/// - [`AccountSelectError::NotLoggedIn`] — matched but no live creds (→ 409).
///
/// A cooldown never fails resolution; it is surfaced via
/// [`ResolvedAccount::cooldown_remaining_secs`] so the caller decides.
pub fn resolve_requested_account(account: &str) -> Result<ResolvedAccount, AccountSelectError> {
    resolve_from(
        account,
        &settings::get_claude_config_dirs(),
        |d| super::oauth_refresh::has_valid_credentials(d),
        |d| super::config::time_until_cooled_down(d),
    )
}

/// Pure core of [`resolve_requested_account`], parameterised over the
/// credential-validity and cooldown lookups so it can be unit-tested without
/// the settings/creds/state singletons (mirrors [`pick_from`]).
///
/// Resolution order:
/// 1. Match `account` against an exact roster entry, else a roster dir whose
///    `derive_account_name` equals `account` case-insensitively, else one whose
///    config-dir BASENAME does (`.claude-hotmail`). No match ⇒ `NotInRoster`
///    (with the available friendly names for the error body).
/// 2. `has_valid_creds(dir)` false ⇒ `NotLoggedIn`.
/// 3. Populate `cooldown_remaining_secs` from `cooldown(dir)`.
///
/// The basename arm exists because that is the `label` the per-device account
/// feed puts on the wire (`commands::ai_settings`'s `usage_twin_report`), and
/// therefore the string an operator picking an account off that feed hands
/// back. Without it, every account name the operator can actually SEE fails to
/// resolve.
fn resolve_from(
    account: &str,
    roster: &[String],
    has_valid_creds: impl Fn(&str) -> bool,
    cooldown: impl Fn(&str) -> Option<Duration>,
) -> Result<ResolvedAccount, AccountSelectError> {
    let dir = roster
        .iter()
        .find(|d| d.as_str() == account)
        .or_else(|| {
            roster
                .iter()
                .find(|d| derive_account_name(d).eq_ignore_ascii_case(account))
        })
        .or_else(|| {
            roster
                .iter()
                .find(|d| roster_basename(d).eq_ignore_ascii_case(account))
        })
        .ok_or_else(|| AccountSelectError::NotInRoster {
            requested: account.to_string(),
            roster: roster.iter().map(|d| derive_account_name(d)).collect(),
        })?;

    if !has_valid_creds(dir) {
        return Err(AccountSelectError::NotLoggedIn {
            config_dir: dir.clone(),
        });
    }

    Ok(ResolvedAccount {
        config_dir: dir.clone(),
        account_name: derive_account_name(dir),
        cooldown_remaining_secs: cooldown(dir).map(|d| d.as_secs()),
    })
}

#[cfg(test)]
mod tests {
    use super::super::config::{record_account_usage, usage_rank, PaceRank};
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
    // settings/state needed. `usage` returns a `UsageRank`: a usable account
    // beats an exhausted one regardless of pace, then under-pace beats unknown
    // beats over-pace, then that tier's own key in that tier's own direction
    // (highest `expected` under pace, lowest `utilization` when unknown,
    // lowest `utilization / expected` ratio over pace) — mirroring the
    // frontend `compareByUsageHeadroom` plus the out-of-tokens guard.

    fn dirs(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// A usable account measured UNDER its projected pace, whose 7-day window
    /// is `expected` of the way through. Higher wins (use-it-or-lose-it).
    fn under(expected: f64) -> UsageRank {
        UsageRank {
            exhausted: false,
            pace: PaceRank::UnderPace { expected },
        }
    }

    /// A usable account measured AT OR OVER its pace, at `ratio =
    /// utilization / expected`. Lower wins.
    fn over(ratio: f64) -> UsageRank {
        UsageRank {
            exhausted: false,
            pace: PaceRank::OverPace { ratio },
        }
    }

    /// A usable account with no usable pace signal, ranked on raw
    /// `utilization`. Lower wins.
    fn unknown(utilization: f64) -> UsageRank {
        UsageRank {
            exhausted: false,
            pace: PaceRank::Unknown { utilization },
        }
    }

    /// Same pace key as `rank`, but flagged exhausted — the dominating tier.
    fn exhausted(rank: UsageRank) -> UsageRank {
        UsageRank {
            exhausted: true,
            pace: rank.pace,
        }
    }

    // Every legacy test injects `|_| true` for the credential-validity filter
    // (all candidates authenticated) so its assertion isolates the ranking it
    // targets. The credential-filter behaviour itself is exercised by the
    // `credential_*` tests at the end of this module.

    #[test]
    fn highest_expected_wins_among_under_pace() {
        let d = dirs(&["/a", "/b", "/c"]);
        // All three are under their projected pace. /b's 7-day window is
        // furthest along (highest `expected`), so its unused capacity expires
        // soonest → chosen, even though /a sorts first by position.
        let chosen = pick_from(
            &d,
            |_| true,  // all valid
            |_| false, // none cooled
            |x| match x {
                "/a" => Some(under(0.30)),
                "/b" => Some(under(0.85)),
                "/c" => Some(under(0.50)),
                _ => None,
            },
            |_| false,
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/b"));
    }

    #[test]
    fn under_pace_beats_unknown_beats_over_pace() {
        let d = dirs(&["/over", "/unknown", "/under"]);
        // The pace TIER dominates each tier's own key: /over has the least
        // possible overrun and /unknown is barely used, but only /under has
        // measured spare capacity that expires at its reset.
        let chosen = pick_from(
            &d,
            |_| true,
            |_| false,
            |x| match x {
                "/over" => Some(over(1.0001)),
                "/unknown" => Some(unknown(0.01)),
                // Lowest possible `expected` inside the winning tier.
                "/under" => Some(under(0.02)),
                _ => None,
            },
            |_| false,
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/under"));

        // Drop the under-pace account: unknown still beats over-pace.
        let d2 = dirs(&["/over", "/unknown"]);
        let chosen2 = pick_from(
            &d2,
            |_| true,
            |_| false,
            |x| match x {
                "/over" => Some(over(1.0001)),
                "/unknown" => Some(unknown(0.99)),
                _ => None,
            },
            |_| false,
            |_| None,
        );
        assert_eq!(
            chosen2.as_deref(),
            Some("/unknown"),
            "an unmeasured account is not KNOWN over budget, so it outranks one that is"
        );
    }

    #[test]
    fn usable_account_beats_exhausted_with_better_pace_key() {
        let d = dirs(&["/full", "/usable"]);
        // /full is under pace with the best possible pace key (its window is
        // 95% elapsed) BUT it is exhausted. /usable is measured over pace
        // (a worse tier) yet has tokens left → must win.
        let chosen = pick_from(
            &d,
            |_| true,
            |_| false,
            |x| match x {
                "/full" => Some(exhausted(under(0.95))), // exhausted, best pace key
                "/usable" => Some(over(1.20)),           // usable, worst pace tier
                _ => None,
            },
            |_| false,
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/usable"));
    }

    #[test]
    fn all_exhausted_picks_best_pace_key_among_them() {
        let d = dirs(&["/a", "/b"]);
        // Both exhausted → the pace key still decides between them, so /b
        // (window furthest along) is the least-bad fallback choice.
        let chosen = pick_from(
            &d,
            |_| true,
            |_| false,
            |x| match x {
                "/a" => Some(exhausted(under(0.20))),
                "/b" => Some(exhausted(under(0.90))),
                _ => None,
            },
            |_| false,
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/b"));
    }

    #[test]
    fn cooled_account_excluded_even_with_best_pace_key() {
        let d = dirs(&["/a", "/b"]);
        // /a has the best pace key but is cooled → must not be picked.
        let chosen = pick_from(
            &d,
            |_| true,
            |x| x == "/a",
            |x| match x {
                "/a" => Some(under(0.99)),
                "/b" => Some(over(1.50)),
                _ => None,
            },
            |_| false,
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/b"));
    }

    // --- end-to-end rosters: real `record_account_usage` → `usage_rank` ------
    //
    // The tests above inject a hand-built `UsageRank` to pin the comparator.
    // These three go through the REAL classifier instead — they record probe
    // fields into the selection snapshot and let `usage_rank` build the key —
    // so they cover the tier boundaries and the ratio guard as well as the
    // ordering. `USAGE_SNAPSHOT` is a process-global keyed by config dir, so
    // every roster below uses dir paths unique to its own test.

    /// REGRESSION — the measured roster the rule was written against.
    ///
    /// Source: `GET http://127.0.0.1:9876/analytics/account-usage` on
    /// merytshost, 2026-09-01, `source == "oauth_usage"`. Re-measure there if
    /// these numbers ever need refreshing.
    ///
    /// | account | actual | expected | delta |
    /// |---|---|---|---|
    /// | `.claude-paktis` | 0.79 | 0.8037 | −0.0137 |
    /// | `.claude-iris` | 0.56 | 0.6489 | −0.0889 |
    /// | `.claude-qontinui` | 0.50 | 0.5894 | −0.0894 |
    /// | `.claude-hotmail` | 0.29 | 0.3810 | −0.0910 |
    /// | `.claude-pakqon` | 0.04 | 0.0596 | −0.0196 |
    /// | `.claude` (gmail) | 1.00 | 0.8275 | +0.1725 (EXHAUSTED) |
    /// | `.claude-paktis-gmail` | 0.76 | 0.7025 | +0.0575 |
    /// | `.claude-tiohorst` | 0.79 | 0.4822 | +0.3078 |
    #[test]
    fn measured_roster_2026_09_01_picks_paktis_not_hotmail() {
        let base = "/test/account_usage/roster_2026_09_01";
        let paktis = format!("{base}/.claude-paktis");
        let iris = format!("{base}/.claude-iris");
        let qontinui = format!("{base}/.claude-qontinui");
        let hotmail = format!("{base}/.claude-hotmail");
        let pakqon = format!("{base}/.claude-pakqon");
        let gmail = format!("{base}/.claude");
        let paktis_gmail = format!("{base}/.claude-paktis-gmail");
        let tiohorst = format!("{base}/.claude-tiohorst");

        record_account_usage(&[
            (paktis.clone(), 0.79, Some(-0.0137), Some(0.8037), false),
            (iris.clone(), 0.56, Some(-0.0889), Some(0.6489), false),
            (qontinui.clone(), 0.50, Some(-0.0894), Some(0.5894), false),
            (hotmail.clone(), 0.29, Some(-0.0910), Some(0.3810), false),
            (pakqon.clone(), 0.04, Some(-0.0196), Some(0.0596), false),
            (gmail.clone(), 1.00, Some(0.1725), Some(0.8275), true),
            (
                paktis_gmail.clone(),
                0.76,
                Some(0.0575),
                Some(0.7025),
                false,
            ),
            (tiohorst.clone(), 0.79, Some(0.3078), Some(0.4822), false),
        ]);

        let d = vec![
            paktis.clone(),
            iris,
            qontinui,
            hotmail.clone(),
            pakqon,
            gmail,
            paktis_gmail,
            tiohorst,
        ];
        let chosen = pick_from(&d, |_| true, |_| false, usage_rank, |_| false, |_| None);

        assert_eq!(
            chosen.as_deref(),
            Some(paktis.as_str()),
            "among the under-pace accounts, paktis' window is furthest along (expected 0.8037), \
             so its spare capacity is the capacity that expires soonest"
        );
        assert_ne!(
            chosen.as_deref(),
            Some(hotmail.as_str()),
            "hotmail is what the DISPLACED min-usage_delta key picked — it is the account with \
             the MOST runway, i.e. the capacity in no danger of expiring"
        );
    }

    /// The constructed pair the measured roster cannot distinguish.
    ///
    /// Both accounts are over pace, and the two candidate rules disagree:
    ///   X = 0.14 used / 0.10 expected → delta **+0.04**, ratio **1.400**
    ///   Y = 0.85 used / 0.80 expected → delta **+0.05**, ratio **1.063**
    ///
    /// Difference-ascending (the displaced key) picks X. The ratio rule picks
    /// Y. **An assertion of X here means the code reverted to the displaced
    /// difference key.**
    #[test]
    fn over_pace_ranks_by_ratio_not_by_difference() {
        let base = "/test/account_usage/ratio_vs_difference";
        let x = format!("{base}/x-early-window");
        let y = format!("{base}/y-late-window");

        record_account_usage(&[
            (x.clone(), 0.14, Some(0.04), Some(0.10), false),
            (y.clone(), 0.85, Some(0.05), Some(0.80), false),
        ]);

        let d = vec![x.clone(), y.clone()];
        let chosen = pick_from(&d, |_| true, |_| false, usage_rank, |_| false, |_| None);

        assert_eq!(
            chosen.as_deref(),
            Some(y.as_str()),
            "Y is 6% past its own pace with the week nearly done; X is 40% past its pace with a \
             nearly-full week to go. Picking X would mean the ranking reverted to usage_delta."
        );
    }

    /// The operator's fallback sentence, literally: "if no accounts have less
    /// than expected token usage, fall back to the ratio calculation."
    /// A roster where EVERY candidate is at or over pace must still return a
    /// pick — `pick_from` never returns `None` with valid, uncooled accounts.
    #[test]
    fn all_over_pace_roster_still_picks_by_ratio() {
        let base = "/test/account_usage/all_over_pace";
        let a = format!("{base}/a"); // 0.60 / 0.50 → ratio 1.200
        let b = format!("{base}/b"); // 0.90 / 0.85 → ratio 1.059  ← least over
        let c = format!("{base}/c"); // 0.30 / 0.10 → ratio 3.000

        record_account_usage(&[
            (a.clone(), 0.60, Some(0.10), Some(0.50), false),
            (b.clone(), 0.90, Some(0.05), Some(0.85), false),
            (c.clone(), 0.30, Some(0.20), Some(0.10), false),
        ]);

        let d = vec![a, b.clone(), c];
        let chosen = pick_from(&d, |_| true, |_| false, usage_rank, |_| false, |_| None);

        assert!(
            chosen.is_some(),
            "spawn-time selection must always pin something"
        );
        assert_eq!(
            chosen.as_deref(),
            Some(b.as_str()),
            "least-over RELATIVE to its own pace wins the fallback tier"
        );
    }

    #[test]
    fn falls_back_to_first_available_when_no_usage_data() {
        let d = dirs(&["/a", "/b", "/c"]);
        // No fresh samples → legacy "first non-cooled" behaviour. /a is cooled,
        // so /b (first available) wins.
        let chosen = pick_from(&d, |_| true, |x| x == "/a", |_| None, |_| false, |_| None);
        assert_eq!(chosen.as_deref(), Some("/b"));
    }

    #[test]
    fn stale_fallback_skips_known_exhausted() {
        let d = dirs(&["/gmail", "/hotmail", "/paktis"]);
        // Stale snapshot: no fresh ranks. /gmail is known-exhausted and NOT
        // cooled (the usage probe's 429 doesn't set an inference cooldown), so
        // the cold-start fallback must skip it and take the first non-exhausted
        // available account.
        let chosen = pick_from(
            &d,
            |_| true,
            |_| false,         // none cooled
            |_| None,          // no fresh usage sample (stale)
            |x| x == "/gmail", // gmail last seen exhausted
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/hotmail"));
    }

    #[test]
    fn stale_fallback_all_known_exhausted_returns_first() {
        let d = dirs(&["/a", "/b"]);
        // Stale snapshot and every available account known-exhausted → still
        // return something (first available) rather than nothing.
        let chosen = pick_from(&d, |_| true, |_| false, |_| None, |_| true, |_| None);
        assert_eq!(chosen.as_deref(), Some("/a"));
    }

    #[test]
    fn ranks_only_accounts_with_samples_then_keeps_them() {
        let d = dirs(&["/a", "/b"]);
        // Only /b has a sample → it is selected (a known-rank account is
        // preferred over an unprobed one).
        let chosen = pick_from(
            &d,
            |_| true,
            |_| false,
            |x| if x == "/b" { Some(under(0.40)) } else { None },
            |_| false,
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/b"));
    }

    #[test]
    fn single_account_is_pinned() {
        // One configured account, no usage sample, not cooled → it is selected
        // (the single-account case must resolve, not no-op).
        let d = dirs(&["/only"]);
        let chosen = pick_from(&d, |_| true, |_| false, |_| None, |_| false, |_| None);
        assert_eq!(chosen.as_deref(), Some("/only"));
    }

    #[test]
    fn single_exhausted_account_still_pinned() {
        // One account, exhausted → still the only option, so still selected.
        let d = dirs(&["/only"]);
        let chosen = pick_from(
            &d,
            |_| true,
            |_| false,
            |_| Some(exhausted(over(1.0))),
            |_| true,
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/only"));
    }

    #[test]
    fn all_cooled_picks_soonest_to_expire() {
        let d = dirs(&["/a", "/b"]);
        let chosen = pick_from(
            &d,
            |_| true,
            |_| true, // all cooled
            |_| None,
            |_| false,
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
        let chosen = pick_from(&d, |_| true, |_| false, |_| None, |_| false, |_| None);
        assert_eq!(chosen, None);
    }

    // --- credential-validity filter (highest precedence) --------------------

    #[test]
    fn credential_invalid_dir_never_selected() {
        let d = dirs(&["/no-creds", "/authed"]);
        // /no-creds has the best pace key but no live credentials → must be
        // excluded entirely; /authed wins despite a worse one.
        let chosen = pick_from(
            &d,
            |x| x == "/authed",
            |_| false,
            |x| match x {
                "/no-creds" => Some(under(0.99)),
                "/authed" => Some(over(1.30)),
                _ => None,
            },
            |_| false,
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/authed"));
    }

    #[test]
    fn expired_refreshable_account_is_selectable() {
        let d = dirs(&["/refreshable"]);
        // `has_valid_credentials` returns true for an expired-but-refreshed dir;
        // model that with the injected validity closure → it stays selectable.
        let chosen = pick_from(
            &d,
            |x| x == "/refreshable",
            |_| false,
            |_| None,
            |_| false,
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/refreshable"));
    }

    /// G1 REGRESSION, end-to-end through the REAL credential filter
    /// (`oauth_refresh::has_valid_credentials`, which IS what
    /// [`pick_best_account`] injects).
    ///
    /// An account whose refresh grant the token endpoint has REVOKED still
    /// holds a non-empty `refreshToken` string on disk forever. If the
    /// credential predicate answers off that string's presence, `LeastUsage`
    /// selection will pin the DEAD account over a healthy one whenever it ranks
    /// better on the pace key — every spawn under it then 401-zombies. The
    /// revoked account must be filtered out before ranking ever happens.
    #[test]
    fn revoked_account_is_never_selected_over_a_healthy_one() {
        fn write_creds(dir: &std::path::Path, refresh_token: &str, expires_at_ms: i64) {
            let body = serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "sk-oauth-test",
                    "refreshToken": refresh_token,
                    "expiresAt": expires_at_ms,
                    "scopes": ["user:inference"],
                }
            });
            std::fs::write(dir.join(".credentials.json"), body.to_string()).expect("write creds");
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        let revoked_dir = tempfile::tempdir().expect("tempdir");
        let healthy_dir = tempfile::tempdir().expect("tempdir");
        // Expired access token + a refresh token the server has revoked.
        write_creds(revoked_dir.path(), "rt-revoked", now_ms - 60_000);
        // Comfortably live.
        write_creds(healthy_dir.path(), "rt-live", now_ms + 3_600_000);

        let revoked = revoked_dir.path().to_string_lossy().to_string();
        let healthy = healthy_dir.path().to_string_lossy().to_string();
        super::super::oauth_refresh::mark_grant_revoked_for_test(&revoked);

        let d = dirs(&[revoked.as_str(), healthy.as_str()]);
        let chosen = pick_from(
            &d,
            // The REAL filter — not a stub.
            |x| super::super::oauth_refresh::has_valid_credentials(x),
            |_| false,
            // The revoked account ranks BEST on the pace key (its window is
            // 99% elapsed, so its spare capacity expires soonest), so it wins the
            // LeastUsage comparison the moment it survives the validity filter.
            |x| {
                if x == revoked {
                    Some(under(0.99))
                } else {
                    Some(over(1.30))
                }
            },
            |_| false,
            |_| None,
        );
        assert_eq!(
            chosen.as_deref(),
            Some(healthy.as_str()),
            "a revoked account must be filtered out before ranking — otherwise LeastUsage pins \
             the dead account over the healthy one"
        );
    }

    #[test]
    fn no_valid_account_returns_none() {
        let d = dirs(&["/a", "/b"]);
        // Every candidate lacks live credentials → no spawnable account. `None`
        // is the signal the spawn path uses to fail loud instead of 401-zombie.
        let chosen = pick_from(&d, |_| false, |_| false, |_| None, |_| false, |_| None);
        assert_eq!(chosen, None);
    }

    #[test]
    fn all_valid_cooled_never_picks_credentialless_dir() {
        let d = dirs(&["/no-creds", "/authed-cooled"]);
        // Every credential-valid account is cooled; a credential-LESS dir has no
        // cooldown (remaining → None → ZERO) and would win the min if the
        // all-cooled fallback didn't restrict to valid dirs. It must not.
        let chosen = pick_from(
            &d,
            |x| x == "/authed-cooled",
            |x| x == "/authed-cooled", // the only valid account is cooled
            |_| None,
            |_| false,
            |x| {
                if x == "/authed-cooled" {
                    Some(Duration::from_secs(300))
                } else {
                    None
                }
            },
        );
        assert_eq!(chosen.as_deref(), Some("/authed-cooled"));
    }

    // --- pick_target_from: migration-target selection ------------------------
    //
    // Migration differs from spawn-time selection in one key way: it may
    // return `None`. Moving a session onto another dead account is pure
    // churn, so exhausted/cooled/credential-less dirs are filtered out
    // entirely instead of falling back to "least bad".

    #[test]
    fn migration_target_highest_expected_under_pace_wins() {
        let d = dirs(&["/gmail", "/paktis"]);
        let chosen = pick_target_from(
            &d,
            |_| true,
            |_| false,
            |x| match x {
                "/gmail" => Some(under(0.30)),
                "/paktis" => Some(under(0.85)), // window furthest along
                _ => None,
            },
            |_| false,
        );
        assert_eq!(chosen.as_deref(), Some("/paktis"));
    }

    #[test]
    fn migration_target_none_when_all_exhausted() {
        let d = dirs(&["/gmail", "/paktis"]);
        let chosen = pick_target_from(
            &d,
            |_| true,
            |_| false,
            |_| Some(exhausted(over(1.0))),
            |_| true, // every candidate known-exhausted
        );
        assert_eq!(
            chosen, None,
            "migrating onto another exhausted account gains nothing"
        );
    }

    #[test]
    fn migration_target_none_when_all_cooled_or_credentialless() {
        let d = dirs(&["/no-creds", "/cooled"]);
        let chosen = pick_target_from(
            &d,
            |x| x == "/cooled", // only /cooled is authenticated…
            |x| x == "/cooled", // …but it's in cooldown
            |_| None,
            |_| false,
        );
        assert_eq!(chosen, None);
    }

    #[test]
    fn migration_target_fresh_exhausted_sample_filtered() {
        // `known_exhausted` (long TTL) says fine, but the FRESH sample says
        // exhausted — the fresh signal must win and the dir be skipped.
        let d = dirs(&["/stale-ok-fresh-dead", "/usable"]);
        let chosen = pick_target_from(
            &d,
            |_| true,
            |_| false,
            |x| match x {
                "/stale-ok-fresh-dead" => Some(exhausted(under(0.95))),
                "/usable" => Some(over(1.20)),
                _ => None,
            },
            |_| false,
        );
        assert_eq!(chosen.as_deref(), Some("/usable"));
    }

    #[test]
    fn migration_target_cold_start_takes_first_surviving() {
        // No fresh usage samples at all: the filters are the only signal —
        // first survivor wins rather than returning None.
        let d = dirs(&["/cooled", "/a", "/b"]);
        let chosen = pick_target_from(&d, |_| true, |x| x == "/cooled", |_| None, |_| false);
        assert_eq!(chosen.as_deref(), Some("/a"));
    }

    // --- resolve_from: per-request account selection ------------------------
    //
    // `resolve_from` is parameterised over the credential-validity + cooldown
    // lookups, so these tests inject closures and fixture roster dirs — no
    // global settings/creds/state needed. Windows-style fixture dirs exercise
    // the cross-platform `derive_account_name` split.

    fn win_roster() -> Vec<String> {
        dirs(&[
            "C:\\claude\\.claude-hotmail",
            "C:\\claude\\.claude-gmail",
            "C:\\claude\\.claude-paktis",
        ])
    }

    #[test]
    fn resolve_in_roster_by_friendly_name() {
        let roster = win_roster();
        let resolved = resolve_from("gmail", &roster, |_| true, |_| None)
            .expect("friendly name should resolve");
        assert_eq!(resolved.config_dir, "C:\\claude\\.claude-gmail");
        assert_eq!(resolved.account_name, "gmail");
        assert_eq!(resolved.cooldown_remaining_secs, None);
    }

    #[test]
    fn resolve_in_roster_by_friendly_name_case_insensitive() {
        let roster = win_roster();
        let resolved = resolve_from("HotMail", &roster, |_| true, |_| None)
            .expect("case-insensitive friendly name should resolve");
        assert_eq!(resolved.config_dir, "C:\\claude\\.claude-hotmail");
        assert_eq!(resolved.account_name, "hotmail");
    }

    #[test]
    fn resolve_in_roster_by_exact_path() {
        let roster = win_roster();
        let resolved = resolve_from("C:\\claude\\.claude-paktis", &roster, |_| true, |_| None)
            .expect("exact path should resolve");
        assert_eq!(resolved.config_dir, "C:\\claude\\.claude-paktis");
        assert_eq!(resolved.account_name, "paktis");
    }

    /// The per-device account feed publishes the config-dir BASENAME as
    /// `label`, so that string has to resolve — otherwise every account name
    /// an operator can see is unusable as a spawn pin.
    #[test]
    fn resolve_in_roster_by_published_label() {
        let roster = win_roster();
        let resolved = resolve_from(".claude-gmail", &roster, |_| true, |_| None)
            .expect("the published wire label should resolve");
        assert_eq!(resolved.config_dir, "C:\\claude\\.claude-gmail");
        assert_eq!(resolved.account_name, "gmail");
    }

    #[test]
    fn resolve_off_roster_name_errors_with_roster_list() {
        let roster = win_roster();
        let err = resolve_from("nonexistent", &roster, |_| true, |_| None)
            .expect_err("off-roster name must error");
        match err {
            AccountSelectError::NotInRoster { requested, roster } => {
                assert_eq!(requested, "nonexistent");
                assert_eq!(roster, vec!["hotmail", "gmail", "paktis"]);
            }
            other => panic!("expected NotInRoster, got {:?}", other),
        }
    }

    #[test]
    fn resolve_logged_out_account_errors() {
        let roster = win_roster();
        // Matched by name but has_valid_creds returns false → NotLoggedIn.
        let err = resolve_from("gmail", &roster, |_| false, |_| None)
            .expect_err("logged-out account must error");
        match err {
            AccountSelectError::NotLoggedIn { config_dir } => {
                assert_eq!(config_dir, "C:\\claude\\.claude-gmail");
            }
            other => panic!("expected NotLoggedIn, got {:?}", other),
        }
    }

    #[test]
    fn resolve_cooled_down_account_populates_remaining_secs() {
        let roster = win_roster();
        let resolved = resolve_from(
            "hotmail",
            &roster,
            |_| true,
            |d| {
                if d == "C:\\claude\\.claude-hotmail" {
                    Some(Duration::from_secs(90))
                } else {
                    None
                }
            },
        )
        .expect("cooled-down account still resolves (warn, not fail)");
        assert_eq!(resolved.account_name, "hotmail");
        assert_eq!(resolved.cooldown_remaining_secs, Some(90));
    }

    #[test]
    fn resolve_error_message_lists_available_names() {
        let roster = win_roster();
        let err = resolve_from("bogus", &roster, |_| true, |_| None).unwrap_err();
        let msg = err.message();
        assert!(msg.contains("bogus"), "message names the request: {}", msg);
        assert!(
            msg.contains("hotmail") && msg.contains("gmail") && msg.contains("paktis"),
            "message lists available accounts: {}",
            msg
        );
    }
}
