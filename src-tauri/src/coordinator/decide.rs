//! Pure rule functions A–E for the Coordinator's cheap decision path.
//!
//! Faithful port of `qontinui-claude-config/.claude/commands/coordinate.md`
//! lines 43–78. Each rule is a `pub(crate) fn rule_<x>(obs, last_decisions)
//! -> Option<DecideOutcome>`. No I/O, no async — `decide` is a pure
//! function over the observation snapshot. The scheduler calls them in
//! order (A → E) and stops at the first one that fires per iteration
//! (per Phase 1b spec: "one action per rule per iteration").
//!
//! Rule version is encoded into the `reasoning` field's prefix
//! (`[rule=B v=2026-05-02] …`) — see the plan §Risks for why this beats
//! a new schema column.
//!
//! ## Idempotency notes
//!
//! - Rule A: skip the advisory if a same-target advisory is in the last
//!   5 decisions (avoids spam every tick).
//! - Rule B: skip if the same `(task_id, session_id)` already had an
//!   `assign-task` row in the recent window for the same iteration —
//!   prevents reissue while the worker is mid-startup.
//! - Rule D: only act on review rows newer than the latest
//!   `coordinator_decisions.created_at` for the same task.

use crate::claude_session::state::SessionState;
use crate::database::pg::coordinator_decisions::CoordinatorDecisionRow;
use crate::database::pg::reviews::ReviewRow;
use crate::database::pg::tasks::TaskRow;
use crate::executor::file_registry::normalize_path;
use crate::mcp::coordinator::CoordinatorAction;

use super::observe::{LiveSession, Observation};

pub(crate) const RULE_VERSION: &str = "2026-05-02";

/// Result of a fired rule. `rule` is the single-letter tag the scheduler
/// stamps on the persisted decision row; `action` is the wire-shaped
/// `CoordinatorAction` that gets handed to `coordinator::act::apply`.
#[derive(Debug, Clone)]
pub(crate) struct DecideOutcome {
    pub rule: &'static str,
    pub action: CoordinatorAction,
}

/// Wrap a body string with the `[rule=X v=YYYY-MM-DD]` prefix the plan
/// agreed on (Risks §Rule fidelity drift). Dashboard renders `reasoning`
/// verbatim, so this is the version surface.
pub(crate) fn stamp_reasoning(rule: &'static str, body: &str) -> String {
    format!("[rule={} v={}] {}", rule, RULE_VERSION, body)
}

// =============================================================================
// In-memory scheduler state — Rule A's stale-heartbeat sweep
// =============================================================================

/// Per-tick `SessionState` observation. The scheduler keeps a
/// `HashMap<task_run_id, ProcessingSeenSince>` across ticks so Rule A can
/// detect "stuck in Processing > 5 min" without touching PG.
///
/// Lives in `decide` rather than `scheduler` because the unit tests for
/// Rule A construct one directly.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProcessingSeenSince {
    pub since_ms: i64,
}

/// Look up whether `session` should be considered stuck right now.
/// `now_ms` and `tracker` are passed in (rather than read from
/// `SystemTime::now`) so the unit tests can pin time deterministically.
fn is_stuck(
    session: &LiveSession,
    tracker: &std::collections::HashMap<String, ProcessingSeenSince>,
    now_ms: i64,
    threshold_ms: i64,
) -> bool {
    if session.state != Some(SessionState::Processing) {
        return false;
    }
    match tracker.get(&session.task_run_id) {
        Some(seen) => now_ms.saturating_sub(seen.since_ms) >= threshold_ms,
        None => false,
    }
}

// =============================================================================
// Rule A — stale-heartbeat sweep
// =============================================================================

/// Coordinator session id used inside the same tick to dedupe advisories.
/// Phase 1b's `recent_decisions` window is the dedup surface; the rule
/// itself only reads it.
const STALE_HEARTBEAT_THRESHOLD_MS: i64 = 5 * 60 * 1000;

