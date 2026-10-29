//! Weekly-usage-aware Claude account selection.
//!
//! [`pick_best_account`] picks an effective Claude config dir from the
//! configured `claude_config_dirs`, using one of four ranking strategies
//! depending on `account_selection_mode`:
//!
//! - `AlternatingHandoff` (the default) and `WeightedRandom`: see "Handoff
//!   strategies" below. At spawn time `AlternatingHandoff` picks exactly as
//!   `HighestExpectedUsage` does (its "account 1"); `WeightedRandom` draws.
//! - `HighestExpectedUsage`: among accounts that are **not** in
//!   a rate-limit cooldown, not exhausted, AND currently under their expected
//!   linear pace (actual 7-day utilization < expected utilization at this
//!   point in the billing window), picks the one with the **highest expected
//!   utilization** — see [`super::config::usage_rank_highest_expected`]. An
//!   account at or over its expected pace is never selected by this tier; an
//!   exhausted account never is either.
//! - `LeastUsage`: ranks by [`super::config::usage_rank`] and
//!   [`super::config::cmp_rank`], mirroring the Terminal "best account" picker
//!   (`compareByUsageHeadroom` in `src/components/settings/types.ts`).
//!   **The rule is use-it-or-lose-it.** Unused weekly capacity expires at the
//!   account's reset and does not roll over, so the account worth burning is
//!   the one whose spare capacity is about to be lost — *not* the emptiest
//!   one, whose runway is in no danger. Concretely: among accounts under
//!   their projected pace, the one whose 7-day window is furthest along wins.
//!   Ranking key, in order (see [`super::config::PaceRank`] for the tier
//!   keys):
//!   1. `exhausted` — the dominating tier. An out-of-tokens / rejected
//!      account is deprioritized no matter how favourable its pace key; a
//!      fully-used account whose window is nearly over still has nothing
//!      left to burn.
//!   2. `low_headroom` — less than `MIN_HEADROOM` left before the reset.
//!      Such an account ranks after every other usable account in every tier,
//!      but before every exhausted one.
//!   3. Pace **tier**: under-pace (under pace by at least `MIN_SHORTFALL`,
//!      with at least `MIN_HEADROOM` left — [`super::config::under_pace_shortfall`])
//!      before unknown (no usable pace signal) before over-pace (at or over
//!      pace, or under it by less than a floor allows).
//!   4. Within a tier only, that tier's own key in its own direction:
//!      - under-pace → `expected` **DESCENDING** (use-it-or-lose-it);
//!      - unknown → raw `utilization` ascending (unchanged behaviour for the
//!        population that never had a pace signal);
//!      - over-pace → the **ratio** `utilization / expected` ASCENDING. A
//!        ratio, not a difference: a difference is not comparable across
//!        accounts at different points in their windows, since +5 points over
//!        at 10% expected is far more over-pace than +5 points over at 80%
//!        expected, yet a difference scores the two identically.
//!
//! The co-pilot relays its planning prompt through this picker, so it chooses
//! the same account a human would when opening a new session.
//!
//! Selection order (both modes share this shape; only the "best among
//! available" step differs):
//! 1. Among non-cooled accounts with a fresh, qualifying usage sample, the
//!    best by the active mode's ranking.
//! 2. If no non-cooled account has a qualifying sample (cold start / stale
//!    snapshot / — for `HighestExpectedUsage` — every account at-or-over
//!    pace), the first non-cooled account that is not *known-exhausted* (per
//!    the last sample, with a longer staleness tolerance); if every available
//!    account is known-exhausted, the first non-cooled. The single-account
//!    case resolves here — the one account is simply pinned.
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
//! [`pick_best_account`] is a no-op when `account_selection_mode == Manual`
//! or no accounts are configured.
//!
//! # Handoff strategies
//!
//! A *handoff* is a token-exhausted session moving to another account
//! ([`pick_migration_target`]). Both strategies consider only the accounts
//! that are under their expected pace with real headroom, usable, out of
//! cooldown and logged in — see [`super::config::under_pace_shortfall`]:
//!
//! - `WeightedRandom`: each account's weight is `expected * (expected -
//!   actual)`. The weights are laid end to end as ranges from 0 to their sum,
//!   and a uniform draw in that span picks the account whose range holds it
//!   ([`weighted_pick`]).
//! - `AlternatingHandoff`: the accounts are sorted by expected usage, highest
//!   first. Handoffs alternate between the first account and each of the
//!   others in turn — 1,2,1,3,1,4,1,2,... — continuing from the handoffs of
//!   the last [`HANDOFF_WINDOW`], kept in an in-memory ledger; with none in the
//!   window the first account gets it ([`alternating_pick`]). "In turn" is
//!   least-recently-served first, so an account whose rank moves mid-cycle is
//!   neither served twice nor skipped. The ledger lives in this process, so a
//!   runner restart starts the sequence over.
//!
//! With no account under pace both fall back to the cross-mode ranking
//! [`pick_target_from`] uses. Every handoff in every mode is recorded in the
//! ledger ([`super::handoff_ledger`]), so switching to `AlternatingHandoff`
//! continues from real history.
//!
//! # Handoff load charge
//!
//! Each recorded handoff charges its target `HANDOFF_LOAD_ESTIMATE`
//! (utilization units), decaying to zero over the handoff window, and every
//! pace reader ranks on `effective_utilization = probed + charge`. So a burst
//! of handoffs ranked off one cached snapshot stops admitting an account once
//! the charges push its shortfall below `MIN_SHORTFALL`, and the next pick
//! moves on. The readers take the charge as an argument; they never lock the
//! ledger (lock order `HANDOFFS` -> `USAGE_SNAPSHOT`, see
//! [`super::handoff_ledger`]).

use super::config::{cmp_rank, HeadroomFloors, UsageRank};
use super::handoff_ledger::{self, HandoffLedger};
use crate::claude_session::federation::derive_account_name;
use crate::settings::{self, AccountSelectionMode};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::info;

