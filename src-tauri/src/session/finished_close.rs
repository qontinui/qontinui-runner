//! The wind-down executor's `finished_close` arm — its switch, its cadence,
//! which sessions it may consider, and the custody verdicts it last reached
//! (plan `2026-10-03-finished-runner-sessions-close-their-window-without-a-drain`,
//! D1–D3).
//!
//! ## What the arm is
//!
//! The drain arm closes finished, idle sessions only while coord holds this
//! device drained. This arm closes a finished, idle TERMINAL session on any
//! tick, drained or not, once `wind_down::eligibility` rates it `Eligible`
//! AND `wind_down::custody_gate` says every worktree it touched is clean and
//! pushed. Looping agents and stewards are never its candidates: their
//! supervisors respawn them, so closing one outside a drain is churn, not
//! tidying (D1).
//!
//! The close itself still happens in `wind_down_executor`, through the one
//! `graceful_exit` call that file's kill-path tripwire pins. Everything here
//! is either pure or a small read; nothing here reaches a pane.
//!
//! ## The switch (D3)
//!
//! Settings → General → Sessions → "Close finished sessions after grace":
//! `on` (the default) / `shadow` / `off`, re-read on every tick.
//! [`KILL_ENV`]`=0` in the runner's spawn environment is a machine kill switch
//! that wins over the setting, and `QONTINUI_WIND_DOWN_EXECUTOR=0` still
//! disables the whole executor. `shadow` runs the arm in full — census,
//! custody probes and the per-close re-checks — and logs "would close" where
//! `on` would close.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::session::tracking_health::LiveClaudeProcess;
use crate::session::wind_down_executor::{select_candidates, ClockVerdict, TICK};
use crate::session::wind_down_observer::ObservedInputs;
use crate::settings::FinishedSessionClose;
use qontinui_runner_lib::wind_down::{CustodyVerdict, CustodyView, SessionKind};

/// Machine kill switch: exactly `0` in the runner's spawn environment turns
/// the arm off whatever the setting says. Absent, `1`, or anything else defers
/// to the setting.
pub const KILL_ENV: &str = "QONTINUI_FINISHED_SESSION_CLOSE";

/// PURE: the mode the arm runs in, from the saved setting and the raw
/// [`KILL_ENV`] value. The env can only turn the arm OFF — it never turns on
/// an arm the operator switched off, and never promotes `shadow` to `on`.
pub fn effective_mode(setting: FinishedSessionClose, env: Option<&str>) -> FinishedSessionClose {
    if env.map(str::trim) == Some("0") {
        FinishedSessionClose::Off
    } else {
        setting
    }
}

/// Is the machine kill switch engaged? See [`KILL_ENV`].
pub fn kill_switch_engaged() -> bool {
    effective_mode(
        FinishedSessionClose::On,
        std::env::var(KILL_ENV).ok().as_deref(),
    ) == FinishedSessionClose::Off
}

/// PURE: [`effective_mode`] over a saved value that may not have been
/// readable. An unreadable `settings.json` is OFF for this arm: the arm closes
/// the user's windows, and a damaged file cannot tell us they had not
/// switched it off — its placeholder default (`on`) is not their answer.
pub fn mode_from(
    saved: &Result<FinishedSessionClose, String>,
    env: Option<&str>,
) -> FinishedSessionClose {
    match saved {
        Ok(setting) => effective_mode(*setting, env),
        Err(_) => FinishedSessionClose::Off,
    }
}

/// Whether the last [`current_mode`] read found `settings.json` unreadable —
/// kept so the warning is logged on the change, not on every 30 s tick.
static SETTINGS_UNREADABLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The mode the arm runs in right now: the saved setting, overridden by the
/// machine kill switch, and OFF while `settings.json` is unreadable.
pub fn current_mode() -> FinishedSessionClose {
    let saved = crate::settings::get_finished_session_close();
    let unreadable = saved.is_err();
    if SETTINGS_UNREADABLE.swap(unreadable, std::sync::atomic::Ordering::Relaxed) != unreadable {
        match &saved {
            Err(e) => tracing::warn!(
                error = %e,
                "finished_close: settings.json is unreadable, so the saved switch cannot be \
                 known — the arm is OFF until it reads again (a damaged file must not turn a \
                 saved `off` back on)"
            ),
            Ok(_) => tracing::info!("finished_close: settings.json is readable again"),
        }
    }
    mode_from(&saved, std::env::var(KILL_ENV).ok().as_deref())
}