pub(crate) fn rule_a(
    obs: &Observation,
    tracker: &std::collections::HashMap<String, ProcessingSeenSince>,
    now_ms: i64,
) -> Option<DecideOutcome> {
    for ls in &obs.live_sessions {
        if !is_stuck(ls, tracker, now_ms, STALE_HEARTBEAT_THRESHOLD_MS) {
            continue;
        }

        // Idempotency: skip if a same-target Rule-A advisory exists in
        // the last 5 decisions. Keeps the audit log readable.
        let already_advised = obs
            .recent_decisions
            .iter()
            .take(5)
            .any(|d| d.rule == "A" && d.target_id.as_deref() == Some(ls.task_run_id.as_str()));
        if already_advised {
            continue;
        }

        let body = format!(
            "session {} may be stuck (state=Processing > 5 min); user judgement requested before any kill",
            ls.task_run_id
        );
        return Some(DecideOutcome {
            rule: "A",
            action: CoordinatorAction::AdviseWithText {
                target_id: Some(ls.task_run_id.clone()),
                reasoning: stamp_reasoning("A", &body),
            },
        });
    }
    None
}

// =============================================================================
// Rule B — ready-task assignment
// =============================================================================

/// True if `idle_session` is acceptable for `task` in the file-overlap
/// sense from `coordinate.md` Rule B: the session's last claimed paths
/// must not overlap the task's `expected_file_claims`.
fn paths_overlap(claimed: &[String], expected: &[String]) -> bool {
    if claimed.is_empty() || expected.is_empty() {
        return false;
    }
    let claimed_set: std::collections::HashSet<String> =
        claimed.iter().map(|p| normalize_path(p)).collect();
    expected
        .iter()
        .map(|p| normalize_path(p))
        .any(|p| claimed_set.contains(&p))
}

/// True if `session` is idle for Rule-B purposes:
/// - `state == Ready`
/// - not assigned to any non-terminal task
fn is_idle(session: &LiveSession) -> bool {
    session.state == Some(SessionState::Ready) && session.assigned_task_id.is_none()
}

