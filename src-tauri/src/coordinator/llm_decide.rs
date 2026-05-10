//! Phase 2 of the productivity-coordinator-rust-promotion plan: LLM
//! callout for ambiguous cases the cheap rules A–E didn't fire on.
//!
//! The cheap rules walk the observation snapshot first; if none fire and
//! there's still pending/ready/escalated work the scheduler asks the warm
//! Claude provider for a single `CoordinatorAction`, then routes the
//! returned action through the same `coordinator::act::apply` path the
//! cheap rules use. This module owns:
//!
//! - The structured-output schema declaring the 11-variant tagged union.
//! - The system-prefix prompt (cache-friendly stable text).
//! - The per-iteration user message (compact JSON of observation + last 20
//!   decisions).
//! - The hourly `LlmBudget` slider that bounds runaway spend.
//!
//! Reference call template: `step_output/script_emitter.rs:509-535`. The
//! warm provider is blocking (`reqwest::blocking::Client`) per
//! `CLAUDE.md`, so we wrap it in `tauri::async_runtime::spawn_blocking`.
//!
//! `None` from `maybe_decide_with_llm` means "fall through to the
//! AdviseWithText 'no clear plan' row" — covers no warm credentials,
//! budget exhausted, malformed tool-call output, or unknown action `type`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::ai_provider::claude_api_warm;
use crate::database::pg::completion_reports::{CompletionReport, CompletionSource};
use crate::database::pg::coordinator_decisions::CoordinatorDecisionRow;
use crate::mcp::coordinator::CoordinatorAction;
use crate::mcp::types::ApiState;

use super::decide::DecideOutcome;
use super::observe::Observation;

/// Schema name surfaced to the warm provider's tool-call API. Stable so
/// the warm provider's prompt cache can key off it.
const SCHEMA_NAME: &str = "coordinator_action_v1";

/// Tight token cap — the LLM emits one of 11 short JSON objects.
const MAX_TOKENS: u32 = 512;

/// Default model for the LLM callout. Mirrors the script-emitter's
/// default (`claude-haiku-4-5-20251001`); cheaper and faster than Sonnet
/// for this pick-one-of-eleven decision.
const DEFAULT_MODEL: &str = "claude-haiku-4-5-20251001";

/// Per-tick recent-decisions window the model gets as context.
const RECENT_DECISIONS_WINDOW: usize = 20;

/// Phase 5 upstream-reports cap for the LLM-branch consultation
/// (productivity-coordinator-completion-reports §4 "LLM-branch
/// consultation"). When a pending task is gated by upstreams, we surface up
/// to this many of the most-recently-completed upstream `completion_report`
/// payloads to the model so it can cite specific `breakingChanges[*].area`
/// values when escalating or recommending an override. Bounded by the
/// existing per-iteration token cap.
const MAX_UPSTREAM_REPORTS_PER_TASK: usize = 5;

// ============================================================================
// Hourly budget
// ============================================================================

/// Sliding-hour LLM call budget. Best-effort — the inner `try_consume`
/// is non-atomic, but the scheduler holds it under a `tokio::sync::Mutex`
/// so concurrent LLM-callout attempts from a single instance can't burst
/// past the cap. Race conditions across runner instances aren't worth a
/// distributed counter; the lease semantics already serialize work.
#[derive(Debug)]
pub(crate) struct LlmBudget {
    calls_in_window: u32,
    window_start: Instant,
}

impl LlmBudget {
    pub(crate) fn new() -> Self {
        Self {
            calls_in_window: 0,
            window_start: Instant::now(),
        }
    }

    /// Slide the hour window if needed and try to consume one call. Returns
    /// `true` if the call fits inside the cap, `false` otherwise.
    pub(crate) fn try_consume(&mut self, max_per_hour: u32) -> bool {
        if max_per_hour == 0 {
            return false;
        }
        let now = Instant::now();
        if now.duration_since(self.window_start) >= Duration::from_secs(3600) {
            self.window_start = now;
            self.calls_in_window = 0;
        }
        if self.calls_in_window >= max_per_hour {
            return false;
        }
        self.calls_in_window += 1;
        true
    }

