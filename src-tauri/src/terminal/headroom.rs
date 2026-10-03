//! Proactive account migration from headroom (plan
//! `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
//! Phase 8).
//!
//! The reactive path (`usage_limit` → `account_migration::handle_usage_limit_hint`)
//! waits for the limit to fire. This module adds a second TRIGGER into the same
//! confirm → cooldown → cap → `pick_migration_target` path, fed by the headroom
//! readings `agent_metrics` already holds per account:
//!
//! - utilization ≥ the threshold (5-hour or 7-day), **or**
//! - the burn-rate projection over the current 5-hour window
//!   ([`projected_exhaustion_at`]) says the account runs dry before its window
//!   resets, within [`PROJECTION_HORIZON_MS`].
//!
//! ## A reading is only a hint
//!
//! Anything local can forge a headroom reading (a `.claude.json` is a user
//! file). So a trigger never migrates by itself: the confirm re-runs the
//! OAuth probe and only the PROBE decides ([`probe_confirms`]) — its fresh
//! reading at/over the threshold, or a projection over the probe's OWN
//! five-hour samples (never the cached file's) that agrees. A forged 100 %
//! with a healthy probe migrates nothing. (The reactive path's confirm —
//! "the probe says exhausted" — is unchanged; it would make this trigger dead,
//! since a 95 % threshold fires well before the probe reads exhausted.)
//!
//! ## Only at a turn boundary
//!
//! A trigger fires only at a POSITIVE, AUTHORITATIVE turn boundary: the
//! verdict is `TurnEnded` with `Confidence::Authoritative` (a hook `Stop` or
//! the agent's own sideband `finished`). A `NeedsYou` of any reason is not a
//! boundary (the turn is paused mid-flight, waiting on the human), nor is a
//! `TurnEnded` read off the screen or inferred. `Working`, `Unknown`,
//! `Starting` and `Failed` defer too — an unknown state is not evidence that
//! no turn is in flight, and migrating mid-turn would cut it off.
//!
//! ## Flag
//!
//! `QONTINUI_PROACTIVE_MIGRATION=off|observe|on`, default **off** — the ramp
//! convention of `context_watcher` (same [`Mode`] parser). `observe` logs the
//! would-fire once per terminal and acts on nothing.
//! `QONTINUI_PROACTIVE_MIGRATION_THRESHOLD_PCT` (1–100, default 95) sets the
//! utilization threshold.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use tracing::{debug, info};

use qontinui_runner_lib::agent_truth::{AgentState, Confidence, Verdict};

use crate::mcp::continuation_verdict::Mode;
use crate::terminal::agent_metrics::{self, FiveHourSample, HeadroomReading, HeadroomSource};

/// Tri-state ramp flag. `off` (default) | `observe` | `on`.
pub const FLAG_ENV: &str = "QONTINUI_PROACTIVE_MIGRATION";

/// Env override for the utilization threshold (1–100).
pub const THRESHOLD_ENV: &str = "QONTINUI_PROACTIVE_MIGRATION_THRESHOLD_PCT";

/// Default utilization threshold, percent.
pub const DEFAULT_THRESHOLD_PCT: f64 = 95.0;

/// The warning tier: a window projected to end at ≥ 80 % of its limit.
pub const WARNING_TIER: f64 = 0.8;

/// A projected exhaustion further out than this is not acted on yet.
pub const PROJECTION_HORIZON_MS: u64 = 30 * 60 * 1000;

/// After a fire, a terminal is not hinted again for this long — one probe per
/// ten minutes at most, the fleet usage loop's own cadence.
pub const HINT_COOLDOWN_MS: u64 = 10 * 60 * 1000;

const FIVE_HOUR_MS: u64 = 5 * 60 * 60 * 1000;

/// Two samples whose `resets_at` differ by less than this belong to the same
/// window (the provider's reset stamp jitters by sub-seconds between reads).
const SAME_WINDOW_TOLERANCE_MS: u64 = 5 * 60 * 1000;

/// A projection needs at least this much time between its first and last
/// sample.
const MIN_PROJECTION_SPAN_MS: u64 = 60_000;