pub(crate) fn rule_b(obs: &Observation) -> Option<DecideOutcome> {
    // Phase 1b ports the simplified version: tab-title-prefix matching
    // doesn't have a Rust-side surface (sessions are keyed by
    // task_run_id, not tab title). The phase-name match is therefore
    // skipped here. Documented in the report; revisit if Phase 2 wants
    // tighter routing.

    for task in &obs.tasks {
        if task.status != "ready" {
            continue;
        }

        // Idempotency: was this task already assigned in the recent
        // decisions window? If so, skip — the worker is mid-startup.
        let recent_assign = obs.recent_decisions.iter().take(20).any(|d| {
            d.rule == "B"
                && d.action == "assign-task"
                && d.target_id.as_deref() == Some(task.id.as_str())
        });
        if recent_assign {
            continue;
        }

        // Idle sessions where last touched paths don't overlap the task's
        // expected_file_claims.
        let mut candidates: Vec<&LiveSession> = obs
            .live_sessions
            .iter()
            .filter(|ls| is_idle(ls))
            .filter(|ls| {
                let touched = obs
                    .session_touched_files
                    .get(&ls.task_run_id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                !paths_overlap(touched, &task.expected_file_claims)
            })
            .collect();

        match candidates.len() {
            0 => {
                let body = format!(
                    "no suitable idle session for ready task {} (phase={}); user attention requested",
                    task.id, task.phase_name
                );
                return Some(DecideOutcome {
                    rule: "B",
                    action: CoordinatorAction::AdviseWithText {
                        target_id: Some(task.id.clone()),
                        reasoning: stamp_reasoning("B", &body),
                    },
                });
            }
            1 => {
                let session = candidates[0];
                let body = format!(
                    "assigning ready task {} to idle session {} (phase={})",
                    task.id, session.task_run_id, task.phase_name
                );
                return Some(DecideOutcome {
                    rule: "B",
                    action: CoordinatorAction::AssignTask {
                        task_id: task.id.clone(),
                        session_id: session.task_run_id.clone(),
                        reasoning: Some(stamp_reasoning("B", &body)),
                    },
                });
            }
            _ => {
                // Multi-match → least-recently-busy. With no per-session
                // last-busy timestamp on `LiveSession`, fall back to
                // "smallest touched-files set" (proxy for "least
                // recently busy" — fewer touched files implies the
                // session has been idle longer or just started).
                candidates.sort_by_key(|ls| {
                    obs.session_touched_files
                        .get(&ls.task_run_id)
                        .map(|v| v.len())
                        .unwrap_or(0)
                });
                let session = candidates[0];
                let body = format!(
                    "assigning ready task {} to idle session {} (least-recently-busy of {} candidates; phase={})",
                    task.id,
                    session.task_run_id,
                    candidates.len(),
                    task.phase_name
                );
                return Some(DecideOutcome {
                    rule: "B",
                    action: CoordinatorAction::AssignTask {
                        task_id: task.id.clone(),
                        session_id: session.task_run_id.clone(),
                        reasoning: Some(stamp_reasoning("B", &body)),
                    },
                });
            }
        }
    }
    None
}

// =============================================================================
// Rule C — upcoming-claim conflict
// =============================================================================

pub(crate) fn rule_c(obs: &Observation) -> Option<DecideOutcome> {
    // Build map: normalized upcoming-claim path → (plan_id, task_id) for
    // tasks whose own session hasn't started (status=pending|ready).
    use std::collections::HashSet;
    let pending_or_ready: HashSet<&str> = obs
        .tasks
        .iter()
        .filter(|t| matches!(t.status.as_str(), "pending" | "ready"))
        .map(|t| t.id.as_str())
        .collect();

    let upcoming_paths: std::collections::HashMap<String, UpcomingClaimRef> = obs
        .upcoming_file_registry
        .iter()
        .filter(|c| pending_or_ready.contains(c.task_id.as_str()))
        .map(|c| {
            (
                normalize_path(&c.path),
                UpcomingClaimRef {
                    plan_id: &c.plan_id,
                    task_id: &c.task_id,
                    path: &c.path,
                },
            )
        })
        .collect();

    if upcoming_paths.is_empty() {
        return None;
    }

    // Find a running session whose touched files include any upcoming-claim
    // path that belongs to a different task.
    for ls in &obs.live_sessions {
        let session_task = ls.assigned_task_id.as_deref();

        let touched = match obs.session_touched_files.get(&ls.task_run_id) {
            Some(v) => v,
            None => continue,
        };

        for raw in touched {
            let p = normalize_path(raw);
            if let Some(claim) = upcoming_paths.get(&p) {
                // Skip if the conflict is the session's own task.
                if session_task == Some(claim.task_id) {
                    continue;
                }

                // Idempotency: skip if a Rule-C advisory was filed for
                // (session, upcoming-task) recently.
                let already = obs.recent_decisions.iter().take(20).any(|d| {
                    d.rule == "C"
                        && d.target_id.as_deref() == Some(claim.task_id)
                        && d.reasoning.contains(&ls.task_run_id)
                });
                if already {
                    continue;
                }

                let body = format!(
                    "session {} is editing future-claim path {} ahead of schedule; upcoming task {} (plan {}) hasn't started",
                    ls.task_run_id, claim.path, claim.task_id, claim.plan_id
                );
                return Some(DecideOutcome {
                    rule: "C",
                    action: CoordinatorAction::AdviseWithText {
                        target_id: Some(claim.task_id.to_string()),
                        reasoning: stamp_reasoning("C", &body),
                    },
                });
            }
        }
    }
    None
}

/// Ref-style alias to `UpcomingClaim` — only the columns Rule C reads.
struct UpcomingClaimRef<'a> {
    plan_id: &'a String,
    task_id: &'a String,
    path: &'a String,
}

// Make UpcomingClaim importable here without a circular module import.
use crate::executor::upcoming_file_registry::UpcomingClaim;

// =============================================================================
// Rule D — review-completed handling
// =============================================================================

const REVIEW_AUTOMERGE_THRESHOLD: f64 = 0.85;
const REVIEW_USERAPPROVAL_THRESHOLD: f64 = 0.7;
const REASSIGN_RETRY_CAP: i64 = 3;

pub(crate) fn rule_d(obs: &Observation) -> Option<DecideOutcome> {
    // "Process new reviews only": a review is new if it was created
    // strictly after the most-recent decision row for the same task.
    let latest_decision_at: std::collections::HashMap<&str, &str> = obs
        .recent_decisions
        .iter()
        .filter_map(|d| d.target_id.as_deref().map(|t| (t, d.created_at.as_str())))
        .fold(std::collections::HashMap::new(), |mut acc, (t, ts)| {
            acc.entry(t).or_insert(ts);
            acc
        });

    for review in &obs.recent_reviews {
        // Skip reviews older than the latest decision for the same task
        // — that decision is the authoritative response.
        if let Some(decision_ts) = latest_decision_at.get(review.task_id.as_str()) {
            if review.created_at.as_str() <= *decision_ts {
                continue;
            }
        }

        return Some(rule_d_decide(review, &obs.recent_decisions));
    }
    None
}

