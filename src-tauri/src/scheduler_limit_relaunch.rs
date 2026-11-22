//! A scheduled `RemoteAgent` run that dies on an account limit: cool the
//! account it ran under, then relaunch the slot on another account while one
//! remains, or wait for the soonest KNOWN reset when none does.
//!
//! Plan `2026-11-13-devops-scheduled-remote-agent-relaunches-into-an-exhausted-account`
//! (Design 3). Served `production-and-cost` `spawn-routing-reads-account-budget`:
//! never spawn into an account with no budget while another has some, and
//! schedule against the stated reset when all are exhausted.
//!
//! ## What counts as "remains"
//!
//! An account is a relaunch target only when it has valid credentials, is not
//! in a cooldown, AND no successful usage probe marks it exhausted. The last
//! clause is not decoration: at all three of nawa's failures on 2026-11-13
//! (the plan's Phase 0 trace) every one of its 17 accounts sat at or above the
//! 0.99 exhaustion line — 16 at `7d=100%` and `.claude-qontinui` at 99% — and a
//! test on cooldown alone would have relaunched 17 sessions to die in ~6 s
//! each. So a pool the probe already shows exhausted goes straight to the
//! "all N accounts exhausted" record.
//!
//! A probe that FAILED for an account says nothing about its budget: that
//! account is UNKNOWN, stays a relaunch target, and is named "probe failed
//! for" rather than counted as exhausted.
//!
//! ## What sets `next_run`
//!
//! Only a KNOWN reset: an exhausted probe window's `resets_at` /
//! `session_resets_at`, or the reset a limit banner stated. A floor cooldown
//! or an unrelated in-memory cooldown is a guess, not a reset, and never moves
//! `next_run` ([`NextAttempt::override_for`]).
//!
//! ## The seam
//!
//! [`LimitRecovery`] is the three side effects the settle path needs — cool,
//! read the roster, relaunch — so the scheduler's loop is tested without a
//! real probe, a real picker or a real `claude`.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;

use crate::ai_provider::limit_hit::{CoolOutcome, LimitHit};
use crate::commands::ai_settings::AccountUsageInfo;
use crate::scheduler_remote_agent::{RemoteAgentLaunch, RemoteAgentSpec};

/// One account as the relaunch decision sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountAvailability {
    pub dir: String,
    pub valid_credentials: bool,
    /// Remaining cooldown, `None` when not cooled. An EXCLUSION only — its
    /// expiry is not a reset and never schedules anything.
    pub cooled_for: Option<Duration>,
    /// A successful probe (or the recent snapshot) reads it exhausted.
    pub known_exhausted: bool,
    /// The fresh probe ERRORED for it: its budget is UNKNOWN, not exhausted.
    pub probe_failed: bool,
    /// A KNOWN reset: its exhausted probe window's reset.
    pub known_reset: Option<DateTime<Utc>>,
}

/// Why the spawn-time picker cannot steer a relaunch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerBlock {
    /// `account_selection_mode == Manual`: `pick_best_account` is a no-op.
    ManualSelection,
    /// A model gateway is declared: there are no subscription accounts.
    ModelGateway,
}

impl PickerBlock {
    pub fn label(self) -> &'static str {
        match self {
            Self::ManualSelection => "Manual account selection",
            Self::ModelGateway => "model-gateway mode",
        }
    }
}

/// The accounts a relaunch could land on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Roster {
    pub accounts: Vec<AccountAvailability>,
    /// `Some` when a relaunch would land on the same account again.
    pub picker_block: Option<PickerBlock>,
}

/// When the slot can next run, from KNOWN resets only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NextAttempt {
    /// The soonest known reset among the unavailable accounts.
    pub soonest_known: Option<DateTime<Utc>>,
    /// Accounts whose return is UNKNOWN (no known reset).
    pub unknown_for: Vec<String>,
}

