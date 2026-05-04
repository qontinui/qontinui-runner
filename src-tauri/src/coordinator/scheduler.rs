//! In-process Rust Coordinator scheduler — Phase 1b.
//!
//! Periodically observes the productivity-stack state, applies the cheap
//! rules A–E from `qontinui-claude-config/.claude/commands/coordinate.md`,
//! and writes a `coord.coordinator_decisions` row per fired rule (and an
//! `idle-no-action` row when no rule fires). Mirrors the loop the
//! `/coordinate` Claude session runs today, minus the LLM callout — that
//! ships in Phase 2.
//!
//! Patterned after `crate::memory::scheduler::start_memory_scheduler`,
//! but takes `Arc<ApiState>` instead of `Arc<PgDb>` because the act path
//! needs `SessionManager` via Tauri-state lookup.
//!
//! ## Lease semantics
//!
//! `try_acquire_coordinator_lease` is idempotent: same-instance calls
//! extend the lease. The loop calls it every tick — when a Claude
//! `/coordinate` session is the incumbent, our calls return `Ok(false)`
//! and the loop body short-circuits.
//!
//! ## Runtime toggle (Phase 1.5)
//!
//! The scheduler always starts at boot now — never gated at startup.
//! Whether each tick does work is governed by an `Arc<AtomicBool>` flag
//! exposed via [`CoordinatorSchedulerHandle`]. The flag's initial value
//! still comes from `CoordinatorSchedulerConfig::rust_scheduler_enabled`
//! (env-var driven for first boot), but the
//! `launch_coordinator_session` Tauri command flips it at runtime via
//! the handle stashed in Tauri state — no process restart needed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::async_runtime;
use tauri::Emitter;
use tokio::sync::Mutex;
use tokio::time::interval;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::claude_session::state::SessionState;
use crate::database::pg::completion_reports::{UnblockEvaluation, UnblockOutcome};
use crate::database::pg::coordinator_decisions::InsertCoordinatorDecisionInput;
use crate::mcp::coordinator::CoordinatorAction;
use crate::mcp::types::ApiState;
use crate::productivity::review::{auto_review_in_product, ReviewError};

use super::act;
use super::config::CoordinatorSchedulerConfig;
use super::decide::{self, stamp_reasoning, DecideOutcome, ProcessingSeenSince, RULE_VERSION};
use super::llm_decide::{self, LlmBudget};
use super::observe::observe;

/// Runtime-toggle handle for the Rust coordinator scheduler.
///
/// Returned by [`start_coordinator_scheduler`] and stashed in Tauri state
/// so the `launch_coordinator_session` / `stop_coordinator_session`
/// Tauri commands can flip the loop on or off without restarting the
/// runner process. The scheduler task itself is kept alive in the same
/// struct (the `JoinHandle`) — dropping the handle is a no-op for the
/// background task because `async_runtime::spawn` detaches it.
#[derive(Clone)]
pub struct CoordinatorSchedulerHandle {
    enabled: Arc<AtomicBool>,
}

impl CoordinatorSchedulerHandle {
    /// True when the loop will execute its tick body. Reading is lock-free.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Set the enabled flag. Returns the previous value so callers can
    /// detect no-op transitions if they care.
    pub fn set_enabled(&self, value: bool) -> bool {
        self.enabled.swap(value, Ordering::Relaxed)
    }

    /// Test-only constructor — used by unit tests that drive
    /// `should_run_tick` directly without spawning the loop.
    #[cfg(test)]
    pub(crate) fn new_for_test(enabled: bool) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
        }
    }
}