/// The live mode from the process env.
pub fn mode() -> Mode {
    Mode::from_flag(std::env::var(FLAG_ENV).ok().as_deref())
}

/// Parse a threshold override; anything unusable falls back to the default.
/// Pure.
pub fn threshold_from(raw: Option<&str>) -> f64 {
    raw.and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|n| n.is_finite() && (1.0..=100.0).contains(n))
        .unwrap_or(DEFAULT_THRESHOLD_PCT)
}

pub(crate) fn threshold_pct() -> f64 {
    threshold_from(std::env::var(THRESHOLD_ENV).ok().as_deref())
}

// ---------------------------------------------------------------------------
// Burn-rate projection (pure)
// ---------------------------------------------------------------------------

/// How a window is projected to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Projected under [`WARNING_TIER`] at reset.
    Ok,
    /// Projected at ≥ [`WARNING_TIER`] of the limit at reset.
    Warning,
    /// Projected to exhaust before the reset.
    Critical,
}

/// A burn-rate projection over the current five-hour window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projection {
    /// Percentage points per hour; ≤ 0 means nothing is being burned.
    pub rate_pct_per_hour: f64,
    pub projected_pct_at_reset: f64,
    /// When the window hits 100 % at this rate (`None` at a non-positive
    /// rate).
    pub projected_exhaustion_at_ms: Option<u64>,
    pub resets_at_ms: u64,
    pub tier: Tier,
}

/// Project the current five-hour window from its samples. Uses only the
/// samples of the LATEST sample's window (same `resets_at`, observed inside
/// the five hours before it), from the first to the last; `None` with fewer
/// than two such samples a minute apart. Pure.
pub fn project(samples: &[FiveHourSample]) -> Option<Projection> {
    let last = *samples.iter().max_by_key(|s| s.at_ms)?;
    let window_start = last.resets_at_ms.saturating_sub(FIVE_HOUR_MS);
    let first = samples
        .iter()
        .filter(|s| {
            s.resets_at_ms.abs_diff(last.resets_at_ms) <= SAME_WINDOW_TOLERANCE_MS
                && s.at_ms >= window_start
                && s.at_ms <= last.at_ms
        })
        .min_by_key(|s| s.at_ms)?;
    let span_ms = last.at_ms.checked_sub(first.at_ms)?;
    if span_ms < MIN_PROJECTION_SPAN_MS {
        return None;
    }
    let hours = |ms: u64| ms as f64 / 3_600_000.0;
    let rate = (last.pct - first.pct) / hours(span_ms);
    let to_reset = hours(last.resets_at_ms.saturating_sub(last.at_ms));
    let projected_pct_at_reset = if rate > 0.0 {
        last.pct + rate * to_reset
    } else {
        last.pct
    };
    let projected_exhaustion_at_ms = if last.pct >= 100.0 {
        Some(last.at_ms)
    } else if rate > 0.0 {
        let ms = (100.0 - last.pct) / rate * 3_600_000.0;
        // Finite and positive here; a u64 cast saturates.
        Some(last.at_ms.saturating_add(ms as u64))
    } else {
        None
    };
    let tier = if projected_exhaustion_at_ms.is_some_and(|t| t < last.resets_at_ms) {
        Tier::Critical
    } else if projected_pct_at_reset / 100.0 >= WARNING_TIER {
        Tier::Warning
    } else {
        Tier::Ok
    };
    Some(Projection {
        rate_pct_per_hour: rate,
        projected_pct_at_reset,
        projected_exhaustion_at_ms,
        resets_at_ms: last.resets_at_ms,
        tier,
    })
}

/// When the current five-hour window runs dry at the observed burn rate.
/// Pure.
pub fn projected_exhaustion_at(samples: &[FiveHourSample]) -> Option<u64> {
    project(samples)?.projected_exhaustion_at_ms
}

// ---------------------------------------------------------------------------
// The trigger and the decision (pure)
// ---------------------------------------------------------------------------

/// Why a headroom hint fired.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HeadroomTrigger {
    Utilization {
        window: &'static str,
        pct: f64,
    },
    Projected {
        exhaustion_at_ms: u64,
        resets_at_ms: u64,
    },
}