/// PURE: how often the arm runs a census — `max(TICK, grace / 4)`.
///
/// The drain arm pays a process-table census plus a coord read every 30 s,
/// but only while drained, which is rare. This arm would pay it on every tick
/// of every runner. A session needs a full grace period of continuous
/// idleness before it is eligible at all, so looking four times per grace
/// period closes it at most a quarter-grace late and costs a quarter of the
/// censuses. A grace shorter than four ticks keeps the tick.
pub fn census_interval(grace: Duration) -> Duration {
    TICK.max(grace / 4)
}

/// Why a `finished_close` tick did not run a census.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The switch (or the machine kill switch) is off.
    Off,
    /// The wall clock stepped against the monotonic clock, or its quarantine
    /// is still running (the executor's hazard 1 — shared with the drain arm).
    ClockUntrusted,
    /// The arm's last census was less than [`census_interval`] ago.
    NotDue,
    /// The lifecycle store holds no open terminal record, so there is nothing
    /// a census could find for this arm.
    NoOpenTerminalRecord,
}

/// What a `finished_close` tick should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassPlan {
    Skip(SkipReason),
    /// Run the census; `shadow` means log instead of closing.
    Census {
        shadow: bool,
    },
}

/// PURE: plan one `finished_close` tick. Checked cheapest first, and the
/// census — the expensive part — only when every check passes.
pub fn plan_pass(
    mode: FinishedSessionClose,
    clock: ClockVerdict,
    last_census: Option<Instant>,
    now: Instant,
    grace: Duration,
    open_terminal_records: usize,
) -> PassPlan {
    if mode == FinishedSessionClose::Off {
        return PassPlan::Skip(SkipReason::Off);
    }
    if !clock.trustworthy() {
        return PassPlan::Skip(SkipReason::ClockUntrusted);
    }
    if last_census.is_some_and(|last| now.saturating_duration_since(last) < census_interval(grace))
    {
        return PassPlan::Skip(SkipReason::NotDue);
    }
    if open_terminal_records == 0 {
        return PassPlan::Skip(SkipReason::NoOpenTerminalRecord);
    }
    PassPlan::Census {
        shadow: mode == FinishedSessionClose::Shadow,
    }
}

/// PURE: the `(claude_session_id, terminal_id)` pairs this arm may consider —
/// [`select_candidates`]' `Eligible`, top-level, one-per-pane set, narrowed to
/// [`SessionKind::Terminal`]. Looping agents and stewards stay drain-only.
pub fn select_finished_close_candidates(
    processes: &[LiveClaudeProcess],
    observed: &ObservedInputs,
) -> Vec<(String, String)> {
    select_candidates(processes, observed)
        .into_iter()
        .filter(|(_, terminal_id)| observed.kind_for(terminal_id) == SessionKind::Terminal)
        .collect()
}

// ---------------------------------------------------------------------------
// The last custody verdicts, for `GET /restart-readiness` `windDown.custody`
// ---------------------------------------------------------------------------

/// `claude_session_id` → the custody view the arm's most recent pass reached.
fn last_custody_map() -> &'static Mutex<HashMap<String, CustodyView>> {
    static MAP: OnceLock<Mutex<HashMap<String, CustodyView>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Replace the published verdicts with one pass's. REPLACED, not merged: a