/// Start the scheduler. Returns the runtime-toggle handle so the caller
/// can flip the loop on/off later. Mirrors `start_memory_scheduler`'s
/// contract — the loop runs forever and is dropped only when the runner
/// exits.
pub fn start_coordinator_scheduler(
    state: Arc<ApiState>,
    config: CoordinatorSchedulerConfig,
) -> CoordinatorSchedulerHandle {
    let enabled = Arc::new(AtomicBool::new(config.rust_scheduler_enabled));
    let handle = CoordinatorSchedulerHandle {
        enabled: enabled.clone(),
    };
    let initial_enabled = config.rust_scheduler_enabled;

    let auto_review_enabled = config.auto_review_enabled;

    async_runtime::spawn(async move {
        // Initial delay matches the memory scheduler — let the runner
        // finish bootstrapping before we contend for the lease.
        tokio::time::sleep(Duration::from_secs(config.initial_delay_secs)).await;

        let instance_id = format!("rust-{}", Uuid::new_v4());
        info!(
            "Coordinator (Rust) scheduler started: instance_id={} interval={}s rule_version={} max_llm_calls_per_hour={} auto_review_enabled={} initial_enabled={}",
            instance_id,
            config.interval_secs,
            RULE_VERSION,
            config.max_llm_calls_per_hour,
            auto_review_enabled,
            initial_enabled,
        );

        let mut tick = interval(Duration::from_secs(config.interval_secs));
        let mut iteration: i64 = 0;
        // Tracks first-seen-as-Processing timestamps for Rule A.
        let mut processing_seen: HashMap<String, ProcessingSeenSince> = HashMap::new();
        // Best-effort hourly LLM budget. Mutex so a future intra-iteration
        // re-entry (parallelizing across plans) can't burst past the cap.
        let llm_budget: Arc<Mutex<LlmBudget>> = Arc::new(Mutex::new(LlmBudget::new()));
        let max_llm_calls_per_hour = config.max_llm_calls_per_hour;

        // Track whether the previous tick held the lease — when ownership
        // flips back to us the metric counter increments.
        let mut held_lease_last_tick = false;

        loop {
            tick.tick().await;
            iteration = iteration.wrapping_add(1);

            // Phase 1.5 runtime toggle: when disabled, skip the entire
            // tick body (no lease acquire, no log spam — just a debug
            // line so the loop's heartbeat is visible). When the user
            // flips the Start button, the next tick proceeds.
            if !should_run_tick(&enabled) {
                debug!(
                    "Coordinator scheduler: rust_scheduler_enabled=false, skipping tick (iter={})",
                    iteration
                );
                // Reset lease-takeover bookkeeping while disabled so the
                // first tick after re-enable counts as a takeover.
                held_lease_last_tick = false;
                continue;
            }

            let pg = &state.app_state.pg_db;
            match pg.try_acquire_coordinator_lease(&instance_id, 60).await {
                Ok(true) => {
                    if !held_lease_last_tick {
                        info!(
                            "coordinator.lease_takeovers_total instance_id={}",
                            instance_id
                        );
                    }
                    held_lease_last_tick = true;
                }
                Ok(false) => {
                    held_lease_last_tick = false;
                    debug!("Coordinator scheduler: lease held by another instance, skipping tick");
                    continue;
                }
                Err(e) => {
                    held_lease_last_tick = false;
                    warn!("Coordinator scheduler: lease acquire failed: {}", e);
                    continue;
                }
            }

            info!(
                "coordinator.iterations_total instance_id={} iteration={}",
                instance_id, iteration
            );

            if let Err(e) = run_one_iteration(
                &state,
                &instance_id,
                iteration,
                &mut processing_seen,
                &llm_budget,
                max_llm_calls_per_hour,
            )
            .await
            {
                error!("Coordinator iteration failed: {}", e);
            }

            // Phase 4 auto-trigger: fire reviews for tasks newly observed
            // in `review` status that don't already have a verdict. Spawned
            // off the iteration so the LLM call doesn't block the next
            // tick. Gated on the config flag (default on, but disabled
            // when no LLM provider is configured — checked below per-call
            // via OneshotError::Disabled).
            if auto_review_enabled {
                maybe_trigger_auto_reviews(&state).await;
            }
        }
    });

    handle
}