    /// For unit tests only — pin the window start to a known instant.
    #[cfg(test)]
    pub(crate) fn with_window_start(window_start: Instant) -> Self {
        Self {
            calls_in_window: 0,
            window_start,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_window_start(&mut self, window_start: Instant) {
        self.window_start = window_start;
    }

    #[cfg(test)]
    pub(crate) fn calls_in_window(&self) -> u32 {
        self.calls_in_window
    }
}

// ============================================================================
// Public entry point
// ============================================================================

/// Ask the warm provider for a single `CoordinatorAction`. Returns
/// `None` on disabled (no warm credentials), budget exhausted, transport
/// failure, or schema-rejected payload. The scheduler treats `None` as
/// "fall through to AdviseWithText 'no clear plan'".
pub(crate) async fn maybe_decide_with_llm(
    state: &Arc<ApiState>,
    observation: &Observation,
    last_decisions: &[CoordinatorDecisionRow],
    budget: &mut LlmBudget,
    max_per_hour: u32,
) -> Option<DecideOutcome> {
    if !budget.try_consume(max_per_hour) {
        info!(
            "coordinator.llm_callout_skipped reason=budget_exhausted max_per_hour={}",
            max_per_hour
        );
        return None;
    }

    info!("coordinator.llm_callout_started");

    let claude_api_settings = crate::settings::get_ai_settings().claude_api;
    let schema = build_schema();
    let system_prefix = build_system_prefix();

    // Phase 5 productivity-coordinator-completion-reports §4: gather upstream
    // completion reports for any pending task whose deps include at least one
    // upstream with a non-null `completion_report`. The model uses these to
    // cite specific `breakingChanges[*].area` /
    // `followUps[*].blockingForDependents` values in its reasoning.
    //
    // Best-effort: a PG hiccup just means the LLM gets the prompt without the
    // reports section, which is the same behavior as before this phase.
    let upstream_report_section = build_upstream_reports_section(state, observation).await;
    let user_message = build_user_message(observation, last_decisions, &upstream_report_section);

    let model = if claude_api_settings.model.is_empty() {
        DEFAULT_MODEL.to_string()
    } else {
        claude_api_settings.model.clone()
    };

    // The warm path uses `reqwest::blocking::Client`; bounce off the
    // tokio reactor for the 5–60s round-trip. (Reference template:
    // `script_emitter.rs:302-315`.)
    let join_result = tauri::async_runtime::spawn_blocking(move || {
        claude_api_warm::run_claude_api_warm_with_structured_output(
            &system_prefix,
            &user_message,
            &claude_api_settings,
            Some(&model),
            None,      // no doctor handle — short synthetic call
            Some(0.0), // deterministic
            Some(MAX_TOKENS),
            &schema,
            SCHEMA_NAME,
        )
    })
    .await;

    let response = match join_result {
        Ok(r) => r,
        Err(e) => {
            warn!("coordinator.llm_callout_join_error: {}", e);
            return None;
        }
    };

    if claude_api_warm::is_no_credential_error(&response) {
        info!(
            "coordinator.llm_callout_skipped reason=no_warm_credentials \
             (Phase 2 falls through to AdviseWithText)"
        );
        return None;
    }

    if !response.success {
        warn!(
            "coordinator.llm_callout_failed: {}",
            response.error.as_deref().unwrap_or("<no error>")
        );
        return None;
    }

    match parse_coordinator_action(&response.output) {
        Ok(action) => {
            info!(
                "coordinator.llm_callout_succeeded action={}",
                action_type_tag(&action)
            );
            Some(DecideOutcome {
                rule: "LLM",
                action,
            })
        }
        Err(e) => {
            warn!(
                "coordinator.llm_callout_parse_error: {} payload_len={}",
                e,
                response.output.len()
            );
            None
        }
    }
}

// ============================================================================
// Schema + parsing
// ============================================================================

/// Build the JSON schema describing `CoordinatorAction` as a tagged
/// union on `type`. Matches the wire shape from `mcp/coordinator.rs:140-210`.
fn build_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "type": {
                "type": "string",
                "enum": [
                    "assign-task",
                    "pause-session",
                    "merge-task",
                    "reassign-needs-fix",
                    "escalate",
                    "kill-session",
                    "force-promote-to-worktree",
                    "cancel-task",
                    "advise-with-text",
                    "escalate-with-text",
                    "idle-no-action"
                ]
            },
            "taskId": { "type": "string" },
            "sessionId": { "type": "string" },
            "targetId": { "type": "string" },
            "reasoning": { "type": "string" },
            "reason": { "type": "string" }
        },
        "required": ["type"]
    })
}