/// The charge `charges` holds for `config_dir`, zero when it has none.
fn charge_of(charges: &HashMap<String, f64>, config_dir: &str) -> f64 {
    charges.get(config_dir).copied().unwrap_or(0.0)
}

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
/// `config_dir`). With several, selection prefers (per the active mode) the
/// non-cooled, non-exhausted account either furthest under its expected pace
/// (`LeastUsage`) or, among those under pace, with the highest expected usage
/// (`HighestExpectedUsage` and `AlternatingHandoff`, the default; or a weighted
/// draw under `WeightedRandom`); falls back to first-non-cooled
/// (cold start), then to the soonest-to-expire cooldown when every account is
/// rate-limited. Full key: the module doc.
pub fn pick_best_account() {
    // Subscription accounts are off while a model gateway is declared: there
    // is nothing to pick between (`crate::model_gateway`).
    if crate::model_gateway::gateway_declared() {
        return;
    }
    // The EFFECTIVE mode — the local roster/settings mode resolved against the
    // fleet's `account_selection_mode` policy and this machine's pin — never
    // the raw settings field: the spawn path (`config::get_effective_config_dir`)
    // and the per-device account report read the same resolver, so the picker
    // cannot rotate under one mode while the spawn honours another.
    let mode = crate::claude_accounts::effective_selection_mode();
    if mode == AccountSelectionMode::Manual {
        return;
    }

    let config_dirs = settings::get_claude_config_dirs();
    if config_dirs.is_empty() {
        return;
    }

    // The same pace inputs a handoff ranks on: the floors, and every account's
    // decayed handoff charge — read under a ledger lock released BEFORE the
    // usage snapshot is read, so the lock order matches `pick_migration_target`
    // (HANDOFFS -> USAGE_SNAPSHOT) without nesting at all.
    let floors = HeadroomFloors::effective();
    let charges = handoff_ledger::charge_sums_now();
    let charge = |d: &str| charge_of(&charges, d);
    let low_headroom =
        |d: &str| super::config::usage_rank(d, charge(d), &floors).is_some_and(|r| r.low_headroom);

    let chosen = match mode {
        AccountSelectionMode::WeightedRandom => {
            let candidates = under_pace_candidates(
                &config_dirs,
                |d| {
                    super::oauth_refresh::has_valid_credentials(d)
                        && !super::config::is_account_cooled_down(d)
                },
                |d| super::config::under_pace_shortfall(d, charge(d), &floors),
            );
            weighted_pick(&candidates, rand::random::<f64>()).or_else(|| {
                pick_from_highest_expected(
                    &config_dirs,
                    |d| super::oauth_refresh::has_valid_credentials(d),
                    |d| super::config::is_account_cooled_down(d),
                    |d| super::config::usage_rank_highest_expected(d, charge(d), &floors),
                    |d| super::config::account_known_exhausted(d),
                    low_headroom,
                    |d| super::config::time_until_cooled_down(d),
                )
            })
        }
        // At spawn time the alternating strategy's "account 1" is exactly the
        // highest-expected pick; the alternation applies to handoffs.
        AccountSelectionMode::HighestExpectedUsage | AccountSelectionMode::AlternatingHandoff => {
            pick_from_highest_expected(
                &config_dirs,
                |d| super::oauth_refresh::has_valid_credentials(d),
                |d| super::config::is_account_cooled_down(d),
                |d| super::config::usage_rank_highest_expected(d, charge(d), &floors),
                |d| super::config::account_known_exhausted(d),
                low_headroom,
                |d| super::config::time_until_cooled_down(d),
            )
        }
        AccountSelectionMode::LeastUsage => pick_from(
            &config_dirs,
            |d| super::oauth_refresh::has_valid_credentials(d),
            |d| super::config::is_account_cooled_down(d),
            |d| super::config::usage_rank(d, charge(d), &floors),
            |d| super::config::account_known_exhausted(d),
            |d| super::config::time_until_cooled_down(d),
        ),
        AccountSelectionMode::Manual => unreachable!("returned above"),
    };

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

/// Pure selection core for `AccountSelectionMode::HighestExpectedUsage`,
/// parameterised like [`pick_from`] except for the `usage` closure — here it
/// returns `Some(expected_utilization)` only for an account that qualifies for
/// the primary tier: usable AND under its expected pace with real headroom
/// (mirrors [`super::config::usage_rank_highest_expected`]), and `None` for
/// everything else (exhausted, at-or-over pace, under a floor, or no fresh
/// sample) — and for the extra `low_headroom` closure, which the fallback
/// reads so a thin account (`UsageRank::low_headroom`) is taken only when no
/// account with headroom is available.
///
/// Because a `None` from `usage` is indistinguishable here from "no sample",
/// an over-pace or exhausted account is never chosen by the primary ranking —
/// it can only be picked up by the same cold-start / all-known-exhausted
/// fallback tail [`pick_from`] uses, exactly the "should not be selected
/// [when a better option exists]" behaviour this mode calls for.
///
/// Credential validity and cooldown filtering, and the cold-start / all-
/// cooled fallback tail, are identical to [`pick_from`] — only the "best
/// among available" step differs (highest `expected_utilization` wins here,
/// vs. lowest `(exhausted, headroom)` there).
fn pick_from_highest_expected<'a>(
    config_dirs: &'a [String],
    has_valid_creds: impl Fn(&str) -> bool,
    is_cooled: impl Fn(&str) -> bool,
    usage: impl Fn(&str) -> Option<f64>,
    known_exhausted: impl Fn(&str) -> bool,
    low_headroom: impl Fn(&str) -> bool,
    remaining: impl Fn(&str) -> Option<Duration>,
) -> Option<String> {
    let valid: Vec<&'a String> = config_dirs.iter().filter(|d| has_valid_creds(d)).collect();
    if valid.is_empty() {
        return None;
    }

    let available: Vec<&'a String> = valid.iter().filter(|d| !is_cooled(d)).copied().collect();

    if !available.is_empty() {
        // Among available accounts under their expected pace, the one with
        // the highest expected utilization wins.
        let best = available
            .iter()
            .filter_map(|d| usage(d).map(|expected| (*d, expected)))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(d, _)| d.clone());
        return best
            // No account qualified (none under pace, or cold start / stale
            // snapshot): prefer the first available account NOT
            // known-exhausted and not thin, then the first not
            // known-exhausted — same fallback as `pick_from`, with the
            // headroom tier kept ahead of exhaustion.
            .or_else(|| {
                available
                    .iter()
                    .find(|d| !known_exhausted(d) && !low_headroom(d))
                    .map(|d| (*d).clone())
            })
            .or_else(|| {
                available
                    .iter()
                    .find(|d| !known_exhausted(d))
                    .map(|d| (*d).clone())
            })
            .or_else(|| available.first().map(|d| (*d).clone()));
    }

    // All (credential-valid) accounts cooled: shortest remaining cooldown.
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
///
/// Under `WeightedRandom` and `AlternatingHandoff` the target is chosen by
/// that strategy (module doc, "Handoff strategies") and the ranking above is
/// only the fallback for when no account is under pace. The pick is recorded
/// in the handoff ledger — with its `HANDOFF_LOAD_ESTIMATE` charge — under the
/// same lock that read it, so a burst of concurrent handoffs each sees the
/// ones before it, both in the alternation and in every account's
/// `effective_utilization` (module doc, "Handoff load charge"). A migration
/// refused or failed after the pick withdraws the entry and its charge
/// ([`super::handoff_ledger::unrecord_handoff`]).
pub fn pick_migration_target(exclude_dir: &str) -> Option<String> {
    let config_dirs: Vec<String> = settings::get_claude_config_dirs()
        .into_iter()
        .filter(|d| d != exclude_dir)
        .collect();

    // Everything settings-backed is read BEFORE the ledger lock, so nothing
    // nests inside it but the usage-snapshot reads.
    let pick = HandoffPick {
        mode: crate::claude_accounts::effective_selection_mode(),
        floors: HeadroomFloors::effective(),
        charge: super::config::effective_handoff_load_estimate(),
        roll: rand::random::<f64>(),
    };

    let mut ledger = handoff_ledger::lock();
    pick_migration_target_in(
        &mut ledger,
        Instant::now(),
        &config_dirs,
        pick,
        |d| super::oauth_refresh::has_valid_credentials(d),
        |d| super::config::is_account_cooled_down(d),
        |d| super::config::account_known_exhausted(d),
    )
}