/// session the latest pass did not judge (it is no longer `Eligible`, or it
/// closed) must stop showing a verdict that has gone stale.
pub fn publish_custody(verdicts: &[(String, CustodyVerdict)], judged_at_ms: i64) {
    let next: HashMap<String, CustodyView> = verdicts
        .iter()
        .map(|(session_id, verdict)| {
            (
                session_id.clone(),
                CustodyView::from_verdict(verdict, judged_at_ms),
            )
        })
        .collect();
    let mut map = match last_custody_map().lock() {
        Ok(m) => m,
        Err(e) => e.into_inner(),
    };
    *map = next;
}

/// The arm's last custody view for a session, if its latest pass judged it.
pub fn last_custody(claude_session_id: &str) -> Option<CustodyView> {
    let map = match last_custody_map().lock() {
        Ok(m) => m,
        Err(e) => e.into_inner(),
    };
    map.get(claude_session_id).cloned()
}

/// Attach [`last_custody`] to every top-level process carrying a `windDown`
/// view — the readiness report's `windDown.custody`.
pub fn attach_last_custody(processes: &mut [LiveClaudeProcess]) {
    for process in processes.iter_mut() {
        let Some(session_id) = process.session_id.clone() else {
            continue;
        };
        if let Some(view) = process.wind_down.as_mut() {
            view.custody = last_custody(&session_id);
        }
    }
}