impl HeadroomTrigger {
    pub fn label(&self) -> String {
        match self {
            Self::Utilization { window, pct } => format!("headroom:{window}={pct:.0}%"),
            Self::Projected {
                exhaustion_at_ms,
                resets_at_ms,
            } => format!(
                "headroom:projected-exhaustion {}m before reset",
                resets_at_ms.saturating_sub(*exhaustion_at_ms) / 60_000
            ),
        }
    }
}

/// Does this reading (plus projection) warrant a hint? Utilization at/over
/// the threshold wins; else a projected exhaustion that precedes the reset
/// and lands within the horizon. Pure.
pub fn trigger(
    reading: &HeadroomReading,
    projection: Option<&Projection>,
    threshold_pct: f64,
    now_ms: u64,
) -> Option<HeadroomTrigger> {
    if let Some(pct) = reading.five_hour_pct.filter(|p| *p >= threshold_pct) {
        return Some(HeadroomTrigger::Utilization {
            window: "five_hour",
            pct,
        });
    }
    if let Some(pct) = reading.seven_day_pct.filter(|p| *p >= threshold_pct) {
        return Some(HeadroomTrigger::Utilization {
            window: "seven_day",
            pct,
        });
    }
    let p = projection?;
    let at = p.projected_exhaustion_at_ms?;
    (at < p.resets_at_ms && at.saturating_sub(now_ms) <= PROJECTION_HORIZON_MS).then_some(
        HeadroomTrigger::Projected {
            exhaustion_at_ms: at,
            resets_at_ms: p.resets_at_ms,
        },
    )
}

/// The headroom-specific CONFIRM: does the OAuth PROBE itself agree?
///
/// `probe` must be the probe's own fresh reading (a cached-file reading is
/// refused, whatever it says); `probe_history` the probe's own five-hour
/// samples. Confirmed when the probe reading is at/over the threshold, or the
/// projection over the probe samples says the window runs dry before its
/// reset within the horizon — the same [`trigger`] rule, fed only what a
/// local file cannot forge. Pure.
pub fn probe_confirms(
    probe: Option<&HeadroomReading>,
    probe_history: &[FiveHourSample],
    threshold_pct: f64,
    now_ms: u64,
) -> bool {
    let Some(reading) = probe.filter(|r| r.source == HeadroomSource::OauthProbe) else {
        return false;
    };
    trigger(
        reading,
        project(probe_history).as_ref(),
        threshold_pct,
        now_ms,
    )
    .is_some()
}

/// What one evaluation should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadroomDecision {
    /// Flag off, no trigger, cooling down, or the session ended.
    Skip,
    /// Triggered mid-turn (or with no evidence of a boundary): wait for the
    /// next turn boundary.
    Defer,
    /// `observe`: log the would-fire, act on nothing.
    WouldFire,
    /// `on`: run the confirm path.
    Fire,
}

/// Is this verdict a positive turn boundary? `Some(true)` only for an
/// AUTHORITATIVE `TurnEnded`; `None` (skip) once the session ended; every
/// other verdict — `NeedsYou` of any reason, a screen-read or inferred
/// `TurnEnded`, `Failed`, `Working`, `Unknown`, `Starting` — defers.
fn at_turn_boundary(state: &AgentState, confidence: Option<Confidence>) -> Option<bool> {
    match state {
        AgentState::Ended { .. } => None,
        AgentState::TurnEnded => Some(confidence == Some(Confidence::Authoritative)),
        AgentState::NeedsYou { .. }
        | AgentState::Failed { .. }
        | AgentState::Working
        | AgentState::Unknown
        | AgentState::Starting => Some(false),
    }
}

/// The deterministic rule. Pure.
pub fn decide(
    mode: Mode,
    triggered: bool,
    verdict: &Verdict,
    last_fired_ms: Option<u64>,
    now_ms: u64,
) -> HeadroomDecision {
    if mode == Mode::Off || !triggered {
        return HeadroomDecision::Skip;
    }
    if last_fired_ms.is_some_and(|t| now_ms.saturating_sub(t) < HINT_COOLDOWN_MS) {
        return HeadroomDecision::Skip;
    }
    match at_turn_boundary(&verdict.state, verdict.confidence) {
        None => HeadroomDecision::Skip,
        Some(false) => HeadroomDecision::Defer,
        Some(true) => match mode {
            Mode::Observe => HeadroomDecision::WouldFire,
            Mode::On => HeadroomDecision::Fire,
            Mode::Off => HeadroomDecision::Skip,
        },
    }
}