impl NextAttempt {
    /// The `next_run` to store instead of the cron slot, if any. A known reset
    /// is used when EVERY account's return is known; when some are unknown it
    /// is used only if it is EARLIER than the cron slot, since an unknown
    /// account may come back first and the cron slot then still tries it.
    /// Nothing known: no override.
    pub fn override_for(&self, cron_next: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
        let known = self.soonest_known?;
        if self.unknown_for.is_empty() || cron_next.is_none_or(|c| known < c) {
            Some(known)
        } else {
            None
        }
    }

    /// `applied` is the `next_run` override actually stored
    /// ([`Self::override_for`]); `None` means the cron slot stands. The row
    /// and the stored `next_run` therefore never disagree.
    fn render(&self, applied: Option<DateTime<Utc>>) -> String {
        let render = crate::ai_provider::limit_hit::render_reset;
        match (applied, self.soonest_known, self.unknown_for.is_empty()) {
            (Some(t), _, true) => format!("next attempt at {}", render(Some(t))),
            (Some(t), _, false) => format!(
                "next attempt at {} (no known reset for: {})",
                render(Some(t)),
                labels(&self.unknown_for)
            ),
            (None, Some(t), false) => format!(
                "next attempt UNKNOWN (no known reset for: {}; soonest known reset {})",
                labels(&self.unknown_for),
                render(Some(t))
            ),
            (None, _, _) => "next attempt UNKNOWN".to_string(),
        }
    }
}

/// What to do after a limited attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RelaunchDecision {
    /// Another account remains: launch the slot again as a new execution,
    /// under exactly `account` (the spawn's account override), so the child
    /// runs where this decision pointed and not where the picker's snapshot
    /// would send it (review round 2 A1: the snapshot reads a failed probe
    /// as exhausted).
    Relaunch { account: String },
    /// None remains.
    Exhausted {
        /// Unavailable accounts a successful probe or a limit hit vouches for.
        exhausted: Vec<String>,
        tried: Vec<String>,
        /// Unavailable accounts whose probe failed: UNKNOWN, not exhausted.
        probe_failed: Vec<String>,
        next_attempt: NextAttempt,
    },
    /// The picker cannot steer (Manual selection, a model gateway).
    NotSteerable {
        block: PickerBlock,
        next_attempt: NextAttempt,
    },
}

impl RelaunchDecision {
    /// The `next_run` this decision stores instead of the cron slot.
    pub fn next_run_override(&self, cron_next: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
        match self {
            Self::Relaunch { .. } => None,
            Self::Exhausted { next_attempt, .. } | Self::NotSteerable { next_attempt, .. } => {
                next_attempt.override_for(cron_next)
            }
        }
    }

    /// The suffix appended to the limited row's error. `limited_dir` is the
    /// account the last attempt ran under; `applied` is the `next_run`
    /// override actually stored (`None`: the cron slot stands).
    pub fn message(&self, limited_dir: &str, applied: Option<DateTime<Utc>>) -> Option<String> {
        match self {
            Self::Relaunch { .. } => None,
            Self::Exhausted {
                exhausted,
                tried,
                probe_failed,
                next_attempt,
            } => {
                let mut out = format!(
                    "all {} accounts exhausted (tried: {})",
                    exhausted.len(),
                    labels(tried)
                );
                if !probe_failed.is_empty() {
                    out.push_str(&format!("; probe failed for: {}", labels(probe_failed)));
                }
                out.push_str("; ");
                out.push_str(&next_attempt.render(applied));
                Some(out)
            }
            Self::NotSteerable {
                block,
                next_attempt,
            } => Some(format!(
                "limit hit on {limited_dir}; the account picker cannot steer in {}, so no \
                 relaunch; {}",
                block.label(),
                next_attempt.render(applied)
            )),
        }
    }
}