/// One observe → decide → act → log iteration. Public to the module so
/// integration tests (Phase 2/3) can drive it directly with a test
/// `ApiState`.
pub(crate) async fn run_one_iteration(
    state: &Arc<ApiState>,
    instance_id: &str,
    iteration: i64,
    processing_seen: &mut HashMap<String, ProcessingSeenSince>,
    llm_budget: &Arc<Mutex<LlmBudget>>,
    max_llm_calls_per_hour: u32,
) -> Result<(), String> {
    let obs = observe(state).await?;

    // Update the Rule-A tracker before evaluating rules — sessions that
    // dropped out of Processing should be evicted, and new Processing
    // observations get their first-seen timestamp recorded.
    let now_ms = current_unix_millis();
    refresh_processing_tracker(processing_seen, &obs.live_sessions, now_ms);

    // Walk the rules in order; first one that fires wins for the iteration.
    let outcomes: Vec<DecideOutcome> = {
        let mut acc = Vec::new();
        if let Some(o) = decide::rule_a(&obs, processing_seen, now_ms) {
            acc.push(o);
        }
        if acc.is_empty() {
            if let Some(o) = decide::rule_b(&obs) {
                acc.push(o);
            }
        }
        if acc.is_empty() {
            if let Some(o) = decide::rule_c(&obs) {
                acc.push(o);
            }
        }
        if acc.is_empty() {
            if let Some(o) = decide::rule_d(&obs) {
                acc.push(o);
            }
        }
        if acc.is_empty() {
            if let Some(o) = decide::rule_e(&obs) {
                acc.push(o);
            }
        }
        acc
    };

    if outcomes.is_empty() {
        // No cheap rule fired. If there's still pending/ready/escalated
        // work, hand off to the LLM (Phase 2). On budget-out / no warm
        // creds / parse failure, fall through to a single
        // AdviseWithText "no clear plan" row.
        if obs.has_unhandled_work() {
            info!("coordinator.cheap_rules_fired_none has_unhandled_work=true");
            let mut budget_guard = llm_budget.lock().await;
            let llm_outcome = llm_decide::maybe_decide_with_llm(
                state,
                &obs,
                &obs.recent_decisions,
                &mut *budget_guard,
                max_llm_calls_per_hour,
            )
            .await;
            drop(budget_guard);

            match llm_outcome {
                Some(outcome) => {
                    info!("coordinator.llm_callouts_total instance_id={}", instance_id);
                    apply_and_log(state, instance_id, iteration, outcome).await?;
                    return Ok(());
                }
                None => {
                    let outcome = advise_no_plan(&obs);
                    apply_and_log(state, instance_id, iteration, outcome).await?;
                    return Ok(());
                }
            }
        }

        // No cheap rule fired and nothing to do at all — log idle row to
        // keep the audit trail dense (per `coordinate.md` line 86).
        let body = decide::stamp_reasoning("idle", "no cheap rule fired this iteration");
        log_decision(
            state,
            instance_id,
            iteration,
            "idle",
            "idle-no-action",
            None,
            &body,
            true,
        )
        .await?;
        return Ok(());
    }

    for outcome in outcomes {
        let action_name = act::action_name(&outcome.action);
        let target = act::action_target(&outcome.action);
        let reasoning = act::action_reasoning(&outcome.action);
        let advise_only = act::must_advise_only(&outcome.action);
        let auto_acted = !advise_only;

        info!(
            "coordinator.cheap_rules_fired rule={} action={}",
            outcome.rule, action_name
        );

        if auto_acted {
            if let Err((_, e)) = act::apply(state, &outcome.action).await {
                warn!(
                    "Coordinator action {} apply failed: {} (logging row anyway)",
                    action_name, e
                );
            }
        }

        log_decision(
            state,
            instance_id,
            iteration,
            outcome.rule,
            action_name,
            target.as_deref(),
            &reasoning,
            auto_acted,
        )
        .await?;

        // Rule E side effect: after logging the unblock note, ask PG to
        // do the actual `pending → ready` flip. Per
        // productivity-coordinator-completion-reports §4 the flip must:
        //   (a) read each upstream's completion_report;
        //   (b) refuse if any upstream's follow-ups flag
        //       blocking_for_dependents (emit a coordinator-escalation
        //       row with action `blocking-follow-up-detected`);
        //   (c) emit `data-integrity-anomaly` when an upstream's report
        //       is null despite being `done`;
        //   (d) flip pending → ready in the same transaction that stashes
        //       the upstream reports in `assignment_brief_extras` for
        //       Rule B's brief composer to consume.
        //
        // `mark_ready_for_unblocked_with_briefs` returns one
        // `UnblockOutcome` per candidate row so we can translate each into
        // a per-task decision row here.
        if outcome.rule == "E" {
            if let Some(plan_id) = task_plan_for_target(&obs, target.as_deref()) {
                match state
                    .app_state
                    .pg_db
                    .mark_ready_for_unblocked_with_briefs(&plan_id)
                    .await
                {
                    Ok(outcomes) => {
                        for o in outcomes {
                            apply_rule_e_outcome(state, instance_id, iteration, o).await;
                        }
                    }
                    Err(e) => {
                        warn!("Rule E mark_ready_for_unblocked_with_briefs failed: {}", e);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Per-task post-processing for Rule E. Translates one `UnblockOutcome`
/// into the right audit-row + Tauri-event pair so the dashboard can render
/// the dependency-resolution progress without polling.
async fn apply_rule_e_outcome(
    state: &Arc<ApiState>,
    instance_id: &str,
    iteration: i64,
    outcome: UnblockOutcome,
) {
    let task_id = outcome.task_id;
    match outcome.evaluation {
        UnblockEvaluation::Flipped => {
            // No additional row — the outer Rule-E AdviseWithText already
            // captured the unblock intent. Emit a coordinator-wakeup-style
            // signal so the dashboard refreshes the task panel without
            // polling.
            if let Err(e) = state.app_handle.emit(
                "task-flipped-ready",
                serde_json::json!({
                    "taskId": task_id,
                }),
            ) {
                debug!("emit task-flipped-ready failed: {}", e);
            }
        }
        UnblockEvaluation::Blocked { blockers } => {
            // Build a human-readable reasoning string listing every
            // blocking follow-up and which upstream surfaced it.
            let mut body = format!(
                "task {} has all deps done but Rule E refuses the flip — \
                 upstream completion reports flag blocking follow-ups:",
                task_id
            );
            for b in &blockers {
                body.push_str(&format!(
                    "\n  - upstream {} [{}]: {}",
                    b.upstream_task_id, b.priority, b.description
                ));
            }
            body.push_str(
                "\n\nUser action: either mark the follow-ups not-blocking, \
                 cancel the dependent task, or fire \
                 `force-flip-ready-despite-blocker` from the dashboard.",
            );
            let reasoning = stamp_reasoning("E", &body);
            if let Err(e) = log_decision(
                state,
                instance_id,
                iteration,
                "E",
                "blocking-follow-up-detected",
                Some(&task_id),
                &reasoning,
                false, // advisory: needs user resolution
            )
            .await
            {
                warn!(
                    "rule_e blocking-follow-up-detected log failed for task {}: {}",
                    task_id, e
                );
            }
            // Surface as an escalation card so the dashboard's escalations
            // panel notices.
            if let Err(e) = state.app_handle.emit(
                "coordinator-escalation",
                serde_json::json!({
                    "rule": "E",
                    "action": "blocking-follow-up-detected",
                    "target_id": task_id,
                    "reasoning": reasoning,
                }),
            ) {
                debug!("emit coordinator-escalation (blocker) failed: {}", e);
            }
        }
        UnblockEvaluation::MissingReport { upstream_ids } => {
            let body = format!(
                "task {} has all deps done but at least one upstream is \
                 done with a null completion_report (data-integrity \
                 anomaly): upstream_ids={:?}. Backfill expected \
                 'legacy-pre-completion-reports' or fire a manual-user-fire \
                 source via the dashboard.",
                task_id, upstream_ids
            );
            let reasoning = stamp_reasoning("E", &body);
            if let Err(e) = log_decision(
                state,
                instance_id,
                iteration,
                "E",
                "data-integrity-anomaly",
                Some(&task_id),
                &reasoning,
                false,
            )
            .await
            {
                warn!(
                    "rule_e data-integrity-anomaly log failed for task {}: {}",
                    task_id, e
                );
            }
            if let Err(e) = state.app_handle.emit(
                "coordinator-escalation",
                serde_json::json!({
                    "rule": "E",
                    "action": "data-integrity-anomaly",
                    "target_id": task_id,
                    "reasoning": reasoning,
                }),
            ) {
                debug!("emit coordinator-escalation (anomaly) failed: {}", e);
            }
        }
        UnblockEvaluation::Skipped { reason } => {
            // No row — racing tick or the candidate became ineligible
            // between SELECT and UPDATE. Debug-log so the audit trail isn't
            // cluttered.
            debug!(
                "rule_e skipped task {}: {} (no row written)",
                task_id, reason
            );
        }
    }
}

/// Look up a task's plan id from the observation, since the unblock
/// helper is per-plan.
fn task_plan_for_target(
    obs: &crate::coordinator::observe::Observation,
    target: Option<&str>,
) -> Option<String> {
    let target = target?;
    obs.tasks
        .iter()
        .find(|t| t.id == target)
        .map(|t| t.plan_id.clone())
}

/// Run the action's side effects (when not advise-only) then persist the
/// decision row. Used by the LLM-callout fallthrough; the cheap-rules
/// loop inlines the equivalent logic so it can also do Rule E's PG flip.
async fn apply_and_log(
    state: &Arc<ApiState>,
    instance_id: &str,
    iteration: i64,
    outcome: DecideOutcome,
) -> Result<(), String> {
    let action_name = act::action_name(&outcome.action);
    let target = act::action_target(&outcome.action);
    let reasoning = act::action_reasoning(&outcome.action);
    let advise_only = act::must_advise_only(&outcome.action);
    let auto_acted = !advise_only;

    if auto_acted {
        if let Err((_, e)) = act::apply(state, &outcome.action).await {
            warn!(
                "Coordinator action {} apply failed: {} (logging row anyway)",
                action_name, e
            );
        }
    }

    log_decision(
        state,
        instance_id,
        iteration,
        outcome.rule,
        action_name,
        target.as_deref(),
        &reasoning,
        auto_acted,
    )
    .await
}

/// Build the fallback advisory used when no cheap rule fired AND the LLM
/// callout couldn't pick anything (no warm credentials, budget out, parse
/// error). Shows up in the dashboard's Advisories panel so the user
/// notices the coordinator wants attention.
fn advise_no_plan(obs: &crate::coordinator::observe::Observation) -> DecideOutcome {
    let body = format!(
        "no cheap rule fired and LLM callout unavailable; tasks={} live_sessions={} open_escalations={} — user attention requested",
        obs.tasks.len(),
        obs.live_sessions.len(),
        obs.open_escalations.len(),
    );
    DecideOutcome {
        rule: "LLM",
        action: CoordinatorAction::AdviseWithText {
            target_id: None,
            reasoning: stamp_reasoning("LLM", &body),
        },
    }
}

/// Persist the decision row. Errors propagate so the caller logs them.
async fn log_decision(
    state: &Arc<ApiState>,
    instance_id: &str,
    iteration: i64,
    rule: &str,
    action: &str,
    target_id: Option<&str>,
    reasoning: &str,
    auto_acted: bool,
) -> Result<(), String> {
    state
        .app_state
        .pg_db
        .insert_coordinator_decision(&InsertCoordinatorDecisionInput {
            session_id: instance_id,
            iteration,
            rule,
            action,
            target_id,
            reasoning,
            auto_acted,
        })
        .await
        .map(|_| ())
}

/// Update the per-tick `(task_run_id, since_ms)` tracker so Rule A can
/// reason about "Processing for ≥ 5 min". Keeps only the sessions still
/// observed as Processing this tick.
fn refresh_processing_tracker(
    tracker: &mut HashMap<String, ProcessingSeenSince>,
    live_sessions: &[crate::coordinator::observe::LiveSession],
    now_ms: i64,
) {
    let processing_now: std::collections::HashSet<String> = live_sessions
        .iter()
        .filter(|ls| ls.state == Some(SessionState::Processing))
        .map(|ls| ls.task_run_id.clone())
        .collect();

    // Drop sessions no longer in Processing.
    tracker.retain(|sid, _| processing_now.contains(sid));

    // Insert new ones with `now_ms` as the first-seen-as-Processing
    // timestamp. Existing entries keep their original timestamp.
    for sid in processing_now {
        tracker
            .entry(sid)
            .or_insert(ProcessingSeenSince { since_ms: now_ms });
    }
}

fn current_unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Lock-free read of the runtime-toggle flag. Extracted so the unit
/// tests can exercise the same predicate without spawning the loop.
fn should_run_tick(enabled: &Arc<AtomicBool>) -> bool {
    enabled.load(Ordering::Relaxed)
}

// =============================================================================
// Phase 4 — auto-review trigger
// =============================================================================

/// Identifier the scheduler uses as the reviewer's `reviewer_session_id`.
/// Stable across the runner's lifetime; passes the `/reviews` self-review
/// check as long as no worker session takes the same id.
const SCHEDULER_REVIEWER_ID: &str = "rust-auto-reviewer";

/// Find tasks in `review` status that don't yet have a `reviews` row and
/// fire `auto_review_in_product` for each (off the iteration thread). The
/// function returns immediately — review work happens in detached tokio
/// tasks so the next scheduler tick isn't held up by the LLM call.
///
/// Idempotency: we check `get_reviews_for_task` before dispatching. After
/// the LLM finishes the row exists and subsequent ticks no-op for the
/// same task.
async fn maybe_trigger_auto_reviews(state: &Arc<ApiState>) {
    let pg = state.app_state.pg_db.clone();

    let tasks = match pg.list_tasks_by_status(&["review"]).await {
        Ok(t) => t,
        Err(e) => {
            warn!("auto_review trigger: list_tasks_by_status failed: {}", e);
            return;
        }
    };

    if tasks.is_empty() {
        return;
    }

    let runner_port = crate::mcp::types::runner_api_port(&state.app_state);

    for task in tasks {
        // Skip if a verdict already exists for this task — reviews are
        // additive (re-review on `needs_fix`), but the scheduler shouldn't
        // double-fire while a verdict is in flight.
        match pg.get_reviews_for_task(&task.id).await {
            Ok(rows) if !rows.is_empty() => continue,
            Err(e) => {
                warn!(
                    "auto_review trigger: get_reviews_for_task({}) failed: {}; skipping",
                    task.id, e
                );
                continue;
            }
            _ => {}
        }

        // Skip when no assigned worker — there's nothing to review.
        if task.assigned_session_id.is_none() {
            debug!(
                "auto_review trigger: task {} has no assigned session; skipping",
                task.id
            );
            continue;
        }

        let pg_clone = pg.clone();
        let task_id = task.id.clone();
        let app_handle = state.app_handle.clone();

        // Detached spawn — review work shouldn't block the next tick.
        // OneshotError::Disabled is treated as a quiet skip (no stub row
        // from the scheduler path; the manual Tauri command handles UX).
        async_runtime::spawn(async move {
            let llm = crate::ai_provider::oneshot::oneshot_for_settings();
            match auto_review_in_product(
                &pg_clone,
                llm.as_ref(),
                runner_port,
                &task_id,
                SCHEDULER_REVIEWER_ID,
            )
            .await
            {
                Ok(result) => {
                    info!(
                        "coordinator.auto_review_fired task={} verdict={} confidence={:.2}",
                        task_id, result.verdict, result.confidence
                    );
                    // Match the HTTP /reviews emit so ReviewBadge picks
                    // it up without polling.
                    let _ = tauri::Emitter::emit(
                        &app_handle,
                        "review-completed",
                        serde_json::json!({
                            "id": result.review_id,
                            "taskId": task_id,
                            "reviewerSessionId": SCHEDULER_REVIEWER_ID,
                            "verdict": result.verdict,
                            "confidence": result.confidence,
                        }),
                    );
                }
                Err(ReviewError::LlmDisabled(msg)) => {
                    debug!(
                        "auto_review trigger: LLM unavailable for task {}: {} (skipping; manual review path will queue stub)",
                        task_id, msg
                    );
                }
                Err(other) => {
                    warn!("auto_review trigger: task {} failed: {}", task_id, other);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::observe::LiveSession;

    fn make_session(id: &str, state: SessionState) -> LiveSession {
        LiveSession {
            task_run_id: id.to_string(),
            state: Some(state),
            assigned_task_id: None,
        }
    }

    #[test]
    fn refresh_tracker_inserts_new_processing_sessions() {
        let mut tracker = HashMap::new();
        let sessions = vec![make_session("s1", SessionState::Processing)];
        refresh_processing_tracker(&mut tracker, &sessions, 1234);
        assert_eq!(tracker.get("s1").map(|p| p.since_ms), Some(1234));
    }

    #[test]
    fn refresh_tracker_keeps_existing_timestamps() {
        let mut tracker = HashMap::new();
        tracker.insert("s1".to_string(), ProcessingSeenSince { since_ms: 500 });
        let sessions = vec![make_session("s1", SessionState::Processing)];
        refresh_processing_tracker(&mut tracker, &sessions, 999);
        assert_eq!(tracker.get("s1").map(|p| p.since_ms), Some(500));
    }

    #[test]
    fn refresh_tracker_evicts_when_no_longer_processing() {
        let mut tracker = HashMap::new();
        tracker.insert("s1".to_string(), ProcessingSeenSince { since_ms: 500 });
        let sessions = vec![make_session("s1", SessionState::Ready)];
        refresh_processing_tracker(&mut tracker, &sessions, 999);
        assert!(tracker.is_empty());
    }

    // ------------------------------------------------------------------
    // Phase 1.5 — runtime-toggle behaviour
    // ------------------------------------------------------------------

    #[test]
    fn handle_initial_enabled_round_trips() {
        let h = CoordinatorSchedulerHandle::new_for_test(false);
        assert!(!h.is_enabled());

        let h_on = CoordinatorSchedulerHandle::new_for_test(true);
        assert!(h_on.is_enabled());
    }

    #[test]
    fn handle_set_enabled_returns_previous_value() {
        let h = CoordinatorSchedulerHandle::new_for_test(false);
        let prev = h.set_enabled(true);
        assert!(!prev, "swapping false→true should report previous=false");
        assert!(h.is_enabled());

        let prev2 = h.set_enabled(true);
        assert!(prev2, "swapping true→true should report previous=true");
        assert!(h.is_enabled());

        let prev3 = h.set_enabled(false);
        assert!(prev3, "swapping true→false should report previous=true");
        assert!(!h.is_enabled());
    }

    #[test]
    fn handle_clones_share_atomic() {
        // Cloning the handle must hand out a view of the SAME
        // AtomicBool — otherwise the launch command's flip wouldn't be
        // visible to the loop. Keeping this regression test.
        let h1 = CoordinatorSchedulerHandle::new_for_test(false);
        let h2 = h1.clone();
        assert!(!h2.is_enabled());

        h1.set_enabled(true);
        assert!(h2.is_enabled(), "clone must observe sibling's flip");

        h2.set_enabled(false);
        assert!(!h1.is_enabled(), "original must observe clone's flip");
    }

    #[test]
    fn should_run_tick_reflects_atomic_state() {
        let flag = Arc::new(AtomicBool::new(false));
        assert!(!should_run_tick(&flag));

        flag.store(true, Ordering::Relaxed);
        assert!(should_run_tick(&flag));

        flag.store(false, Ordering::Relaxed);
        assert!(!should_run_tick(&flag));
    }
}