fn rule_d_decide(review: &ReviewRow, recent_decisions: &[CoordinatorDecisionRow]) -> DecideOutcome {
    let task_id = &review.task_id;

    match review.verdict.as_str() {
        "approved" if review.confidence >= REVIEW_AUTOMERGE_THRESHOLD => {
            let body = format!(
                "review approved (confidence {:.2} >= {:.2}); auto-merging task {}",
                review.confidence, REVIEW_AUTOMERGE_THRESHOLD, task_id
            );
            DecideOutcome {
                rule: "D",
                action: CoordinatorAction::MergeTask {
                    task_id: task_id.clone(),
                    reasoning: Some(stamp_reasoning("D", &body)),
                },
            }
        }
        "approved" if review.confidence >= REVIEW_USERAPPROVAL_THRESHOLD => {
            let body = format!(
                "review approved at confidence {:.2} (in [{:.2}, {:.2})); user approval needed before merge of task {}",
                review.confidence,
                REVIEW_USERAPPROVAL_THRESHOLD,
                REVIEW_AUTOMERGE_THRESHOLD,
                task_id
            );
            DecideOutcome {
                rule: "D",
                action: CoordinatorAction::AdviseWithText {
                    target_id: Some(task_id.clone()),
                    reasoning: stamp_reasoning("D", &body),
                },
            }
        }
        "approved" => {
            // Confidence < 0.7 on an "approved" verdict shouldn't auto-merge;
            // surface to user as advisory.
            let body = format!(
                "review approved at low confidence {:.2} (< {:.2}); manual review of task {}",
                review.confidence, REVIEW_USERAPPROVAL_THRESHOLD, task_id
            );
            DecideOutcome {
                rule: "D",
                action: CoordinatorAction::AdviseWithText {
                    target_id: Some(task_id.clone()),
                    reasoning: stamp_reasoning("D", &body),
                },
            }
        }
        "needs_fix" => {
            // Retry count = number of prior `reassign-needs-fix` decisions
            // logged against this task. count_needs_fix_for_task on the PG
            // side counts review rows; we read the decision log instead so
            // the cap is "we've already tried N times", not "the task got
            // N needs_fix verdicts" (those can come from re-reviews of the
            // same attempt).
            let retries = recent_decisions
                .iter()
                .filter(|d| {
                    d.action == "reassign-needs-fix"
                        && d.target_id.as_deref() == Some(task_id.as_str())
                })
                .count() as i64;

            if retries < REASSIGN_RETRY_CAP {
                let body = format!(
                    "review needs_fix (retry {} of {}); re-assigning task {} with reviewer feedback",
                    retries + 1,
                    REASSIGN_RETRY_CAP,
                    task_id
                );
                DecideOutcome {
                    rule: "D",
                    action: CoordinatorAction::ReassignNeedsFix {
                        task_id: task_id.clone(),
                        reasoning: stamp_reasoning("D", &body),
                    },
                }
            } else {
                let body = format!(
                    "review needs_fix and retry cap ({}) reached for task {}; escalating to user",
                    REASSIGN_RETRY_CAP, task_id
                );
                DecideOutcome {
                    rule: "D",
                    action: CoordinatorAction::EscalateWithText {
                        target_id: Some(task_id.clone()),
                        reasoning: stamp_reasoning("D", &body),
                    },
                }
            }
        }
        "escalate" => {
            let body = format!(
                "review verdict=escalate for task {}: {}",
                task_id, review.reasoning
            );
            DecideOutcome {
                rule: "D",
                action: CoordinatorAction::EscalateWithText {
                    target_id: Some(task_id.clone()),
                    reasoning: stamp_reasoning("D", &body),
                },
            }
        }
        other => {
            // Unknown verdict — surface as advisory rather than silently
            // dropping. Mirrors the slash command's "no silent moves" rule.
            let body = format!(
                "unrecognized review verdict {:?} for task {}; advising user",
                other, task_id
            );
            DecideOutcome {
                rule: "D",
                action: CoordinatorAction::AdviseWithText {
                    target_id: Some(task_id.clone()),
                    reasoning: stamp_reasoning("D", &body),
                },
            }
        }
    }
}

// =============================================================================
// Rule E — dependency unblock
// =============================================================================