/// The pure decision.
///
/// `attempts` is how many attempts this slot has made; the roster size is the
/// hard bound. `known_resets` carries the KNOWN reset each tried account's own
/// cooldown was set to (a probe window or a banner), which the roster's probe
/// alone may not hold (a spend-cap banner).
pub(crate) fn decide(
    roster: &Roster,
    tried: &[String],
    attempts: usize,
    known_resets: &HashMap<String, DateTime<Utc>>,
    now: DateTime<Utc>,
) -> RelaunchDecision {
    let eligible: Vec<AccountAvailability> = roster
        .accounts
        .iter()
        .filter(|a| a.valid_credentials)
        .map(|a| {
            let mut a = a.clone();
            if let Some(r) = known_resets.get(&a.dir) {
                a.known_reset = Some(a.known_reset.map_or(*r, |k| k.max(*r)));
            }
            a
        })
        .collect();
    let unavailable = |a: &AccountAvailability| {
        a.cooled_for.is_some() || a.known_exhausted || tried.contains(&a.dir)
    };

    let next_attempt = {
        let mut soonest_known: Option<DateTime<Utc>> = None;
        let mut unknown_for = Vec::new();
        for a in &eligible {
            match a.known_reset.filter(|t| *t > now) {
                Some(t) if unavailable(a) => {
                    soonest_known = Some(soonest_known.map_or(t, |s| s.min(t)));
                }
                _ => unknown_for.push(a.dir.clone()),
            }
        }
        NextAttempt {
            soonest_known,
            unknown_for,
        }
    };

    if let Some(block) = roster.picker_block {
        return RelaunchDecision::NotSteerable {
            block,
            next_attempt,
        };
    }

    // The account to relaunch onto: one a successful probe vouches for first
    // (in roster order, which `build_roster` sorts most-available first),
    // then one whose probe failed (UNKNOWN, but not known exhausted).
    let pick = eligible
        .iter()
        .filter(|a| !unavailable(a))
        .min_by_key(|a| a.probe_failed);
    if let Some(a) = pick.filter(|_| attempts < roster.accounts.len()) {
        return RelaunchDecision::Relaunch {
            account: a.dir.clone(),
        };
    }

    let (probe_failed, exhausted): (Vec<_>, Vec<_>) = eligible
        .iter()
        .filter(|a| unavailable(a))
        .partition(|a| a.probe_failed && !tried.contains(&a.dir));
    RelaunchDecision::Exhausted {
        exhausted: exhausted.into_iter().map(|a| a.dir.clone()).collect(),
        tried: tried.to_vec(),
        probe_failed: probe_failed.into_iter().map(|a| a.dir.clone()).collect(),
        next_attempt,
    }
}

fn labels(dirs: &[String]) -> String {
    dirs.iter()
        .map(|d| short_label(d))
        .collect::<Vec<_>>()
        .join(", ")
}

fn short_label(dir: &str) -> &str {
    std::path::Path::new(dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(dir)
}

/// What a relaunch needs to start the slot again.
#[derive(Clone)]
pub(crate) struct RelaunchCtx {
    pub spec: RemoteAgentSpec,
    pub bound_port: Option<u16>,
    pub catch_up: Option<crate::scheduler_service::CatchUpContext>,
}

/// The settle path's side effects, injectable for tests.
pub(crate) trait LimitRecovery: Send + Sync {
    /// Cool `dir` until its reset (see
    /// [`crate::ai_provider::limit_hit::cool_until_reset`]).
    fn cool<'a>(&'a self, dir: &'a str, hit: &'a LimitHit) -> BoxFuture<'a, CoolOutcome>;
    /// The roster as it stands NOW, with `tried` always included.
    fn roster(&self, probe: &[AccountUsageInfo], tried: &[String]) -> Roster;
    /// Launch the slot again under exactly `account` — the one relaunch
    /// door, with no unpinned variant and no default, so every implementor
    /// must decide what to do with the account `decide` chose (review round 3
    /// A3: a default that dropped it would silently lose the pinning).
    fn relaunch_on(
        &self,
        spec: RemoteAgentSpec,
        bound_port: Option<u16>,
        account: String,
    ) -> BoxFuture<'static, Result<RemoteAgentLaunch, String>>;
}