/// The 11 known variant tags — anything else means the model invented a
/// new action. Reject those upstream rather than letting them reach
/// `act::apply`.
const KNOWN_ACTION_TAGS: &[&str] = &[
    "assign-task",
    "pause-session",
    "merge-task",
    "reassign-needs-fix",
    "escalate",
    "kill-session",
    "force-promote-to-worktree",
    "cancel-task",
    "advise-with-text",
    "escalate-with-text",
    "idle-no-action",
];

/// Parse the warm provider's tool-call payload into a `CoordinatorAction`.
/// Returns `Err` on missing/unknown `type`, or any serde failure.
fn parse_coordinator_action(payload: &str) -> Result<CoordinatorAction, String> {
    let value: Value =
        serde_json::from_str(payload).map_err(|e| format!("payload is not valid JSON: {}", e))?;

    let tag = value
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or_else(|| "missing 'type' tag".to_string())?;

    if !KNOWN_ACTION_TAGS.contains(&tag) {
        return Err(format!("unknown action type: {:?}", tag));
    }

    serde_json::from_value::<CoordinatorAction>(value)
        .map_err(|e| format!("CoordinatorAction deserialize: {}", e))
}

fn action_type_tag(action: &CoordinatorAction) -> &'static str {
    match action {
        CoordinatorAction::AssignTask { .. } => "assign-task",
        CoordinatorAction::PauseSession { .. } => "pause-session",
        CoordinatorAction::MergeTask { .. } => "merge-task",
        CoordinatorAction::ReassignNeedsFix { .. } => "reassign-needs-fix",
        CoordinatorAction::Escalate { .. } => "escalate",
        CoordinatorAction::KillSession { .. } => "kill-session",
        CoordinatorAction::ForcePromoteToWorktree { .. } => "force-promote-to-worktree",
        CoordinatorAction::CancelTask { .. } => "cancel-task",
        CoordinatorAction::AdviseWithText { .. } => "advise-with-text",
        CoordinatorAction::EscalateWithText { .. } => "escalate-with-text",
        CoordinatorAction::ForceFlipReadyDespiteBlocker { .. } => {
            "force-flip-ready-despite-blocker"
        }
        CoordinatorAction::IdleNoAction => "idle-no-action",
    }
}

// ============================================================================
// Prompt assembly
// ============================================================================