/// One-line rendering of a verdict for the arm's logs.
pub fn describe(verdict: &CustodyVerdict) -> String {
    match verdict.detail() {
        Some(detail) => format!(
            "{} ({}): {detail}",
            verdict.as_str(),
            verdict.reason().unwrap_or("-")
        ),
        None => verdict.as_str().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_runner_lib::wind_down::{CustodyDirty, CustodyUnknown};

    #[test]
    fn the_env_kill_switch_wins_over_the_setting_and_never_widens_it() {
        use FinishedSessionClose::*;
        for setting in [On, Shadow, Off] {
            assert_eq!(effective_mode(setting, Some("0")), Off, "{setting:?}");
            assert_eq!(effective_mode(setting, Some(" 0 ")), Off, "{setting:?}");
            for env in [None, Some("1"), Some(""), Some("yes")] {
                assert_eq!(effective_mode(setting, env), setting, "{setting:?} {env:?}");
            }
        }
    }

    /// Review item 4: an unreadable `settings.json` must not turn a saved
    /// `off` back into the placeholder default `on`.
    #[test]
    fn an_unreadable_settings_file_is_off_for_this_arm() {
        let unreadable: Result<FinishedSessionClose, String> =
            Err("expected value at line 1 column 1".to_string());
        assert_eq!(mode_from(&unreadable, None), FinishedSessionClose::Off);
        assert_eq!(mode_from(&unreadable, Some("1")), FinishedSessionClose::Off);
        for setting in [
            FinishedSessionClose::On,
            FinishedSessionClose::Shadow,
            FinishedSessionClose::Off,
        ] {
            assert_eq!(mode_from(&Ok(setting), None), setting);
            assert_eq!(
                mode_from(&Ok(setting), Some("0")),
                FinishedSessionClose::Off
            );
        }
    }

    #[test]
    fn the_census_interval_is_a_quarter_grace_never_below_a_tick() {
        assert_eq!(
            census_interval(Duration::from_secs(600)),
            Duration::from_secs(150)
        );
        assert_eq!(census_interval(Duration::from_secs(60)), TICK);
        assert_eq!(census_interval(Duration::from_secs(1)), TICK);
    }

    const GRACE: Duration = Duration::from_secs(600);

    #[test]
    fn off_and_the_kill_switch_skip_the_pass() {
        let now = Instant::now();
        assert_eq!(
            plan_pass(
                FinishedSessionClose::Off,
                ClockVerdict::Trustworthy,
                None,
                now,
                GRACE,
                3
            ),
            PassPlan::Skip(SkipReason::Off)
        );
        assert_eq!(
            plan_pass(
                effective_mode(FinishedSessionClose::On, Some("0")),
                ClockVerdict::Trustworthy,
                None,
                now,
                GRACE,
                3
            ),
            PassPlan::Skip(SkipReason::Off)
        );
    }

    #[test]
    fn an_untrustworthy_clock_skips_the_pass() {
        let now = Instant::now();
        for clock in [
            ClockVerdict::Jumped { skew_ms: 9_000 },
            ClockVerdict::Quarantined,
        ] {
            assert_eq!(
                plan_pass(FinishedSessionClose::On, clock, None, now, GRACE, 3),
                PassPlan::Skip(SkipReason::ClockUntrusted),
                "{clock:?}"
            );
        }
    }

    /// The cost bound: a tick with no open terminal record runs NO census.
    #[test]
    fn a_tick_with_no_open_terminal_record_does_no_census() {
        assert_eq!(
            plan_pass(
                FinishedSessionClose::On,
                ClockVerdict::Trustworthy,
                None,
                Instant::now(),
                GRACE,
                0
            ),
            PassPlan::Skip(SkipReason::NoOpenTerminalRecord)
        );
    }

    #[test]
    fn the_census_runs_at_most_once_per_interval() {
        let t0 = Instant::now();
        let plan = |now: Instant| {
            plan_pass(
                FinishedSessionClose::On,
                ClockVerdict::Trustworthy,
                Some(t0),
                now,
                GRACE,
                2,
            )
        };
        assert_eq!(plan(t0 + TICK), PassPlan::Skip(SkipReason::NotDue));
        assert_eq!(
            plan(t0 + census_interval(GRACE)),
            PassPlan::Census { shadow: false }
        );
    }

    #[test]
    fn shadow_runs_the_census_in_shadow() {
        assert_eq!(
            plan_pass(
                FinishedSessionClose::Shadow,
                ClockVerdict::Trustworthy,
                None,
                Instant::now(),
                GRACE,
                1
            ),
            PassPlan::Census { shadow: true }
        );
    }

    #[test]
    fn published_custody_is_replaced_per_pass_and_attached_to_its_session() {
        publish_custody(
            &[
                ("fc-a".to_string(), CustodyVerdict::Clean),
                (
                    "fc-b".to_string(),
                    CustodyVerdict::Dirty(CustodyDirty::Uncommitted {
                        path: "/ws/b".to_string(),
                    }),
                ),
            ],
            7,
        );
        assert_eq!(last_custody("fc-a").unwrap().verdict, "clean");
        assert_eq!(last_custody("fc-b").unwrap().reason, Some("uncommitted"));

        publish_custody(
            &[(
                "fc-b".to_string(),
                CustodyVerdict::Unknown(CustodyUnknown::NoSessionDir),
            )],
            8,
        );
        assert!(
            last_custody("fc-a").is_none(),
            "a session the latest pass did not judge carries no verdict"
        );
        let b = last_custody("fc-b").unwrap();
        assert_eq!((b.verdict, b.judged_at), ("unknown", 8));

        // ...and `/restart-readiness` attaches exactly that to `windDown`.
        // Kept in THIS test: the published map is process-global, and a
        // second test publishing beside it would race it.
        let process = |sid: &str| LiveClaudeProcess {
            pid: 1,
            parent_pid: None,
            image: Some("claude".to_string()),
            age_s: Some(60),
            cwd: None,
            has_live_children: Some(false),
            nested_under_claude: false,
            session_id: Some(sid.to_string()),
            session_status: Some("finished".to_string()),
            blocks_restart: false,
            wind_down: Some(qontinui_runner_lib::wind_down::WindDownView::from_verdict(
                &qontinui_runner_lib::wind_down::Eligibility::Eligible { since_ms: 1 },
                SessionKind::Terminal,
            )),
        };
        let mut processes = vec![process("fc-a"), process("fc-b")];
        attach_last_custody(&mut processes);
        let json = serde_json::to_value(&processes).unwrap();
        assert!(
            json[0]["windDown"].get("custody").is_none(),
            "not judged by the latest pass → no custody key, never `clean`"
        );
        assert_eq!(json[1]["windDown"]["custody"]["verdict"], "unknown");
        assert_eq!(json[1]["windDown"]["custody"]["reason"], "no_session_dir");
    }
}