/// The resolved inputs of one handoff pick.
#[derive(Clone, Copy, Debug)]
struct HandoffPick {
    mode: AccountSelectionMode,
    floors: HeadroomFloors,
    /// What this handoff charges its target.
    charge: f64,
    /// A uniform draw in `[0, 1)` for `WeightedRandom`.
    roll: f64,
}

/// [`pick_migration_target`] over an already-held `ledger` and an already
/// filtered roster. Reads the real usage snapshot through the pace readers,
/// handing each the account's decayed charge from `ledger` — never re-locking
/// it — and records the pick, with `pick.charge`, before returning.
fn pick_migration_target_in(
    ledger: &mut HandoffLedger,
    now: Instant,
    config_dirs: &[String],
    pick: HandoffPick,
    has_valid_creds: impl Fn(&str) -> bool,
    is_cooled: impl Fn(&str) -> bool,
    known_exhausted: impl Fn(&str) -> bool,
) -> Option<String> {
    ledger.prune(now);
    let charges = ledger.charge_sums(now);
    let charge = |d: &str| charge_of(&charges, d);
    let floors = pick.floors;
    let under = under_pace_candidates(
        config_dirs,
        |d| has_valid_creds(d) && !is_cooled(d) && !known_exhausted(d),
        |d| super::config::under_pace_shortfall(d, charge(d), &floors),
    );
    let chosen =
        choose_handoff_target(pick.mode, &under, ledger.targets(), pick.roll).or_else(|| {
            pick_target_from(
                config_dirs,
                &has_valid_creds,
                &is_cooled,
                |d| super::config::usage_rank(d, charge(d), &floors),
                &known_exhausted,
            )
        });
    if let Some(dir) = &chosen {
        ledger.push(now, dir, pick.charge);
    }
    chosen
}

/// The strategy half of [`pick_migration_target`], free of globals: `under`
/// is the eligible under-pace accounts sorted by expected usage (highest
/// first), `recent` the targets of the handoffs in the window, oldest first,
/// and `roll` a uniform draw in `[0, 1)`. `None` for a mode with no handoff
/// strategy of its own, or when no account is under pace — the caller then
/// falls back to the cross-mode ranking.
fn choose_handoff_target<'a>(
    mode: AccountSelectionMode,
    under: &[UnderPace],
    recent: impl DoubleEndedIterator<Item = &'a str> + Clone,
    roll: f64,
) -> Option<String> {
    match mode {
        AccountSelectionMode::WeightedRandom => weighted_pick(under, roll),
        AccountSelectionMode::AlternatingHandoff => alternating_pick(under, recent),
        AccountSelectionMode::Manual
        | AccountSelectionMode::LeastUsage
        | AccountSelectionMode::HighestExpectedUsage => None,
    }
}

/// One account under its expected pace: `(config dir, expected utilization,
/// shortfall = expected - effective utilization)`.
type UnderPace = (String, f64, f64);

/// The accounts among `config_dirs` that pass `eligible` and are under their
/// expected pace — `shortfall` is [`super::config::under_pace_shortfall`]
/// with the caller's charge and floors bound — sorted by expected usage,
/// highest first (stable, so ties keep roster order).
fn under_pace_candidates(
    config_dirs: &[String],
    eligible: impl Fn(&str) -> bool,
    shortfall: impl Fn(&str) -> Option<(f64, f64)>,
) -> Vec<UnderPace> {
    let mut under: Vec<UnderPace> = config_dirs
        .iter()
        .filter(|d| eligible(d))
        .filter_map(|d| shortfall(d).map(|(expected, shortfall)| (d.clone(), expected, shortfall)))
        .collect();
    sort_by_expected_desc(&mut under);
    under
}