/// Cache-friendly stable system prefix. Holds the role description, the
/// 11-variant catalog, and disambiguation rules. The warm provider's
/// `min_cacheable_chars(model)` heuristic tags this with `cache_control:
/// ephemeral` automatically when the body crosses the model's threshold.
fn build_system_prefix() -> String {
    String::from(
        r#"You are the dispatch decider for the qontinui Coordinator. The Coordinator runs in-process inside a developer-tools runner and watches a small fleet of Claude Code worker sessions completing software-engineering tasks. Cheap deterministic rules (A through E) already ran this iteration and none fired. Your job: pick exactly one CoordinatorAction.

The runner observes:
- tasks: pending/ready/assigned/review/escalated/needs_fix tasks across one or more plans
- live_sessions: Claude Code workers, each in state Ready (idle) or Processing (working)
- file claims: paths a session is editing (active) or has reserved for a future task (upcoming)
- recent_reviews: 1-hour window of completed code reviews with verdicts approved | needs_fix | escalate
- open_escalations: prior coordinator decisions that flipped auto_acted=false and have not been resolved
- recent_decisions: last 20 coordinator_decisions rows so you can avoid loops

You emit ONE action by calling the coordinator_action_v1 tool. The "type" tag picks the variant; the variant determines which fields are required:

1. {"type": "assign-task", "taskId": "...", "sessionId": "...", "reasoning": "..."}
   Assign a ready task to an idle session. Use this when the cheap Rule B somehow missed (e.g. the task and session both showed up mid-tick).

2. {"type": "pause-session", "sessionId": "...", "reason": "..."}
   Stop a session from picking up new work. Use sparingly — only when the session is on a problematic path and freeing it would help the user.

3. {"type": "merge-task", "taskId": "...", "reasoning": "..."}
   Mark a task done after a successful review. Cheap Rule D handles the >=0.85-confidence case; this variant is for edge cases where the review threshold logic should be bypassed.

4. {"type": "reassign-needs-fix", "taskId": "...", "reasoning": "..."}
   Re-send a needs_fix task to its worker with reviewer feedback. Use when Rule D's 3-retry cap hasn't been reached and you have a reason to retry.

5. {"type": "escalate", "targetId": "...", "reasoning": "..."}
   Mark the target as needing user attention. Decision row gets auto_acted=false so the dashboard surfaces it. Use for hard problems with a specific target.

6. {"type": "kill-session", "sessionId": "...", "reasoning": "..."}
   Destructive — terminate a worker session. Decision row gets auto_acted=false; user must approve.

7. {"type": "force-promote-to-worktree", "taskId": "...", "reasoning": "..."}
   Destructive — force a task into worktree-isolated execution mode. auto_acted=false.

8. {"type": "cancel-task", "taskId": "...", "reasoning": "..."}
   Destructive — abandon a task without merging. auto_acted=false; rare.

9. {"type": "advise-with-text", "targetId": "?", "reasoning": "..."}
   Surface a free-form note in the dashboard's Advisories panel. Non-destructive. targetId may be omitted when the advisory has no specific target.

10. {"type": "escalate-with-text", "targetId": "?", "reasoning": "..."}
    Same as advise-with-text but flips auto_acted=false so the user must explicitly resolve. Use when the model needs the user's attention without a clear automated next step.

11. {"type": "idle-no-action", "reasoning": "..."}
    Nothing to do this iteration. Choose this when no other action is clearly correct.

Decision rules:
- Prefer non-destructive actions (advise-with-text, escalate-with-text, idle-no-action) when in doubt.
- Never assign a task to a session whose touched files overlap the task's expected_file_claims — Rule B already enforces this; if you're considering an assign, double-check overlap.
- Never re-issue an action that's already in the recent_decisions window for the same target — that's a loop.
- If no action is clearly correct, return idle-no-action with a brief reasoning.
- Reasoning should be one short sentence; the dashboard renders it verbatim.
"#,
    )
}

