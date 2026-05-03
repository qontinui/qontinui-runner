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
//! and the loop body short-circuits. Default-off via
//! `CoordinatorSchedulerConfig::rust_scheduler_enabled` keeps that the
//! actual production state until Phase 2 ships.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::async_runtime;
use tokio::sync::Mutex;
use tokio::time::interval;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::claude_session::state::SessionState;
use crate::database::pg::coordinator_decisions::InsertCoordinatorDecisionInput;
use crate::mcp::coordinator::CoordinatorAction;
use crate::mcp::types::ApiState;

use super::act;
use super::config::CoordinatorSchedulerConfig;
use super::decide::{self, stamp_reasoning, DecideOutcome, ProcessingSeenSince, RULE_VERSION};
use super::llm_decide::{self, LlmBudget};
use super::observe::observe;

/// Start the scheduler. Returns the spawned task handle so the caller
/// can keep it alive. Mirrors `start_memory_scheduler`'s contract — the
/// loop runs forever and is dropped only when the runner exits.
pub fn start_coordinator_scheduler(
    state: Arc<ApiState>,
    config: CoordinatorSchedulerConfig,
) -> async_runtime::JoinHandle<()> {
    async_runtime::spawn(async move {
        // Initial delay matches the memory scheduler — let the runner
        // finish bootstrapping before we contend for the lease.
        tokio::time::sleep(Duration::from_secs(config.initial_delay_secs)).await;

        let instance_id = format!("rust-{}", Uuid::new_v4());
        info!(
            "Coordinator (Rust) scheduler started: instance_id={} interval={}s rule_version={} max_llm_calls_per_hour={}",
            instance_id,
            config.interval_secs,
            RULE_VERSION,
            config.max_llm_calls_per_hour
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
        }
    })
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
        // do the actual `pending → ready` flip. The slash command's
        // Rule E mentions "may also be handled server-side by a
        // trigger" — there is no such trigger today (verified by grep
        // of CREATE TRIGGER in this crate's PG schema), so we call
        // `mark_ready_for_unblocked` per plan id directly.
        if outcome.rule == "E" {
            if let Some(plan_id) = task_plan_for_target(&obs, target.as_deref()) {
                if let Err(e) = state
                    .app_state
                    .pg_db
                    .mark_ready_for_unblocked(&plan_id)
                    .await
                {
                    warn!("Rule E mark_ready_for_unblocked failed: {}", e);
                }
            }
        }
    }

    Ok(())
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
}