fn sort_by_expected_desc(under: &mut [UnderPace]) {
    under.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
}

/// `WeightedRandom`: each candidate's weight is `expected * shortfall`; the
/// weights are laid end to end from 0, and `roll` (uniform in `[0, 1)`)
/// scaled to their sum picks the candidate whose range holds it. `None` when
/// there is no candidate or every weight is zero.
fn weighted_pick(candidates: &[UnderPace], roll: f64) -> Option<String> {
    let weights: Vec<f64> = candidates
        .iter()
        .map(|(_, expected, shortfall)| (expected * shortfall).max(0.0))
        .collect();
    let total: f64 = weights.iter().sum();
    if !(total > 0.0) {
        return None;
    }
    let point = roll.clamp(0.0, 1.0) * total;
    let mut end = 0.0;
    for ((dir, _, _), w) in candidates.iter().zip(&weights) {
        end += w;
        if point < end {
            return Some(dir.clone());
        }
    }
    // `roll` at the top of its range: the last account with a weight.
    candidates
        .iter()
        .zip(&weights)
        .rev()
        .find(|(_, w)| **w > 0.0)
        .map(|((dir, _, _), _)| dir.clone())
}

/// `AlternatingHandoff`: `candidates` sorted by expected usage, highest first
/// (account 1 first); `recent` the targets of the handoffs in the window,
/// oldest first. Account 1 gets every other handoff; the ones between go to
/// the other accounts in turn: 1,2,1,3,1,4,1,2,... With no handoff in the
/// window, account 1.
///
/// "In turn" is the other account served least recently, an account never
/// served in the window first and in rank order. With a stable ranking that
/// is exactly 2,3,4,2,...; when ranks move mid-cycle it still serves each
/// account once before any account twice.
fn alternating_pick<'a>(
    candidates: &[UnderPace],
    recent: impl DoubleEndedIterator<Item = &'a str> + Clone,
) -> Option<String> {
    let (first, others) = candidates.split_first()?;
    let first = first.0.as_str();
    let Some(last) = recent.clone().next_back() else {
        return Some(first.to_string());
    };
    // The previous handoff went elsewhere (or nothing else qualifies): back
    // to account 1.
    if last != first || others.is_empty() {
        return Some(first.to_string());
    }
    // It went to account 1: the other account served least recently.
    let last_served = |dir: &str| {
        recent
            .clone()
            .enumerate()
            .filter(|(_, d)| *d == dir)
            .map(|(i, _)| i)
            .last()
    };
    others
        .iter()
        .min_by_key(|(dir, _, _)| match last_served(dir) {
            None => (0, 0),
            Some(i) => (1, i),
        })
        .map(|(dir, _, _)| dir.clone())
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
    use super::super::config::{
        record_account_usage, usage_rank, PaceRank, HANDOFF_LOAD_ESTIMATE, MIN_HEADROOM,
        MIN_SHORTFALL,
    };
    use super::*;
    use std::time::Duration;

    /// `usage_rank` with no handoff charge and the built-in floors — the
    /// classifier as a spawn-time pick with an empty ledger sees it.
    fn rank_now(dir: &str) -> Option<UsageRank> {
        usage_rank(dir, 0.0, &HeadroomFloors::default())
    }

    fn ranked_under_pace(rows: &[(&str, f64, f64)]) -> Vec<UnderPace> {
        let mut v: Vec<UnderPace> = rows
            .iter()
            .map(|(d, e, s)| (d.to_string(), *e, *s))
            .collect();
        sort_by_expected_desc(&mut v);
        v
    }

    /// Drive `alternating_pick` for `n` handoffs, feeding each pick back as
    /// history, and return the account numbers (1-based, by expected rank).
    fn alternate(cands: &[UnderPace], n: usize) -> Vec<usize> {
        let mut history: Vec<String> = Vec::new();
        (0..n)
            .map(|_| {
                let pick = alternating_pick(cands, history.iter().map(String::as_str)).unwrap();
                history.push(pick.clone());
                cands.iter().position(|(d, _, _)| *d == pick).unwrap() + 1
            })
            .collect()
    }

    #[test]
    fn alternating_pick_follows_the_one_two_one_three_sequence() {
        let cands = ranked_under_pace(&[
            ("b", 0.6, 0.1),
            ("a", 0.8, 0.1),
            ("d", 0.2, 0.1),
            ("c", 0.4, 0.1),
        ]);
        assert_eq!(alternate(&cands, 10), vec![1, 2, 1, 3, 1, 4, 1, 2, 1, 3]);
    }

    #[test]
    fn alternating_pick_without_history_is_account_one() {
        let cands = ranked_under_pace(&[("a", 0.3, 0.1), ("b", 0.7, 0.1)]);
        assert_eq!(
            alternating_pick(&cands, std::iter::empty()),
            Some("b".into())
        );
    }

    #[test]
    fn alternating_pick_returns_to_account_one_after_any_other_target() {
        let cands = ranked_under_pace(&[("a", 0.8, 0.1), ("b", 0.6, 0.1)]);
        // The last handoff went to an account that no longer qualifies.
        assert_eq!(
            alternating_pick(&cands, ["gone"].into_iter()),
            Some("a".into())
        );
        assert_eq!(
            alternating_pick(&cands, ["a", "b"].into_iter()),
            Some("a".into())
        );
    }

    #[test]
    fn alternating_pick_continues_after_the_last_other_in_the_window() {
        let cands = ranked_under_pace(&[("a", 0.8, 0.1), ("b", 0.6, 0.1), ("c", 0.4, 0.1)]);
        assert_eq!(
            alternating_pick(&cands, ["b", "a"].into_iter()),
            Some("c".into())
        );
        assert_eq!(
            alternating_pick(&cands, ["c", "a"].into_iter()),
            Some("b".into())
        );
    }

    #[test]
    fn alternating_pick_single_account_and_empty() {
        let one = ranked_under_pace(&[("a", 0.5, 0.1)]);
        assert_eq!(alternate(&one, 3), vec![1, 1, 1]);
        assert_eq!(alternating_pick(&[], std::iter::empty()), None);
    }

    #[test]
    fn weighted_pick_maps_the_roll_onto_consecutive_ranges() {
        // Weights: a = 0.5*0.2 = 0.1, b = 0.25*0.4 = 0.1, c = 0.8*0.25 = 0.2;
        // sorted by expected: c [0, 0.2), a [0.2, 0.3), b [0.3, 0.4).
        let cands = ranked_under_pace(&[("a", 0.5, 0.2), ("b", 0.25, 0.4), ("c", 0.8, 0.25)]);
        assert_eq!(weighted_pick(&cands, 0.0), Some("c".into()));
        assert_eq!(weighted_pick(&cands, 0.49), Some("c".into()));
        assert_eq!(weighted_pick(&cands, 0.51), Some("a".into()));
        assert_eq!(weighted_pick(&cands, 0.76), Some("b".into()));
        assert_eq!(weighted_pick(&cands, 1.0), Some("b".into()));
    }

    #[test]
    fn weighted_pick_has_nothing_to_draw_from_zero_weights() {
        assert_eq!(weighted_pick(&[], 0.5), None);
        assert_eq!(
            weighted_pick(&ranked_under_pace(&[("a", 0.0, 0.3)]), 0.5),
            None
        );
    }

    #[test]
    fn alternating_pick_neither_repeats_nor_skips_when_ranks_move() {
        // b and c were served; c's rank has since risen above b's, and d was
        // never served. The next "other" is d, not b again.
        let cands = ranked_under_pace(&[
            ("a", 0.9, 0.1),
            ("c", 0.7, 0.1),
            ("b", 0.6, 0.1),
            ("d", 0.5, 0.1),
        ]);
        assert_eq!(
            alternating_pick(&cands, ["a", "b", "a", "c", "a"].into_iter()),
            Some("d".into())
        );
        // All served: the one served longest ago.
        assert_eq!(
            alternating_pick(&cands, ["a", "b", "a", "c", "a", "d", "a"].into_iter()),
            Some("b".into())
        );
    }

    #[test]
    fn choose_handoff_target_dispatches_on_mode() {
        let cands = ranked_under_pace(&[("a", 0.8, 0.1), ("b", 0.6, 0.3)]);
        let none: [&str; 0] = [];
        assert_eq!(
            choose_handoff_target(
                AccountSelectionMode::AlternatingHandoff,
                &cands,
                none.into_iter(),
                0.0
            ),
            Some("a".into())
        );
        // Weights: a 0.08, b 0.18; a roll of 0.5 lands in b's range.
        assert_eq!(
            choose_handoff_target(
                AccountSelectionMode::WeightedRandom,
                &cands,
                none.into_iter(),
                0.5
            ),
            Some("b".into())
        );
        for mode in [
            AccountSelectionMode::Manual,
            AccountSelectionMode::LeastUsage,
            AccountSelectionMode::HighestExpectedUsage,
        ] {
            assert_eq!(
                choose_handoff_target(mode, &cands, none.into_iter(), 0.5),
                None
            );
        }
        // Nothing under pace: no strategy pick, the caller falls back.
        assert_eq!(
            choose_handoff_target(
                AccountSelectionMode::AlternatingHandoff,
                &[],
                none.into_iter(),
                0.0
            ),
            None
        );
    }

    #[test]
    fn alternating_pick_without_the_exhausted_source_starts_from_the_next_account() {
        // Account 1 ("a") ran out and is the source, so it is not a candidate:
        // the sequence runs over the remaining accounts, b first.
        let cands = ranked_under_pace(&[("b", 0.6, 0.1), ("c", 0.4, 0.1)]);
        assert_eq!(
            alternating_pick(&cands, ["a", "b", "a"].into_iter()),
            Some("b".into())
        );
        assert_eq!(
            alternating_pick(&cands, ["a", "b"].into_iter()),
            Some("c".into())
        );
    }

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
            low_headroom: false,
            pace: PaceRank::UnderPace { expected },
        }
    }

    /// A usable account measured AT OR OVER its pace, at `ratio =
    /// utilization / expected`. Lower wins.
    fn over(ratio: f64) -> UsageRank {
        UsageRank {
            exhausted: false,
            low_headroom: false,
            pace: PaceRank::OverPace { ratio },
        }
    }

    /// A usable account with no usable pace signal, ranked on raw
    /// `utilization`. Lower wins.
    fn unknown(utilization: f64) -> UsageRank {
        UsageRank {
            exhausted: false,
            low_headroom: false,
            pace: PaceRank::Unknown { utilization },
        }
    }

    /// Same pace key as `rank`, but flagged exhausted — the dominating tier.
    fn exhausted(rank: UsageRank) -> UsageRank {
        UsageRank {
            exhausted: true,
            ..rank
        }
    }

    /// Same pace key as `rank`, but with less than `MIN_HEADROOM` left.
    fn thin(rank: UsageRank) -> UsageRank {
        UsageRank {
            low_headroom: true,
            ..rank
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
    ///
    /// Since the `MIN_SHORTFALL` floor (plan
    /// `2026-10-19-usage-limit-handoff-lands-sessions-on-accounts-that-cannot-absorb-them`
    /// D1), paktis' 1.37-point shortfall is below the floor: it is demoted to
    /// the over-pace tier — exactly the by-a-hair admission that sent ten
    /// sessions onto pakqon on 2026-10-18. Among the accounts still under pace
    /// with real margin, iris' window is furthest along, so iris wins, and the
    /// use-it-or-lose-it half of the regression (never hotmail) still holds.
    #[test]
    fn measured_roster_2026_09_01_picks_iris_not_hotmail() {
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
            iris.clone(),
            qontinui,
            hotmail.clone(),
            pakqon.clone(),
            gmail,
            paktis_gmail,
            tiohorst,
        ];
        let chosen = pick_from(&d, |_| true, |_| false, rank_now, |_| false, |_| None);

        for thin_margin in [&paktis, &pakqon] {
            assert!(
                matches!(
                    rank_now(thin_margin).map(|r| r.pace),
                    Some(PaceRank::OverPace { .. })
                ),
                "{thin_margin}: under pace by less than MIN_SHORTFALL ({MIN_SHORTFALL}) is demoted"
            );
        }
        assert_eq!(
            chosen.as_deref(),
            Some(iris.as_str()),
            "among the accounts under pace past the floor, iris' window is furthest along \
             (expected 0.6489), so its spare capacity is the capacity that expires soonest"
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
        let chosen = pick_from(&d, |_| true, |_| false, rank_now, |_| false, |_| None);

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
        let chosen = pick_from(&d, |_| true, |_| false, rank_now, |_| false, |_| None);

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

    // --- pick_from_highest_expected: the HighestExpectedUsage ranking core --
    //
    // `usage` returns `Some(expected_utilization)` only for accounts under
    // their expected pace (mirrors `usage_rank_highest_expected`); `None`
    // (over-pace, exhausted, or no sample) falls through to the same
    // cold-start / cooldown fallback `pick_from` uses.

    #[test]
    fn highest_expected_wins_among_under_pace_accounts() {
        let d = dirs(&["/a", "/b", "/c"]);
        // All under pace; /b has the highest expected utilization → chosen.
        let chosen = pick_from_highest_expected(
            &d,
            |_| true,
            |_| false,
            |x| match x {
                "/a" => Some(0.40),
                "/b" => Some(0.90),
                "/c" => Some(0.55),
                _ => None,
            },
            |_| false,
            |_| false,
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/b"));
    }

    #[test]
    fn over_pace_account_excluded_even_with_highest_expected() {
        let d = dirs(&["/over", "/under"]);
        // /over has a higher expected_utilization but is at/over pace (usage()
        // returns None for it, mirroring usage_rank_highest_expected) → must
        // not be picked; /under wins despite lower expected usage.
        let chosen = pick_from_highest_expected(
            &d,
            |_| true,
            |_| false,
            |x| match x {
                "/over" => None,
                "/under" => Some(0.30),
                _ => None,
            },
            |_| false,
            |_| false,
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/under"));
    }

    #[test]
    fn all_over_pace_falls_back_to_first_not_known_exhausted() {
        let d = dirs(&["/a", "/b"]);
        // Neither account is under pace (usage() → None for both) → falls
        // back like a stale snapshot: first available not known-exhausted.
        let chosen = pick_from_highest_expected(
            &d,
            |_| true,
            |_| false,
            |_| None,
            |x| x == "/a",
            |_| false,
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/b"));
    }

    #[test]
    fn highest_expected_cooled_account_excluded() {
        let d = dirs(&["/a", "/b"]);
        // /a has the higher expected usage but is cooled → must not be picked.
        let chosen = pick_from_highest_expected(
            &d,
            |_| true,
            |x| x == "/a",
            |x| match x {
                "/a" => Some(0.95),
                "/b" => Some(0.30),
                _ => None,
            },
            |_| false,
            |_| false,
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/b"));
    }

    #[test]
    fn highest_expected_all_cooled_picks_soonest_to_expire() {
        let d = dirs(&["/a", "/b"]);
        let chosen = pick_from_highest_expected(
            &d,
            |_| true,
            |_| true, // all cooled
            |_| None,
            |_| false,
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
    fn highest_expected_single_account_is_pinned() {
        let d = dirs(&["/only"]);
        let chosen = pick_from_highest_expected(
            &d,
            |_| true,
            |_| false,
            |_| None,
            |_| false,
            |_| false,
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/only"));
    }

    #[test]
    fn highest_expected_credential_invalid_dir_never_selected() {
        let d = dirs(&["/no-creds", "/authed"]);
        // /no-creds has the highest expected usage but no live credentials →
        // excluded entirely; /authed wins despite lower expected usage.
        let chosen = pick_from_highest_expected(
            &d,
            |x| x == "/authed",
            |_| false,
            |x| match x {
                "/no-creds" => Some(0.95),
                "/authed" => Some(0.30),
                _ => None,
            },
            |_| false,
            |_| false,
            |_| None,
        );
        assert_eq!(chosen.as_deref(), Some("/authed"));
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

    // --- headroom floors and the handoff load charge -------------------------
    //
    // Plan `2026-10-19-usage-limit-handoff-lands-sessions-on-accounts-that-cannot-absorb-them`
    // Phase 1. `USAGE_SNAPSHOT` is process-global and keyed by config dir, so
    // every dir below is unique to its test; each test drives its OWN
    // `HandoffLedger` rather than the process static, so no two tests share
    // handoff state.

    #[test]
    fn low_headroom_sorts_after_every_usable_account_but_before_exhausted() {
        let d = dirs(&["/thin", "/over", "/unknown", "/dead"]);
        let usage = |x: &str| match x {
            // Under pace on paper, with the best possible key — but thin.
            "/thin" => Some(thin(under(0.99))),
            "/over" => Some(over(3.0)),
            "/unknown" => Some(unknown(0.80)),
            "/dead" => Some(exhausted(under(0.99))),
            _ => None,
        };
        let mut order: Vec<&String> = d.iter().collect();
        order.sort_by(|a, b| cmp_rank(&usage(a).unwrap(), &usage(b).unwrap()));
        assert_eq!(order, vec!["/unknown", "/over", "/thin", "/dead"]);

        // Both pickers agree: a usable account with headroom wins…
        assert_eq!(
            pick_from(&d, |_| true, |_| false, usage, |_| false, |_| None).as_deref(),
            Some("/unknown")
        );
        assert_eq!(
            pick_target_from(&d, |_| true, |_| false, usage, |_| false).as_deref(),
            Some("/unknown")
        );
        // …and the thin account is still pickable when it is the only
        // non-exhausted one — demoted, never excluded.
        let only = dirs(&["/dead", "/thin"]);
        assert_eq!(
            pick_from(&only, |_| true, |_| false, usage, |_| false, |_| None).as_deref(),
            Some("/thin")
        );
        assert_eq!(
            pick_target_from(&only, |_| true, |_| false, usage, |_| false).as_deref(),
            Some("/thin")
        );
    }

    #[test]
    fn low_headroom_through_the_real_classifier() {
        let base = "/test/account_usage/low_headroom_classifier";
        // 0.93 used of 0.99 expected: a 6-point shortfall, but only 7 points
        // left before the reset (< MIN_HEADROOM).
        let thin_dir = format!("{base}/thin");
        // Well over pace, plenty left.
        let over_dir = format!("{base}/over");
        let dead_dir = format!("{base}/dead");
        record_account_usage(&[
            (thin_dir.clone(), 0.93, Some(-0.06), Some(0.99), false),
            (over_dir.clone(), 0.50, Some(0.20), Some(0.30), false),
            (dead_dir.clone(), 1.00, Some(0.10), Some(0.90), true),
        ]);
        let thin_rank = rank_now(&thin_dir).unwrap();
        assert!(thin_rank.low_headroom);
        assert!(
            matches!(thin_rank.pace, PaceRank::OverPace { .. }),
            "the headroom floor fails, so it is not under pace either"
        );
        assert_eq!(
            super::super::config::under_pace_shortfall(&thin_dir, 0.0, &HeadroomFloors::default()),
            None
        );
        assert!(1.0 - 0.93 < MIN_HEADROOM);

        let all = vec![thin_dir.clone(), over_dir.clone(), dead_dir.clone()];
        assert_eq!(
            pick_target_from(&all, |_| true, |_| false, rank_now, |_| false),
            Some(over_dir.clone())
        );
        let usable = |d: &str| d != over_dir;
        assert_eq!(
            pick_target_from(&all, usable, |_| false, rank_now, |_| false),
            Some(thin_dir.clone()),
            "the only non-exhausted account stays pickable"
        );
        // The spawn-time HighestExpected fallback skips the thin account while
        // a healthy one is available, and takes it when none is.
        let thin_flag = |d: &str| rank_now(d).is_some_and(|r| r.low_headroom);
        let hexp = |d: &str| {
            super::super::config::usage_rank_highest_expected(d, 0.0, &HeadroomFloors::default())
        };
        let roster = vec![thin_dir.clone(), over_dir.clone()];
        assert_eq!(
            pick_from_highest_expected(
                &roster,
                |_| true,
                |_| false,
                hexp,
                |_| false,
                thin_flag,
                |_| None
            ),
            Some(over_dir.clone())
        );
        assert_eq!(
            pick_from_highest_expected(
                &[thin_dir.clone()],
                |_| true,
                |_| false,
                hexp,
                |_| false,
                thin_flag,
                |_| None
            ),
            Some(thin_dir.clone())
        );
    }

    /// One account's probe result as `refresh_account_usage_snapshot` would
    /// return it from the 60 s probe cache.
    fn probe_body(
        dir: &str,
        utilization: f64,
        expected: f64,
    ) -> crate::commands::ai_settings::AccountUsageInfo {
        crate::commands::ai_settings::AccountUsageInfo {
            config_dir: dir.to_string(),
            label: dir.to_string(),
            utilization,
            expected_utilization: Some(expected),
            usage_delta: Some(utilization - expected),
            source: Some("oauth_usage".to_string()),
            ..Default::default()
        }
    }

    fn pick_with(mode: AccountSelectionMode, charge: f64) -> HandoffPick {
        HandoffPick {
            mode,
            floors: HeadroomFloors::default(),
            charge,
            // Roll 0 always lands in the first (highest-expected) range of
            // `WeightedRandom`, so without the charge every draw is account 1.
            roll: 0.0,
        }
    }

    /// Acceptance (Phase 1): fifteen handoffs in one minute, with the snapshot
    /// re-recorded from the SAME cached bodies before every pick — the
    /// 2026-10-18 22:30:53–22:31:37 burst shape — put no more handoffs on any
    /// account than `(shortfall - MIN_SHORTFALL) / HANDOFF_LOAD_ESTIMATE`
    /// allows (+1: the pick that crosses the floor is the last one admitted).
    /// The charge rides on the ledger, so re-recording cannot wipe it.
    #[test]
    fn burst_from_one_rerecorded_snapshot_spreads_once_charges_cross_the_floor() {
        for (tag, mode) in [
            ("hexp", AccountSelectionMode::HighestExpectedUsage),
            ("alt", AccountSelectionMode::AlternatingHandoff),
            ("weighted", AccountSelectionMode::WeightedRandom),
        ] {
            let base = format!("/test/account_usage/burst_{tag}");
            // (dir, utilization, expected) → shortfall 0.11 / 0.13 / 0.09.
            let rows = [
                (format!("{base}/a"), 0.79, 0.90),
                (format!("{base}/b"), 0.57, 0.70),
                (format!("{base}/c"), 0.41, 0.50),
            ];
            let bodies: Vec<_> = rows.iter().map(|(d, u, e)| probe_body(d, *u, *e)).collect();
            let config_dirs: Vec<String> = rows.iter().map(|(d, _, _)| d.clone()).collect();
            let allowed = |shortfall: f64| {
                ((shortfall - MIN_SHORTFALL) / HANDOFF_LOAD_ESTIMATE + 1e-9).floor() as usize + 1
            };

            let mut ledger = HandoffLedger::new();
            let t0 = Instant::now();
            let mut counts: HashMap<String, usize> = HashMap::new();
            for i in 0..15u64 {
                // Every hint re-probes first; the probe cache hands back the
                // identical bodies and `record_usage_snapshot` REPLACES each
                // sample with a fresh `captured_at`.
                crate::commands::ai_settings::record_usage_snapshot(&bodies);
                let picked = pick_migration_target_in(
                    &mut ledger,
                    t0 + Duration::from_secs(4 * i),
                    &config_dirs,
                    pick_with(mode, HANDOFF_LOAD_ESTIMATE),
                    |_| true,
                    |_| false,
                    |_| false,
                )
                .expect("a usable roster always yields a target");
                *counts.entry(picked).or_insert(0) += 1;
            }
            for (dir, u, e) in &rows {
                assert_eq!(
                    counts.get(dir).copied().unwrap_or(0),
                    allowed(e - u),
                    "{tag}: {dir} (shortfall {:.2}) took {:?} of 15",
                    e - u,
                    counts
                );
            }
            assert_eq!(ledger.len(), 15);

            // Every account is now charged below the floor: the next pick is
            // the demoted fallback, not a refusal.
            crate::commands::ai_settings::record_usage_snapshot(&bodies);
            let after = t0 + Duration::from_secs(60);
            for (dir, _, _) in &rows {
                assert_eq!(
                    super::super::config::under_pace_shortfall(
                        dir,
                        ledger.charge_sum(dir, after),
                        &HeadroomFloors::default()
                    ),
                    None,
                    "{tag}: {dir} is charged below MIN_SHORTFALL"
                );
            }
            let fallback = pick_migration_target_in(
                &mut ledger,
                after,
                &config_dirs,
                pick_with(mode, HANDOFF_LOAD_ESTIMATE),
                |_| true,
                |_| false,
                |_| false,
            );
            assert!(fallback.is_some(), "{tag}: demoted, never excluded");
        }
    }

    /// Without the charge the same burst is the incident: every pick lands on
    /// the account ranked first. Pins that the spreading above IS the charge.
    #[test]
    fn burst_without_a_charge_lands_every_handoff_on_one_account() {
        let base = "/test/account_usage/burst_no_charge";
        let a = format!("{base}/a");
        let b = format!("{base}/b");
        let bodies = vec![probe_body(&a, 0.79, 0.90), probe_body(&b, 0.57, 0.70)];
        let config_dirs = vec![a.clone(), b.clone()];
        let mut ledger = HandoffLedger::new();
        let t0 = Instant::now();
        for i in 0..10u64 {
            crate::commands::ai_settings::record_usage_snapshot(&bodies);
            let picked = pick_migration_target_in(
                &mut ledger,
                t0 + Duration::from_secs(4 * i),
                &config_dirs,
                pick_with(AccountSelectionMode::HighestExpectedUsage, 0.0),
                |_| true,
                |_| false,
                |_| false,
            );
            assert_eq!(picked.as_deref(), Some(a.as_str()));
        }
    }

    #[test]
    fn charge_decays_to_zero_at_the_window_and_a_rerecorded_probe_keeps_it() {
        let dir = "/test/account_usage/charge_decay/a";
        // Shortfall 0.03: one charge of 0.02 takes it to 0.01, below the floor.
        let bodies = vec![probe_body(dir, 0.60, 0.63)];
        let floors = HeadroomFloors::default();
        let shortfall = |charge: f64| {
            super::super::config::under_pace_shortfall(dir, charge, &floors).map(|(_, s)| s)
        };
        crate::commands::ai_settings::record_usage_snapshot(&bodies);

        let t0 = Instant::now();
        let mut ledger = HandoffLedger::new();
        ledger.push(t0, dir, HANDOFF_LOAD_ESTIMATE);

        let soon = t0 + Duration::from_secs(60);
        assert_eq!(shortfall(ledger.charge_sum(dir, soon)), None);
        // A fresh probe a minute later shows none of the moved load and
        // REPLACES the sample — the charge is on the ledger, so it survives.
        crate::commands::ai_settings::record_usage_snapshot(&bodies);
        assert_eq!(shortfall(ledger.charge_sum(dir, soon)), None);

        // Decayed past half: 0.03 - 0.0099 >= 0.02, so it is admitted again.
        let later = t0 + handoff_ledger::HANDOFF_WINDOW / 2 + Duration::from_secs(60);
        assert!(shortfall(ledger.charge_sum(dir, later)).is_some());
        // At the window the charge is gone entirely.
        let at_window = t0 + handoff_ledger::HANDOFF_WINDOW;
        assert_eq!(ledger.charge_sum(dir, at_window), 0.0);
        let full = shortfall(ledger.charge_sum(dir, at_window)).unwrap();
        assert!(
            (full - 0.03).abs() < 1e-9,
            "full shortfall back, got {full}"
        );
    }

    #[test]
    fn unrecorded_handoff_takes_its_charge_with_it() {
        let dir = "/test/account_usage/unrecord_charge/a";
        let bodies = vec![probe_body(dir, 0.60, 0.63)];
        crate::commands::ai_settings::record_usage_snapshot(&bodies);
        let floors = HeadroomFloors::default();
        let t0 = Instant::now();
        let mut ledger = HandoffLedger::new();
        let picked = pick_migration_target_in(
            &mut ledger,
            t0,
            &[dir.to_string()],
            pick_with(
                AccountSelectionMode::AlternatingHandoff,
                HANDOFF_LOAD_ESTIMATE,
            ),
            |_| true,
            |_| false,
            |_| false,
        );
        assert_eq!(picked.as_deref(), Some(dir));
        assert_eq!(
            super::super::config::under_pace_shortfall(dir, ledger.charge_sum(dir, t0), &floors),
            None,
            "the pick charged the account below the floor"
        );
        // The migration failed: withdraw it.
        assert!(ledger.unrecord(dir));
        assert_eq!(ledger.len(), 0);
        assert!(super::super::config::under_pace_shortfall(
            dir,
            ledger.charge_sum(dir, t0),
            &floors
        )
        .is_some());
    }
}