/// Computed unblock target: a pending task whose deps are all `done`.
/// The scheduler will issue an `IdleNoAction` row for each and rely on
/// the side-effect path (PG `mark_ready_for_unblocked`) to do the actual
/// `pending → ready` flip.
#[derive(Debug, Clone)]
pub(crate) struct UnblockTarget {
    pub plan_id: String,
    pub task_id: String,
}

/// Pure version of Rule E — returns the set of tasks the scheduler should
/// unblock this tick. Returns `None` when nothing is unblocked. The
/// scheduler then issues PG side effects.
pub(crate) fn rule_e_targets(obs: &Observation) -> Vec<UnblockTarget> {
    use std::collections::HashSet;

    // All `done` task ids in the snapshot's plans. A pending task is
    // unblocked when every dep id is in this set OR the task has no deps.
    // NON_TERMINAL_STATUSES excludes `done`, so `obs.tasks` won't contain
    // `done` rows — meaning we have to be conservative and only unblock
    // when `depends_on` is empty. To keep this faithful to the SQL
    // version (`mark_ready_for_unblocked`), the scheduler delegates the
    // actual flip to that function instead.
    //
    // What we *do* know: `pending` tasks whose entire `depends_on` array
    // is empty are trivially ready. Identify them here so the scheduler
    // can log them as Rule-E unblocks even when the PG-side flip is
    // delegated.
    let mut targets: Vec<UnblockTarget> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for t in &obs.tasks {
        if t.status == "pending" && t.depends_on.is_empty() {
            // Only emit one IdleNoAction per task per tick.
            let key = t.id.clone();
            if !seen.insert(key.clone()) {
                continue;
            }
            // Idempotency: skip if Rule E already filed an
            // IdleNoAction for this task in the recent window.
            let already = obs
                .recent_decisions
                .iter()
                .take(50)
                .any(|d| d.rule == "E" && d.target_id.as_deref() == Some(t.id.as_str()));
            if !already {
                targets.push(UnblockTarget {
                    plan_id: t.plan_id.clone(),
                    task_id: t.id.clone(),
                });
            }
        }
    }
    targets
}

/// Translate the first unblock target into an `IdleNoAction` decision so
/// the audit log has a row noting the unblock. The scheduler still has
/// to call PG `mark_ready_for_unblocked` for the per-plan flip — see the
/// scheduler module.
pub(crate) fn rule_e(obs: &Observation) -> Option<DecideOutcome> {
    let targets = rule_e_targets(obs);
    let first = targets.first()?;
    Some(DecideOutcome {
        rule: "E",
        action: CoordinatorAction::IdleNoAction,
    })
    .map(|mut o| {
        // Attach the unblock note via reasoning prefix on a follow-up
        // AdviseWithText? No — IdleNoAction has no reasoning field on
        // the wire. The scheduler annotates the persisted decision row's
        // `reasoning` separately via the action_reasoning helper, but
        // for IdleNoAction that returns a fixed "no cheap rule fired
        // this iteration" string. To keep the audit row useful, return
        // an AdviseWithText instead so the unblock note shows up.
        let body = format!(
            "task {} (plan {}) has no deps and is pending → triggering ready transition",
            first.task_id, first.plan_id
        );
        o.action = CoordinatorAction::AdviseWithText {
            target_id: Some(first.task_id.clone()),
            reasoning: stamp_reasoning("E", &body),
        };
        o
    })
}