/// Pure roster build over the lookups `ProductionLimitRecovery` reads from
/// globals. The fresh `probe` decides exhausted / failed for every dir it
/// covers; a dir it does not cover falls back to the snapshot's verdict.
pub(crate) fn build_roster(
    dirs: Vec<String>,
    probe: &[AccountUsageInfo],
    picker_block: Option<PickerBlock>,
    has_valid_creds: impl Fn(&str) -> bool,
    cooled_for: impl Fn(&str) -> Option<Duration>,
    snapshot_exhausted: impl Fn(&str) -> bool,
    now: DateTime<Utc>,
) -> Roster {
    use crate::ai_provider::limit_hit::{exhausted_window_reset, probe_reads_exhausted};
    let accounts = dirs
        .into_iter()
        .map(|dir| {
            let info = probe.iter().find(|i| i.config_dir == dir);
            let probe_failed = info.is_some_and(|i| i.error.is_some());
            let known_exhausted = match info {
                Some(i) => probe_reads_exhausted(i),
                None => snapshot_exhausted(&dir),
            };
            let load = info
                .filter(|i| i.error.is_none())
                .map(|i| i.utilization.max(i.session_utilization.unwrap_or(0.0)));
            (
                load,
                AccountAvailability {
                    valid_credentials: has_valid_creds(&dir),
                    cooled_for: cooled_for(&dir),
                    known_exhausted,
                    probe_failed,
                    known_reset: info
                        .and_then(|i| exhausted_window_reset(i, now))
                        .map(|(t, _)| t),
                    dir,
                },
            )
        })
        .collect::<Vec<_>>();
    // Least-used first. This is deliberately NOT the picker's ranking: the
    // configured modes (`pick_best_account`) mostly prefer the HIGHEST
    // expected usage among under-pace accounts, to spend capacity that would
    // otherwise expire at the reset. A relaunch is different — the slot just
    // died on a limit — so it takes the account with the most headroom left,
    // which makes a second limit death least likely. The picker's ranking is
    // also not reusable here: it lives behind private, global-reading
    // functions, and `WeightedRandom` (the default) is a random draw. A dir
    // with no successful probe reading sorts after every measured one; the
    // sort is stable.
    let mut accounts = accounts;
    accounts.sort_by(|(a, _), (b, _)| match (a, b) {
        (Some(a), Some(b)) => a.total_cmp(b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let accounts = accounts.into_iter().map(|(_, a)| a).collect();
    Roster {
        accounts,
        picker_block,
    }
}

/// The real probe, cooldown store, picker and spawn.
pub(crate) struct ProductionLimitRecovery;

impl LimitRecovery for ProductionLimitRecovery {
    fn cool<'a>(&'a self, dir: &'a str, hit: &'a LimitHit) -> BoxFuture<'a, CoolOutcome> {
        Box::pin(crate::ai_provider::limit_hit::cool_until_reset(dir, hit))
    }

    fn roster(&self, probe: &[AccountUsageInfo], tried: &[String]) -> Roster {
        let picker_block = if crate::model_gateway::gateway_declared() {
            Some(PickerBlock::ModelGateway)
        } else if crate::claude_accounts::effective_selection_mode()
            == crate::settings::AccountSelectionMode::Manual
        {
            Some(PickerBlock::ManualSelection)
        } else {
            None
        };
        let mut dirs = if picker_block.is_none() {
            crate::settings::get_claude_config_dirs()
        } else {
            Vec::new()
        };
        for t in tried {
            if !dirs.contains(t) {
                dirs.push(t.clone());
            }
        }
        build_roster(
            dirs,
            probe,
            picker_block,
            crate::ai_provider::oauth_refresh::has_valid_credentials,
            crate::ai_provider::time_until_cooled_down,
            crate::ai_provider::account_known_exhausted,
            Utc::now(),
        )
    }

    fn relaunch_on(
        &self,
        spec: RemoteAgentSpec,
        bound_port: Option<u16>,
        account: String,
    ) -> BoxFuture<'static, Result<RemoteAgentLaunch, String>> {
        Box::pin(crate::scheduler_remote_agent::launch_on(
            spec,
            bound_port,
            Some(account),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 13, 17, 27, 10).unwrap()
    }

    fn at(d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, d, h, 0, 0).unwrap()
    }

    fn acct(dir: &str) -> AccountAvailability {
        AccountAvailability {
            dir: dir.to_string(),
            valid_credentials: true,
            cooled_for: None,
            known_exhausted: false,
            probe_failed: false,
            known_reset: None,
        }
    }

    fn steering(accounts: Vec<AccountAvailability>) -> Roster {
        Roster {
            accounts,
            picker_block: None,
        }
    }

    fn no_resets() -> HashMap<String, DateTime<Utc>> {
        HashMap::new()
    }

    #[test]
    fn a_cooled_tried_account_is_excluded_and_another_is_relaunched_onto() {
        let mut a = acct("/h/.claude-paktis");
        a.cooled_for = Some(Duration::from_secs(3600));
        let roster = steering(vec![a, acct("/h/.claude-ben")]);
        assert_eq!(
            decide(
                &roster,
                &["/h/.claude-paktis".into()],
                1,
                &no_resets(),
                now()
            ),
            RelaunchDecision::Relaunch {
                account: "/h/.claude-ben".into()
            }
        );
    }

    #[test]
    fn a_pool_the_probe_shows_exhausted_is_not_relaunched_and_waits_for_the_soonest_reset() {
        // Phase 0 (nawa): every account at or above the exhaustion line. Only
        // the tried one is cooled; the rest are probe-exhausted with resets.
        let mut tried = acct("/h/.claude-paktis");
        tried.cooled_for = Some(Duration::from_secs(5 * 24 * 3600));
        let mut ben = acct("/h/.claude-ben");
        ben.known_exhausted = true;
        ben.known_reset = Some(at(18, 20));
        let mut hotmail = acct("/h/.claude-hotmail");
        hotmail.known_exhausted = true;
        hotmail.known_reset = Some(at(14, 20));
        let roster = steering(vec![tried, ben, hotmail]);
        let resets = HashMap::from([("/h/.claude-paktis".to_string(), at(18, 21))]);
        let d = decide(&roster, &["/h/.claude-paktis".into()], 1, &resets, now());
        assert_eq!(d.next_run_override(Some(now())), Some(at(14, 20)));
        assert_eq!(
            d.message("/h/.claude-paktis", d.next_run_override(Some(now())))
                .unwrap(),
            "all 3 accounts exhausted (tried: .claude-paktis); next attempt at 2026-11-14T20:00:00Z"
        );
    }

    /// A2/A3: a cooldown with no KNOWN reset (a floor guess, an unrelated
    /// in-memory cooldown) never becomes `next_run`.
    #[test]
    fn a_guessed_cooldown_expiry_is_not_a_reset_and_never_overrides_next_run() {
        let mut a = acct("/a");
        a.cooled_for = Some(Duration::from_secs(600));
        let mut b = acct("/b");
        b.cooled_for = Some(Duration::from_secs(60));
        let d = decide(
            &steering(vec![a, b]),
            &["/a".into()],
            1,
            &no_resets(),
            now(),
        );
        assert_eq!(d.next_run_override(Some(at(13, 18))), None);
        assert_eq!(d.next_run_override(None), None);
        assert!(d
            .message("/a", d.next_run_override(Some(at(13, 18))))
            .unwrap()
            .ends_with("; next attempt UNKNOWN"));
    }

    /// A3: with SOME returns unknown, a known reset is kept only when it is
    /// earlier than the cron slot — and the row says exactly what was stored
    /// (review round 2 A3): "next attempt at T" when the override applies,
    /// UNKNOWN only when the cron slot stands.
    #[test]
    fn a_partly_known_pool_overrides_only_with_an_earlier_than_cron_reset() {
        let mut a = acct("/a");
        a.cooled_for = Some(Duration::from_secs(3600));
        let mut b = acct("/b");
        b.known_exhausted = true;
        let resets = HashMap::from([("/a".to_string(), at(13, 19))]);
        let d = decide(&steering(vec![a, b]), &["/a".into()], 1, &resets, now());

        let applied = d.next_run_override(Some(at(13, 20)));
        assert_eq!(applied, Some(at(13, 19)));
        assert!(d
            .message("/a", applied)
            .unwrap()
            .ends_with("; next attempt at 2026-11-13T19:00:00Z (no known reset for: b)"));

        let stands = d.next_run_override(Some(at(13, 18)));
        assert_eq!(stands, None);
        assert!(d.message("/a", stands).unwrap().ends_with(
            "; next attempt UNKNOWN (no known reset for: b; soonest known reset \
             2026-11-13T19:00:00Z)"
        ));
    }

    /// A5: a probe ERROR is UNKNOWN — the account stays a relaunch target, and
    /// once unavailable it is named, not counted as exhausted.
    #[test]
    fn a_failed_probe_is_unknown_not_exhausted() {
        let mut a = acct("/a");
        a.cooled_for = Some(Duration::from_secs(3600));
        let mut c = acct("/c");
        c.probe_failed = true;
        let roster = steering(vec![a.clone(), c.clone()]);
        assert_eq!(
            decide(&roster, &["/a".into()], 1, &no_resets(), now()),
            RelaunchDecision::Relaunch {
                account: "/c".into()
            },
            "an errored probe keeps the account eligible"
        );

        c.cooled_for = Some(Duration::from_secs(60));
        let d = decide(
            &steering(vec![a, c]),
            &["/a".into()],
            1,
            &no_resets(),
            now(),
        );
        assert_eq!(
            d.message("/a", None).unwrap(),
            "all 1 accounts exhausted (tried: a); probe failed for: c; next attempt UNKNOWN"
        );
    }

    /// Review round 2 A1: the relaunch is pinned to the account `decide`
    /// chose. With one probe-FAILED account among uncooled probe-EXHAUSTED
    /// ones, it is the failed one — never an exhausted account the picker's
    /// snapshot would fall back to.
    #[test]
    fn a_pool_with_one_failed_probe_relaunches_onto_that_account_not_an_exhausted_one() {
        let mut tried = acct("/h/.claude-paktis");
        tried.cooled_for = Some(Duration::from_secs(3600));
        let mut roster = vec![tried];
        for d in ["/h/.claude-ben", "/h/.claude-hotmail"] {
            let mut x = acct(d);
            x.known_exhausted = true;
            x.known_reset = Some(at(18, 20));
            roster.push(x);
        }
        let mut c = acct("/h/.claude-niklas");
        c.probe_failed = true;
        roster.push(c);
        assert_eq!(
            decide(
                &steering(roster),
                &["/h/.claude-paktis".into()],
                1,
                &no_resets(),
                now()
            ),
            RelaunchDecision::Relaunch {
                account: "/h/.claude-niklas".into()
            }
        );
    }

    /// An account a successful probe vouches for is preferred over one whose
    /// probe failed, and `build_roster` ranks the measured ones
    /// most-available first, so the pinned relaunch keeps the picker's order.
    #[test]
    fn a_vouched_account_is_preferred_and_the_roster_is_most_available_first() {
        let mut failed = acct("/failed");
        failed.probe_failed = true;
        assert_eq!(
            decide(
                &steering(vec![failed, acct("/ok")]),
                &[],
                0,
                &no_resets(),
                now()
            ),
            RelaunchDecision::Relaunch {
                account: "/ok".into()
            }
        );

        let probe = vec![
            AccountUsageInfo {
                config_dir: "/busy".into(),
                utilization: 0.8,
                ..Default::default()
            },
            AccountUsageInfo {
                config_dir: "/idle".into(),
                utilization: 0.1,
                session_utilization: Some(0.2),
                ..Default::default()
            },
        ];
        let r = build_roster(
            vec!["/unprobed".into(), "/busy".into(), "/idle".into()],
            &probe,
            None,
            |_| true,
            |_| None,
            |_| false,
            now(),
        );
        let order: Vec<&str> = r.accounts.iter().map(|a| a.dir.as_str()).collect();
        assert_eq!(order, vec!["/idle", "/busy", "/unprobed"]);
    }

    #[test]
    fn build_roster_separates_a_probe_error_from_exhaustion() {
        let probe = vec![
            AccountUsageInfo {
                config_dir: "/err".into(),
                utilization: 1.0,
                error: Some("401".into()),
                ..Default::default()
            },
            AccountUsageInfo {
                config_dir: "/full".into(),
                utilization: 1.0,
                resets_at: Some(at(18, 21).timestamp() as u64),
                ..Default::default()
            },
        ];
        let r = build_roster(
            vec!["/err".into(), "/full".into(), "/unprobed".into()],
            &probe,
            None,
            |_| true,
            |_| None,
            |d| d == "/unprobed",
            now(),
        );
        let by = |d: &str| r.accounts.iter().find(|a| a.dir == d).unwrap();
        let err = by("/err");
        assert!(err.probe_failed && !err.known_exhausted && err.known_reset.is_none());
        let full = by("/full");
        assert!(!full.probe_failed && full.known_exhausted);
        assert_eq!(full.known_reset, Some(at(18, 21)));
        assert!(
            by("/unprobed").known_exhausted,
            "no fresh probe: the snapshot decides"
        );
    }

    #[test]
    fn invalid_credentials_neither_count_nor_receive_a_relaunch() {
        let mut a = acct("/a");
        a.cooled_for = Some(Duration::from_secs(600));
        let mut b = acct("/b");
        b.valid_credentials = false;
        let d = decide(
            &steering(vec![a, b]),
            &["/a".into()],
            1,
            &no_resets(),
            now(),
        );
        match d {
            RelaunchDecision::Exhausted { exhausted, .. } => assert_eq!(exhausted, vec!["/a"]),
            other => panic!("expected Exhausted, got {other:?}"),
        }
    }

    #[test]
    fn the_roster_size_bounds_the_attempts() {
        let roster = steering(vec![acct("/a"), acct("/b")]);
        assert!(matches!(
            decide(&roster, &[], 2, &no_resets(), now()),
            RelaunchDecision::Exhausted { .. }
        ));
    }

    /// A6: under Manual selection or a gateway the picker cannot steer; the
    /// message says so instead of "all 1 accounts exhausted".
    #[test]
    fn a_picker_that_cannot_steer_says_so_and_never_relaunches() {
        let mut a = acct("/h/.claude");
        a.cooled_for = Some(Duration::from_secs(3600));
        for (block, label) in [
            (PickerBlock::ManualSelection, "Manual account selection"),
            (PickerBlock::ModelGateway, "model-gateway mode"),
        ] {
            let roster = Roster {
                accounts: vec![a.clone(), acct("/other")],
                picker_block: Some(block),
            };
            let resets = HashMap::from([("/h/.claude".to_string(), at(14, 1))]);
            let d = decide(&roster, &["/h/.claude".into()], 1, &resets, now());
            assert!(matches!(d, RelaunchDecision::NotSteerable { .. }));
            let msg = d.message("/h/.claude", None).unwrap();
            assert!(
                msg.starts_with(&format!(
                    "limit hit on /h/.claude; the account picker cannot steer in {label}, so no relaunch"
                )),
                "{msg}"
            );
            assert!(!msg.contains("accounts exhausted"));
        }
    }
}