/// Phase 5 helper: for each pending task in the observation, look up its
/// upstream `completion_report` payloads (if any) and render them into a
/// Markdown section the LLM prompt can consume. Bounded by
/// `MAX_UPSTREAM_REPORTS_PER_TASK` per task — when a task has more
/// upstreams, we keep the most-recently-completed five and add a
/// `(N more upstreams omitted from prompt)` note.
///
/// Returns an empty string when no pending task has any upstream reports —
/// callers append the section unconditionally and an empty section drops
/// out at concat time.
async fn build_upstream_reports_section(
    state: &Arc<ApiState>,
    observation: &Observation,
) -> String {
    let pg = &state.app_state.pg_db;

    // Find the pending tasks that have at least one upstream id.
    let pending_with_deps: Vec<&crate::database::pg::tasks::TaskRow> = observation
        .tasks
        .iter()
        .filter(|t| t.status == "pending" && !t.depends_on.is_empty())
        .collect();
    if pending_with_deps.is_empty() {
        return String::new();
    }

    let mut sections: Vec<String> = Vec::new();
    for task in pending_with_deps {
        // Resolve upstream rows so we can sort by completed_at desc.
        let mut upstream_rows: Vec<crate::database::pg::tasks::TaskRow> = Vec::new();
        for dep_id in &task.depends_on {
            match pg.get_task_by_id(dep_id).await {
                Ok(Some(t)) => upstream_rows.push(t),
                Ok(None) => continue,
                Err(e) => {
                    debug!(
                        "build_upstream_reports_section: get_task_by_id({}) failed: {}",
                        dep_id, e
                    );
                    continue;
                }
            }
        }

        // Most-recent first by completed_at (string-sortable since we store
        // ISO 8601 timestamps; nulls float to the end).
        upstream_rows.sort_by(|a, b| b.completed_at.cmp(&a.completed_at));

        let total = upstream_rows.len();
        let kept: Vec<&crate::database::pg::tasks::TaskRow> = upstream_rows
            .iter()
            .take(MAX_UPSTREAM_REPORTS_PER_TASK)
            .collect();

        // Pull each upstream's completion_report.
        let mut rendered_upstreams: Vec<String> = Vec::new();
        for upstream in &kept {
            let pair: Option<(CompletionReport, CompletionSource)> =
                match pg.get_completion_report(&upstream.id).await {
                    Ok(p) => p,
                    Err(e) => {
                        debug!(
                            "build_upstream_reports_section: get_completion_report({}) failed: {}",
                            upstream.id, e
                        );
                        None
                    }
                };
            let (report, source) = match pair {
                Some(p) => p,
                None => continue,
            };
            // Compact JSON of the typed fields the model is expected to cite.
            // Skip artifacts — those are open-ended source-specific blobs and
            // would burn tokens.
            let payload = json!({
                "upstreamTaskId": upstream.id,
                "completedAt": upstream.completed_at,
                "completionSource": source.as_str(),
                "summaryMd": report.summary_md,
                "deliverables": report.deliverables.iter().map(|d| json!({
                    "kind": d.kind,
                    "reference": d.reference,
                    "description": d.description,
                })).collect::<Vec<_>>(),
                "breakingChanges": report.breaking_changes.iter().map(|bc| json!({
                    "area": bc.area,
                    "description": bc.description,
                })).collect::<Vec<_>>(),
                "followUps": report.follow_ups.iter().map(|fu| json!({
                    "description": fu.description,
                    "priority": fu.priority,
                    "blockingForDependents": fu.blocking_for_dependents,
                })).collect::<Vec<_>>(),
            });
            rendered_upstreams.push(payload.to_string());
        }

        if rendered_upstreams.is_empty() {
            continue;
        }

        let omitted = total.saturating_sub(MAX_UPSTREAM_REPORTS_PER_TASK);
        let mut block = String::new();
        block.push_str(&format!(
            "Pending task {} has {} upstream(s). Showing the {} most-recent with completion_report:\n",
            task.id,
            total,
            rendered_upstreams.len()
        ));
        for r in rendered_upstreams {
            block.push_str(&r);
            block.push('\n');
        }
        if omitted > 0 {
            block.push_str(&format!(
                "({} more upstreams omitted from prompt)\n",
                omitted
            ));
        }
        sections.push(block);
    }

    if sections.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str(
        "\n\nUpstream completion reports (for pending tasks with completed deps):\n\
         The fields breakingChanges[*].area and followUps[*].blockingForDependents are \
         load-bearing — when escalating or recommending an override, cite them by \
         area / description.\n\n",
    );
    for s in sections {
        out.push_str(&s);
        out.push('\n');
    }
    out
}

/// Compact per-iteration user message: observation snapshot + recent
/// decisions. Stays small so cache-keying stays cheap.
fn build_user_message(
    observation: &Observation,
    last_decisions: &[CoordinatorDecisionRow],
    upstream_reports_section: &str,
) -> String {
    // Trim the live-session and decision sets to the columns the model
    // actually needs. Drops the verbose stuff (full reasoning prose, IDs,
    // serialized sub-objects) that would burn tokens without helping the
    // pick.
    let tasks: Vec<Value> = observation
        .tasks
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "planId": t.plan_id,
                "phaseName": t.phase_name,
                "status": t.status,
                "expectedFileClaims": t.expected_file_claims,
                "dependsOn": t.depends_on,
                "assignedSessionId": t.assigned_session_id,
            })
        })
        .collect();

    let live_sessions: Vec<Value> = observation
        .live_sessions
        .iter()
        .map(|ls| {
            json!({
                "taskRunId": ls.task_run_id,
                "state": ls.state.map(|s| format!("{:?}", s)),
                "assignedTaskId": ls.assigned_task_id,
            })
        })
        .collect();

    let active_claims: Vec<Value> = observation
        .active_file_registry
        .iter()
        .map(|fr| {
            json!({
                "filePath": fr.file_path,
                "holderTaskRunId": fr.holder_task_run_id,
                "holderName": fr.holder_name,
            })
        })
        .collect();

    let upcoming_claims: Vec<Value> = observation
        .upcoming_file_registry
        .iter()
        .map(|c| {
            json!({
                "planId": c.plan_id,
                "taskId": c.task_id,
                "path": c.path,
            })
        })
        .collect();

    let recent_reviews: Vec<Value> = observation
        .recent_reviews
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "taskId": r.task_id,
                "verdict": r.verdict,
                "confidence": r.confidence,
                "createdAt": r.created_at,
            })
        })
        .collect();

    let open_escalations: Vec<Value> = observation
        .open_escalations
        .iter()
        .map(|d| {
            json!({
                "targetId": d.target_id,
                "action": d.action,
                "createdAt": d.created_at,
            })
        })
        .collect();

    let recent_decisions: Vec<Value> = last_decisions
        .iter()
        .take(RECENT_DECISIONS_WINDOW)
        .map(|d| {
            json!({
                "targetId": d.target_id,
                "action": d.action,
                "rule": d.rule,
                "autoActed": d.auto_acted,
                "createdAt": d.created_at,
            })
        })
        .collect();

    let observation_json = json!({
        "tasks": tasks,
        "liveSessions": live_sessions,
        "activeFileClaims": active_claims,
        "upcomingFileClaims": upcoming_claims,
        "recentReviews": recent_reviews,
        "openEscalations": open_escalations,
        "sessionTouchedFiles": observation.session_touched_files,
    });

    let decisions_json = json!(recent_decisions);

    let mut buf = String::with_capacity(2048);
    buf.push_str("Observation snapshot (compact):\n");
    buf.push_str(&observation_json.to_string());
    buf.push_str("\n\nLast ");
    buf.push_str(&RECENT_DECISIONS_WINDOW.to_string());
    buf.push_str(" coordinator decisions:\n");
    buf.push_str(&decisions_json.to_string());
    if !upstream_reports_section.is_empty() {
        buf.push_str(upstream_reports_section);
    }
    buf.push_str("\n\nWhat action should the coordinator take this iteration?");

    debug!(
        "coordinator.llm_user_message_built tasks={} sessions={} decisions={} bytes={}",
        observation.tasks.len(),
        observation.live_sessions.len(),
        recent_decisions.len(),
        buf.len()
    );

    buf
}