// ---------------------------------------------------------------------------
// The per-terminal tick (impure glue)
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct TerminalHeadroom {
    last_fired_ms: Option<u64>,
    observe_logged: bool,
    deferred_logged: bool,
}

static TERMINALS: Mutex<Option<HashMap<String, TerminalHeadroom>>> = Mutex::new(None);

/// Drop state for terminals that are gone.
pub fn retain_live<'a>(live: impl Iterator<Item = &'a str>) {
    let live: HashSet<&str> = live.collect();
    let mut guard = TERMINALS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(map) = guard.as_mut() {
        map.retain(|tid, _| live.contains(tid.as_str()));
    }
}

/// Evaluate one pane. Rides the agent-state publish sweep; a dark flag is a
/// zero-cost early-out.
pub fn on_tick(
    terminal_id: &str,
    verdict: &Verdict,
    account: Option<&str>,
    reading: Option<&HeadroomReading>,
    now_ms: u64,
) {
    let mode = mode();
    if mode == Mode::Off {
        return;
    }
    let (Some(account), Some(reading)) = (account, reading) else {
        return;
    };
    let projection = project(&agent_metrics::five_hour_history(account));
    let threshold = threshold_pct();
    let fired = trigger(reading, projection.as_ref(), threshold, now_ms);

    let mut guard = TERMINALS.lock().unwrap_or_else(|e| e.into_inner());
    let entry = guard
        .get_or_insert_with(HashMap::new)
        .entry(terminal_id.to_string())
        .or_default();
    match decide(mode, fired.is_some(), verdict, entry.last_fired_ms, now_ms) {
        HeadroomDecision::Skip => {}
        HeadroomDecision::Defer => {
            if !entry.deferred_logged {
                entry.deferred_logged = true;
                debug!(
                    terminal = %terminal_id,
                    account,
                    "headroom: hint deferred to the next turn boundary"
                );
            }
        }
        HeadroomDecision::WouldFire => {
            if !entry.observe_logged {
                entry.observe_logged = true;
                info!(
                    terminal = %terminal_id,
                    account,
                    trigger = %fired.map(|t| t.label()).unwrap_or_default(),
                    tier = ?projection.map(|p| p.tier),
                    "headroom: WOULD FIRE (observe) — proactive migration hint"
                );
            }
        }
        HeadroomDecision::Fire => {
            entry.last_fired_ms = Some(now_ms);
            entry.deferred_logged = false;
            let label = fired.map(|t| t.label()).unwrap_or_default();
            info!(
                terminal = %terminal_id,
                account,
                trigger = %label,
                "headroom: proactive migration hint — confirming with the probe"
            );
            tauri::async_runtime::spawn(crate::terminal::account_migration::handle_headroom_hint(
                terminal_id.to_string(),
                label,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::agent_metrics::HeadroomSource;
    use qontinui_runner_lib::agent_truth::{EndReason, NeedsYouReason};

    const NOW: u64 = 1_900_000_000_000;
    const MIN: u64 = 60_000;
    const HOUR: u64 = 60 * MIN;

    fn sample(at_ms: u64, pct: f64, resets_at_ms: u64) -> FiveHourSample {
        FiveHourSample {
            at_ms,
            pct,
            resets_at_ms,
        }
    }

    fn reading(five: Option<f64>, seven: Option<f64>) -> HeadroomReading {
        HeadroomReading {
            five_hour_pct: five,
            five_hour_resets_at_ms: five.map(|_| NOW + 2 * HOUR),
            seven_day_pct: seven,
            seven_day_resets_at_ms: seven.map(|_| NOW + 48 * HOUR),
            source: HeadroomSource::CachedUsage,
            observed_at_ms: NOW,
        }
    }

    #[test]
    fn headroom_projection_linear_burn() {
        let reset = NOW + 2 * HOUR;
        // 20% → 50% over one hour: 30 pts/h, 50 left ⇒ dry in 100 min, before
        // the reset in 120 min.
        let s = [sample(NOW - HOUR, 20.0, reset), sample(NOW, 50.0, reset)];
        let p = project(&s).unwrap();
        assert!((p.rate_pct_per_hour - 30.0).abs() < 1e-9);
        assert_eq!(p.projected_exhaustion_at_ms, Some(NOW + 100 * MIN));
        assert_eq!(p.tier, Tier::Critical);
        assert_eq!(projected_exhaustion_at(&s), Some(NOW + 100 * MIN));
    }

    #[test]
    fn headroom_projection_tiers_and_flat_burn() {
        let reset = NOW + 2 * HOUR;
        // 10 pts/h from 60% with 2h left ⇒ 80% at reset: the warning tier.
        let warn = [sample(NOW - HOUR, 50.0, reset), sample(NOW, 60.0, reset)];
        let p = project(&warn).unwrap();
        assert_eq!(p.tier, Tier::Warning);
        assert!(p.projected_exhaustion_at_ms.unwrap() > reset);
        // Slow burn ⇒ Ok.
        let ok = [sample(NOW - HOUR, 10.0, reset), sample(NOW, 12.0, reset)];
        assert_eq!(project(&ok).unwrap().tier, Tier::Ok);
        // Flat or falling ⇒ no exhaustion time.
        let flat = [sample(NOW - HOUR, 30.0, reset), sample(NOW, 30.0, reset)];
        assert_eq!(projected_exhaustion_at(&flat), None);
    }

    #[test]
    fn headroom_projection_uses_only_the_current_window() {
        let old_reset = NOW - 10 * MIN;
        let reset = NOW + 4 * HOUR;
        // A 90% sample from the PREVIOUS window must not read as a fall.
        let s = [
            sample(NOW - 2 * HOUR, 90.0, old_reset),
            sample(NOW - 30 * MIN, 5.0, reset),
            sample(NOW, 25.0, reset),
        ];
        let p = project(&s).unwrap();
        assert!((p.rate_pct_per_hour - 40.0).abs() < 1e-9);
        // One sample, or two too close together, projects nothing.
        assert!(project(&[sample(NOW, 25.0, reset)]).is_none());
        assert!(project(&[sample(NOW - 1000, 20.0, reset), sample(NOW, 25.0, reset)]).is_none());
        assert!(project(&[]).is_none());
    }

    #[test]
    fn headroom_trigger_threshold_and_horizon() {
        assert_eq!(
            trigger(&reading(Some(96.0), None), None, 95.0, NOW),
            Some(HeadroomTrigger::Utilization {
                window: "five_hour",
                pct: 96.0
            })
        );
        assert!(matches!(
            trigger(&reading(Some(10.0), Some(99.0)), None, 95.0, NOW),
            Some(HeadroomTrigger::Utilization {
                window: "seven_day",
                ..
            })
        ));
        assert_eq!(
            trigger(&reading(Some(50.0), Some(50.0)), None, 95.0, NOW),
            None
        );
        // Unknown fields never trigger.
        assert_eq!(trigger(&reading(None, None), None, 95.0, NOW), None);
        // A projection inside the horizon and before the reset triggers …
        let reset = NOW + 2 * HOUR;
        let near = project(&[sample(NOW - HOUR, 20.0, reset), sample(NOW, 80.0, reset)]).unwrap();
        assert!(matches!(
            trigger(&reading(Some(80.0), None), Some(&near), 95.0, NOW),
            Some(HeadroomTrigger::Projected { .. })
        ));
        // … one beyond the horizon does not yet.
        let far = project(&[sample(NOW - HOUR, 20.0, reset), sample(NOW, 50.0, reset)]).unwrap();
        assert_eq!(
            trigger(&reading(Some(50.0), None), Some(&far), 95.0, NOW),
            None
        );
    }

    /// A verdict in `state` at `confidence`.
    fn v(state: AgentState, confidence: Confidence) -> Verdict {
        Verdict {
            state,
            source: None,
            since_ms: Some(NOW),
            confidence: Some(confidence),
            disagreement: None,
        }
    }

    /// The one turn boundary: an authoritative `TurnEnded`.
    fn ended_turn() -> Verdict {
        v(AgentState::TurnEnded, Confidence::Authoritative)
    }

    #[test]
    fn headroom_default_off_does_nothing() {
        assert_eq!(Mode::from_flag(None), Mode::Off, "the ramp default");
        for state in [
            AgentState::TurnEnded,
            AgentState::Working,
            AgentState::Unknown,
        ] {
            assert_eq!(
                decide(
                    Mode::Off,
                    true,
                    &v(state, Confidence::Authoritative),
                    None,
                    NOW
                ),
                HeadroomDecision::Skip
            );
        }
        // The live tick with the flag unset (the harness never sets it) is a
        // no-op: no state is recorded for the terminal.
        let tid = format!("term-{}", uuid::Uuid::new_v4());
        on_tick(
            &tid,
            &ended_turn(),
            Some("/test/headroom/off"),
            Some(&reading(Some(100.0), Some(100.0))),
            NOW,
        );
        let guard = TERMINALS.lock().unwrap();
        assert!(guard.as_ref().is_none_or(|m| !m.contains_key(&tid)));
    }

    #[test]
    fn headroom_hint_during_working_defers_to_the_next_turn_end() {
        let turn = [
            v(AgentState::Working, Confidence::Authoritative),
            v(AgentState::Working, Confidence::Authoritative),
            ended_turn(),
        ];
        let decisions: Vec<_> = turn
            .iter()
            .map(|s| decide(Mode::On, true, s, None, NOW))
            .collect();
        assert_eq!(
            decisions,
            vec![
                HeadroomDecision::Defer,
                HeadroomDecision::Defer,
                HeadroomDecision::Fire
            ]
        );
        // No evidence of a boundary is not a boundary.
        assert_eq!(
            decide(Mode::On, true, &Verdict::UNKNOWN, None, NOW),
            HeadroomDecision::Defer
        );
        assert_eq!(
            decide(
                Mode::On,
                true,
                &v(
                    AgentState::Ended {
                        why: EndReason::Other
                    },
                    Confidence::Authoritative
                ),
                None,
                NOW
            ),
            HeadroomDecision::Skip
        );
        // Observe never acts.
        assert_eq!(
            decide(Mode::Observe, true, &ended_turn(), None, NOW),
            HeadroomDecision::WouldFire
        );
    }

    /// M6: only an AUTHORITATIVE `TurnEnded` is a boundary. A `NeedsYou` of
    /// any reason is a paused turn; a screen-read or inferred `TurnEnded` is
    /// not evidence the turn ended.
    #[test]
    fn headroom_turn_boundary_is_an_authoritative_turn_end_only() {
        for reason in [
            NeedsYouReason::Permission,
            NeedsYouReason::Question,
            NeedsYouReason::Elicitation,
            NeedsYouReason::IdlePrompt,
            NeedsYouReason::Unspecified,
        ] {
            assert_eq!(
                decide(
                    Mode::On,
                    true,
                    &v(AgentState::NeedsYou { reason }, Confidence::Authoritative),
                    None,
                    NOW
                ),
                HeadroomDecision::Defer,
                "{reason:?}"
            );
        }
        for conf in [Confidence::Fallback, Confidence::Inferred] {
            assert_eq!(
                decide(Mode::On, true, &v(AgentState::TurnEnded, conf), None, NOW),
                HeadroomDecision::Defer,
                "{conf:?}"
            );
        }
        let mut no_confidence = ended_turn();
        no_confidence.confidence = None;
        assert_eq!(
            decide(Mode::On, true, &no_confidence, None, NOW),
            HeadroomDecision::Defer
        );
        assert_eq!(
            decide(Mode::On, true, &ended_turn(), None, NOW),
            HeadroomDecision::Fire
        );
    }

    #[test]
    fn headroom_cooldown_bounds_the_probe_rate() {
        assert_eq!(
            decide(Mode::On, true, &ended_turn(), Some(NOW - MIN), NOW),
            HeadroomDecision::Skip
        );
        assert_eq!(
            decide(
                Mode::On,
                true,
                &ended_turn(),
                Some(NOW - HINT_COOLDOWN_MS),
                NOW
            ),
            HeadroomDecision::Fire
        );
    }

    fn probe_reading(five: Option<f64>, seven: Option<f64>) -> HeadroomReading {
        HeadroomReading {
            source: HeadroomSource::OauthProbe,
            ..reading(five, seven)
        }
    }

    /// M5: a forged 100 % hint (the cached file) with a healthy probe confirms
    /// nothing; the probe itself at/over the threshold confirms.
    #[test]
    fn headroom_forged_full_reading_with_healthy_probe_migrates_nothing() {
        // A forged 100% reading does trigger a HINT …
        let forged = reading(Some(100.0), Some(100.0));
        let t = trigger(&forged, None, DEFAULT_THRESHOLD_PCT, NOW);
        assert!(t.is_some());
        assert_eq!(
            decide(Mode::On, t.is_some(), &ended_turn(), None, NOW),
            HeadroomDecision::Fire
        );
        // … but the headroom confirm reads only the PROBE, which says healthy.
        let healthy = probe_reading(Some(20.0), Some(30.0));
        assert!(!probe_confirms(
            Some(&healthy),
            &[],
            DEFAULT_THRESHOLD_PCT,
            NOW
        ));
        // The forged reading itself is refused as a confirm, whatever it says.
        assert!(!probe_confirms(
            Some(&forged),
            &[],
            DEFAULT_THRESHOLD_PCT,
            NOW
        ));
        assert!(!probe_confirms(None, &[], DEFAULT_THRESHOLD_PCT, NOW));
        // The reactive confirm is unchanged: a healthy probe is not exhausted.
        let dir = format!("/test/headroom/forged-{}", uuid::Uuid::new_v4());
        crate::ai_provider::record_account_usage(&[(
            dir.clone(),
            0.2,
            Some(-0.1),
            Some(0.3),
            false,
        )]);
        assert_eq!(
            crate::terminal::account_migration::confirm_step(&dir),
            crate::terminal::account_migration::ConfirmStep::NotConfirmed
        );
    }

    #[test]
    fn headroom_probe_at_threshold_or_projecting_exhaustion_confirms() {
        // The probe at/over the threshold confirms (either window) …
        assert!(probe_confirms(
            Some(&probe_reading(Some(96.0), Some(40.0))),
            &[],
            DEFAULT_THRESHOLD_PCT,
            NOW
        ));
        assert!(probe_confirms(
            Some(&probe_reading(Some(10.0), Some(97.0))),
            &[],
            DEFAULT_THRESHOLD_PCT,
            NOW
        ));
        // … as does the probe's own projection running dry within the horizon
        // and before the reset, though the reading itself is under threshold.
        let reset = NOW + 2 * HOUR;
        let probe_samples = [sample(NOW - HOUR, 20.0, reset), sample(NOW, 80.0, reset)];
        assert!(probe_confirms(
            Some(&probe_reading(Some(80.0), None)),
            &probe_samples,
            DEFAULT_THRESHOLD_PCT,
            NOW
        ));
        // A slow burn does not.
        let slow = [sample(NOW - HOUR, 20.0, reset), sample(NOW, 25.0, reset)];
        assert!(!probe_confirms(
            Some(&probe_reading(Some(25.0), None)),
            &slow,
            DEFAULT_THRESHOLD_PCT,
            NOW
        ));
    }

    #[test]
    fn headroom_threshold_parse() {
        assert_eq!(threshold_from(None), DEFAULT_THRESHOLD_PCT);
        assert_eq!(threshold_from(Some("garbage")), DEFAULT_THRESHOLD_PCT);
        assert_eq!(threshold_from(Some("0")), DEFAULT_THRESHOLD_PCT);
        assert_eq!(threshold_from(Some("101")), DEFAULT_THRESHOLD_PCT);
        assert_eq!(threshold_from(Some(" 90 ")), 90.0);
    }
}