// =============================================================================
// Tests
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::pg::tasks::TaskRow;
    use std::collections::HashMap;

    // --- fixture helpers --------------------------------------------------

    fn empty_obs() -> Observation {
        Observation {
            tasks: Vec::new(),
            active_file_registry: Vec::new(),
            upcoming_file_registry: Vec::new(),
            live_sessions: Vec::new(),
            recent_reviews: Vec::new(),
            open_escalations: Vec::new(),
            session_touched_files: HashMap::new(),
            recent_decisions: Vec::new(),
        }
    }

    fn make_task(id: &str, status: &str) -> TaskRow {
        TaskRow {
            id: id.to_string(),
            plan_id: "plan-1".to_string(),
            plan_version_hash: "h".to_string(),
            phase_name: "phase-1".to_string(),
            sequence_in_phase: 0,
            description: "desc".to_string(),
            expected_file_claims: Vec::new(),
            expected_dirs: Vec::new(),
            depends_on: Vec::new(),
            status: status.to_string(),
            assigned_session_id: None,
            started_at: None,
            completed_at: None,
            created_at: "2026-05-02T00:00:00Z".to_string(),
            updated_at: "2026-05-02T00:00:00Z".to_string(),
            notes: None,
        }
    }

    fn make_session(task_run_id: &str, state: SessionState) -> LiveSession {
        LiveSession {
            task_run_id: task_run_id.to_string(),
            state: Some(state),
            assigned_task_id: None,
        }
    }

    fn make_review(task_id: &str, verdict: &str, confidence: f64, created_at: &str) -> ReviewRow {
        ReviewRow {
            id: format!("review-{}", task_id),
            task_id: task_id.to_string(),
            reviewer_session_id: "rev".to_string(),
            reviewed_session_id: "wrk".to_string(),
            verdict: verdict.to_string(),
            confidence,
            reasoning: "test reasoning".to_string(),
            diff_summary: None,
            test_results: None,
            user_decision: None,
            user_decided_at: None,
            created_at: created_at.to_string(),
        }
    }

    fn make_decision(rule: &str, action: &str, target: Option<&str>) -> CoordinatorDecisionRow {
        CoordinatorDecisionRow {
            id: format!("d-{}-{}", rule, action),
            session_id: "coord-test".to_string(),
            iteration: 0,
            rule: rule.to_string(),
            action: action.to_string(),
            target_id: target.map(|s| s.to_string()),
            reasoning: "test".to_string(),
            auto_acted: true,
            resolved: false,
            resolution: None,
            resolved_at: None,
            created_at: "2026-05-01T00:00:00Z".to_string(),
        }
    }

    // --- Rule A ----------------------------------------------------------

    #[test]
    fn rule_a_fires_on_session_processing_for_over_5_minutes() {
        let mut obs = empty_obs();
        obs.live_sessions
            .push(make_session("sess-1", SessionState::Processing));
        let mut tracker = HashMap::new();
        let now_ms: i64 = 10_000_000;
        // Seen 6 minutes ago.
        tracker.insert(
            "sess-1".to_string(),
            ProcessingSeenSince {
                since_ms: now_ms - 6 * 60 * 1000,
            },
        );

        let outcome = rule_a(&obs, &tracker, now_ms).expect("rule A should fire");
        assert_eq!(outcome.rule, "A");
        match outcome.action {
            CoordinatorAction::AdviseWithText { target_id, .. } => {
                assert_eq!(target_id.as_deref(), Some("sess-1"));
            }
            _ => panic!("expected AdviseWithText"),
        }
    }

    #[test]
    fn rule_a_skips_when_already_advised_recently() {
        let mut obs = empty_obs();
        obs.live_sessions
            .push(make_session("sess-1", SessionState::Processing));
        obs.recent_decisions
            .push(make_decision("A", "advise-with-text", Some("sess-1")));
        let mut tracker = HashMap::new();
        let now_ms: i64 = 10_000_000;
        tracker.insert(
            "sess-1".to_string(),
            ProcessingSeenSince {
                since_ms: now_ms - 10 * 60 * 1000,
            },
        );

        assert!(rule_a(&obs, &tracker, now_ms).is_none());
    }

    #[test]
    fn rule_a_does_not_fire_when_session_is_ready() {
        let mut obs = empty_obs();
        obs.live_sessions
            .push(make_session("sess-1", SessionState::Ready));
        let tracker = HashMap::new();
        assert!(rule_a(&obs, &tracker, 10_000_000).is_none());
    }

    // --- Rule B ----------------------------------------------------------

    #[test]
    fn rule_b_assigns_when_one_idle_session_matches() {
        let mut obs = empty_obs();
        obs.tasks.push(make_task("t-1", "ready"));
        obs.live_sessions
            .push(make_session("sess-1", SessionState::Ready));

        let outcome = rule_b(&obs).expect("rule B should fire");
        assert_eq!(outcome.rule, "B");
        match outcome.action {
            CoordinatorAction::AssignTask {
                task_id,
                session_id,
                ..
            } => {
                assert_eq!(task_id, "t-1");
                assert_eq!(session_id, "sess-1");
            }
            _ => panic!("expected AssignTask"),
        }
    }

    #[test]
    fn rule_b_advises_when_no_idle_sessions() {
        let mut obs = empty_obs();
        obs.tasks.push(make_task("t-1", "ready"));
        // Session exists but is Processing — not idle.
        obs.live_sessions
            .push(make_session("sess-1", SessionState::Processing));

        let outcome = rule_b(&obs).expect("rule B should fire (no-suitable advisory)");
        match outcome.action {
            CoordinatorAction::AdviseWithText { target_id, .. } => {
                assert_eq!(target_id.as_deref(), Some("t-1"));
            }
            _ => panic!("expected AdviseWithText"),
        }
    }

    #[test]
    fn rule_b_skips_path_overlap() {
        let mut obs = empty_obs();
        let mut task = make_task("t-1", "ready");
        task.expected_file_claims = vec!["src/foo.rs".to_string()];
        obs.tasks.push(task);
        obs.live_sessions
            .push(make_session("sess-1", SessionState::Ready));
        obs.session_touched_files
            .insert("sess-1".to_string(), vec!["src/foo.rs".to_string()]);

        // Overlap → not eligible → "no suitable" advisory.
        let outcome = rule_b(&obs).expect("rule B should fire");
        match outcome.action {
            CoordinatorAction::AdviseWithText { .. } => {}
            _ => panic!("expected AdviseWithText (no candidate)"),
        }
    }

    #[test]
    fn rule_b_idempotent_on_recent_assign() {
        let mut obs = empty_obs();
        obs.tasks.push(make_task("t-1", "ready"));
        obs.live_sessions
            .push(make_session("sess-1", SessionState::Ready));
        obs.recent_decisions
            .push(make_decision("B", "assign-task", Some("t-1")));

        // Recent assign → skipped. No other tasks → returns None.
        assert!(rule_b(&obs).is_none());
    }

    // --- Rule C ----------------------------------------------------------

    #[test]
    fn rule_c_fires_when_running_session_touches_upcoming_path() {
        let mut obs = empty_obs();

        // Upcoming task t-2 has not started; claims src/future.rs
        let mut t2 = make_task("t-2", "pending");
        t2.expected_file_claims = vec!["src/future.rs".to_string()];
        obs.tasks.push(t2);

        // Running session sess-1 (assigned to a different task) is
        // touching the upcoming claim path.
        let mut session = make_session("sess-1", SessionState::Processing);
        session.assigned_task_id = Some("t-1".to_string());
        obs.live_sessions.push(session);
        obs.session_touched_files
            .insert("sess-1".to_string(), vec!["src/future.rs".to_string()]);

        obs.upcoming_file_registry.push(UpcomingClaim {
            plan_id: "plan-1".to_string(),
            task_id: "t-2".to_string(),
            path: "src/future.rs".to_string(),
            plan_version_hash: "h".to_string(),
            registered_at_ms: 0,
        });

        let outcome = rule_c(&obs).expect("rule C should fire");
        assert_eq!(outcome.rule, "C");
        match outcome.action {
            CoordinatorAction::AdviseWithText { target_id, .. } => {
                assert_eq!(target_id.as_deref(), Some("t-2"));
            }
            _ => panic!("expected AdviseWithText"),
        }
    }

    #[test]
    fn rule_c_no_fire_when_no_overlap() {
        let mut obs = empty_obs();
        let mut t2 = make_task("t-2", "pending");
        t2.expected_file_claims = vec!["src/future.rs".to_string()];
        obs.tasks.push(t2);

        let mut session = make_session("sess-1", SessionState::Processing);
        session.assigned_task_id = Some("t-1".to_string());
        obs.live_sessions.push(session);
        obs.session_touched_files
            .insert("sess-1".to_string(), vec!["src/other.rs".to_string()]);

        obs.upcoming_file_registry.push(UpcomingClaim {
            plan_id: "plan-1".to_string(),
            task_id: "t-2".to_string(),
            path: "src/future.rs".to_string(),
            plan_version_hash: "h".to_string(),
            registered_at_ms: 0,
        });

        assert!(rule_c(&obs).is_none());
    }

    // --- Rule D ----------------------------------------------------------

    #[test]
    fn rule_d_auto_merges_high_confidence_approval() {
        let mut obs = empty_obs();
        obs.recent_reviews.push(make_review(
            "task-1",
            "approved",
            0.95,
            "2026-05-02T01:00:00Z",
        ));

        let outcome = rule_d(&obs).expect("rule D should fire");
        assert_eq!(outcome.rule, "D");
        match outcome.action {
            CoordinatorAction::MergeTask { task_id, .. } => {
                assert_eq!(task_id, "task-1");
            }
            _ => panic!("expected MergeTask"),
        }
    }

    #[test]
    fn rule_d_advises_on_mid_confidence_approval() {
        let mut obs = empty_obs();
        obs.recent_reviews.push(make_review(
            "task-1",
            "approved",
            0.75,
            "2026-05-02T01:00:00Z",
        ));

        let outcome = rule_d(&obs).expect("rule D should fire");
        match outcome.action {
            CoordinatorAction::AdviseWithText { target_id, .. } => {
                assert_eq!(target_id.as_deref(), Some("task-1"));
            }
            _ => panic!("expected AdviseWithText"),
        }
    }

    #[test]
    fn rule_d_reassigns_needs_fix_under_cap() {
        let mut obs = empty_obs();
        obs.recent_reviews.push(make_review(
            "task-1",
            "needs_fix",
            0.5,
            "2026-05-02T01:00:00Z",
        ));
        // 1 prior reassign — under cap of 3.
        obs.recent_decisions
            .push(make_decision("D", "reassign-needs-fix", Some("task-1")));

        let outcome = rule_d(&obs).expect("rule D should fire");
        match outcome.action {
            CoordinatorAction::ReassignNeedsFix { task_id, .. } => {
                assert_eq!(task_id, "task-1");
            }
            _ => panic!("expected ReassignNeedsFix"),
        }
    }

    #[test]
    fn rule_d_escalates_after_retry_cap() {
        let mut obs = empty_obs();
        obs.recent_reviews.push(make_review(
            "task-1",
            "needs_fix",
            0.5,
            "2026-05-02T01:00:00Z",
        ));
        // 3 prior reassigns — at cap.
        for _ in 0..3 {
            obs.recent_decisions
                .push(make_decision("D", "reassign-needs-fix", Some("task-1")));
        }

        let outcome = rule_d(&obs).expect("rule D should fire");
        match outcome.action {
            CoordinatorAction::EscalateWithText { target_id, .. } => {
                assert_eq!(target_id.as_deref(), Some("task-1"));
            }
            _ => panic!("expected EscalateWithText"),
        }
    }

    #[test]
    fn rule_d_skips_reviews_older_than_latest_decision() {
        let mut obs = empty_obs();
        obs.recent_reviews.push(make_review(
            "task-1",
            "approved",
            0.95,
            "2026-05-02T00:30:00Z", // before the decision
        ));
        // Latest decision is at 01:00:00Z — newer than the review.
        let mut d = make_decision("D", "merge-task", Some("task-1"));
        d.created_at = "2026-05-02T01:00:00Z".to_string();
        obs.recent_decisions.push(d);

        assert!(rule_d(&obs).is_none());
    }

    #[test]
    fn rule_d_escalate_verdict_emits_escalate_with_text() {
        let mut obs = empty_obs();
        obs.recent_reviews.push(make_review(
            "task-1",
            "escalate",
            0.0,
            "2026-05-02T01:00:00Z",
        ));
        let outcome = rule_d(&obs).expect("rule D should fire");
        match outcome.action {
            CoordinatorAction::EscalateWithText { target_id, .. } => {
                assert_eq!(target_id.as_deref(), Some("task-1"));
            }
            _ => panic!("expected EscalateWithText"),
        }
    }

    // --- Rule E ----------------------------------------------------------

    #[test]
    fn rule_e_emits_unblock_for_dep_free_pending_task() {
        let mut obs = empty_obs();
        obs.tasks.push(make_task("t-1", "pending"));

        let outcome = rule_e(&obs).expect("rule E should fire");
        assert_eq!(outcome.rule, "E");
        match outcome.action {
            CoordinatorAction::AdviseWithText { target_id, .. } => {
                assert_eq!(target_id.as_deref(), Some("t-1"));
            }
            _ => panic!("expected AdviseWithText (Rule E unblock note)"),
        }
    }

    #[test]
    fn rule_e_no_fire_when_pending_has_unsatisfied_deps() {
        let mut obs = empty_obs();
        let mut t = make_task("t-1", "pending");
        t.depends_on = vec!["dep-not-done".to_string()];
        obs.tasks.push(t);
        // No `done` info; conservatively don't unblock.
        assert!(rule_e(&obs).is_none());
    }

    #[test]
    fn rule_e_idempotent_on_recent_unblock() {
        let mut obs = empty_obs();
        obs.tasks.push(make_task("t-1", "pending"));
        obs.recent_decisions
            .push(make_decision("E", "advise-with-text", Some("t-1")));

        assert!(rule_e(&obs).is_none());
    }
}