// ============================================================================
// Tests
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // --- LlmBudget --------------------------------------------------------

    #[test]
    fn budget_zero_cap_rejects() {
        let mut b = LlmBudget::new();
        assert!(!b.try_consume(0));
    }

    #[test]
    fn budget_consumes_up_to_cap() {
        let mut b = LlmBudget::new();
        for _ in 0..3 {
            assert!(b.try_consume(3), "first 3 calls should fit");
        }
        assert!(!b.try_consume(3), "4th call should be rejected");
        assert_eq!(b.calls_in_window(), 3);
    }

    #[test]
    fn budget_slides_window_after_one_hour() {
        // Pin the window start to ~61 minutes ago so the next try_consume
        // detects the slide. On Windows CI runners the system Instant base
        // can be very recent (boot time minutes ago) so direct subtraction
        // panics with "overflow when subtracting duration from instant" —
        // skip the test instead of panicking when that happens.
        let past = match Instant::now().checked_sub(Duration::from_secs(61 * 60)) {
            Some(p) => p,
            None => {
                eprintln!(
                    "skip budget_slides_window_after_one_hour: Instant base \
                     too recent for 61-min lookback (system uptime < 61min)"
                );
                return;
            }
        };
        let mut b = LlmBudget::with_window_start(past);
        // Pretend we already filled the cap in the prior window.
        for _ in 0..5 {
            // Direct mutation isn't exposed; call try_consume against a
            // big-enough cap to fill the counter, then assert the slide.
            let _ = b.try_consume(10);
        }
        assert_eq!(b.calls_in_window(), 5);
        // Reset the window start to 61 min ago again — try_consume just
        // updated `window_start` to "now".
        b.set_window_start(past);
        // Next call should slide the window and reset to 1.
        assert!(b.try_consume(3));
        assert_eq!(b.calls_in_window(), 1);
    }

    #[test]
    fn budget_does_not_slide_within_one_hour() {
        let recent = Instant::now() - Duration::from_secs(30 * 60);
        let mut b = LlmBudget::with_window_start(recent);
        assert!(b.try_consume(2));
        assert!(b.try_consume(2));
        assert!(!b.try_consume(2));
        assert_eq!(b.calls_in_window(), 2);
    }

    // --- Schema parsing ---------------------------------------------------

    #[test]
    fn parser_accepts_assign_task() {
        let payload =
            r#"{"type":"assign-task","taskId":"t-1","sessionId":"s-1","reasoning":"why"}"#;
        let action = parse_coordinator_action(payload).unwrap();
        match action {
            CoordinatorAction::AssignTask {
                task_id,
                session_id,
                reasoning,
            } => {
                assert_eq!(task_id, "t-1");
                assert_eq!(session_id, "s-1");
                assert_eq!(reasoning.as_deref(), Some("why"));
            }
            _ => panic!("expected AssignTask"),
        }
    }

    #[test]
    fn parser_accepts_idle_no_action() {
        let payload = r#"{"type":"idle-no-action"}"#;
        let action = parse_coordinator_action(payload).unwrap();
        assert!(matches!(action, CoordinatorAction::IdleNoAction));
    }

    #[test]
    fn parser_accepts_advise_with_text_without_target() {
        let payload = r#"{"type":"advise-with-text","reasoning":"note"}"#;
        let action = parse_coordinator_action(payload).unwrap();
        match action {
            CoordinatorAction::AdviseWithText {
                target_id,
                reasoning,
            } => {
                assert!(target_id.is_none());
                assert_eq!(reasoning, "note");
            }
            _ => panic!("expected AdviseWithText"),
        }
    }

    #[test]
    fn parser_rejects_unknown_type() {
        let payload = r#"{"type":"do-the-needful","taskId":"t-1"}"#;
        let err = parse_coordinator_action(payload).unwrap_err();
        assert!(err.contains("unknown action type"), "got: {}", err);
    }

    #[test]
    fn parser_rejects_missing_type() {
        let payload = r#"{"taskId":"t-1","sessionId":"s-1"}"#;
        let err = parse_coordinator_action(payload).unwrap_err();
        assert!(err.contains("missing 'type' tag"), "got: {}", err);
    }

    #[test]
    fn parser_rejects_invalid_json() {
        let payload = r#"not json at all"#;
        let err = parse_coordinator_action(payload).unwrap_err();
        assert!(err.contains("not valid JSON"), "got: {}", err);
    }

    #[test]
    fn parser_rejects_missing_required_field() {
        // assign-task without taskId — serde should fail.
        let payload = r#"{"type":"assign-task","sessionId":"s-1"}"#;
        let err = parse_coordinator_action(payload).unwrap_err();
        assert!(
            err.contains("CoordinatorAction deserialize"),
            "got: {}",
            err
        );
    }

    #[test]
    fn schema_lists_all_eleven_variants() {
        let schema = build_schema();
        let variants = schema["properties"]["type"]["enum"]
            .as_array()
            .expect("enum array");
        assert_eq!(variants.len(), 11);
        for tag in KNOWN_ACTION_TAGS {
            assert!(
                variants.iter().any(|v| v.as_str() == Some(*tag)),
                "schema missing tag {}",
                tag
            );
        }
    }

    // --- Phase 5: upstream completion reports in the LLM prompt ----------

    /// Helper: build a minimal Observation for `build_user_message` tests.
    fn empty_observation() -> super::super::observe::Observation {
        super::super::observe::Observation {
            tasks: Vec::new(),
            active_file_registry: Vec::new(),
            upcoming_file_registry: Vec::new(),
            live_sessions: Vec::new(),
            recent_reviews: Vec::new(),
            open_escalations: Vec::new(),
            session_touched_files: std::collections::HashMap::new(),
            recent_decisions: Vec::new(),
        }
    }

    #[test]
    fn build_user_message_appends_upstream_reports_section() {
        let obs = empty_observation();
        let decisions: Vec<CoordinatorDecisionRow> = Vec::new();
        let section = "\n\nUpstream completion reports (test marker):\nbody\n";
        let msg = build_user_message(&obs, &decisions, section);
        assert!(
            msg.contains("Upstream completion reports (test marker)"),
            "user message must include the supplied upstream reports section"
        );
        // The closing "What action..." line should still be there after the
        // section.
        assert!(msg.contains("What action should the coordinator take this iteration?"));
        // And the section must precede the closing question, not follow it.
        let section_idx = msg.find("(test marker)").unwrap();
        let question_idx = msg.find("What action").unwrap();
        assert!(section_idx < question_idx);
    }

    #[test]
    fn build_user_message_omits_empty_upstream_reports_section() {
        let obs = empty_observation();
        let decisions: Vec<CoordinatorDecisionRow> = Vec::new();
        let msg = build_user_message(&obs, &decisions, "");
        // No section heading anywhere when the section is empty.
        assert!(!msg.contains("Upstream completion reports"));
    }
}
