//! Auto-act dispatcher and helpers for the productivity-stack Coordinator.
//!
//! Lifted from `crate::mcp::coordinator` (Phase 1a of the
//! `productivity-coordinator-rust-promotion` plan) so that both the HTTP
//! handler `POST /coordinator/act` and the in-process Rust scheduler
//! drive side effects through one code path. The
//! `CoordinatorAction`/`CoordinatorActRequest`/`CoordinatorActResponse`
//! wire types stay in `mcp::coordinator` next to the HTTP routes.
//!
//! Per productivity-stack §4 the auto-act vs advise-only boundary is
//! enforced server-side. `must_advise_only` lists the destructive set;
//! everything else runs through `apply` and ends up with `auto_acted =
//! true` on the persisted decision row.

use std::sync::Arc;

use axum::http::StatusCode;
use tauri::Manager;
use tracing::warn;

use crate::database::pg::completion_reports::CompletionReport;
use crate::database::pg::tasks::TaskRow;
use crate::mcp::coordinator::CoordinatorAction;
use crate::mcp::types::ApiState;

/// Returns the action's discriminator name (matches the `type` tag from
/// the wire format) so it can be persisted and queried alongside `rule`.
pub(crate) fn action_name(action: &CoordinatorAction) -> &'static str {
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

/// Per the auto-act boundary in productivity-stack §4, only the listed
/// destructive actions force an advisory-only outcome. Everything else is
/// cheap+reversible and runs auto-acted. `cancel-task` is in the
/// destructive set because v1 has no signal for "user-requested
/// cancellation" — the slash command escalates first.
///
/// `ForceFlipReadyDespiteBlocker` is forced advisory in the
/// productivity-coordinator-completion-reports plan §4 wording: the
/// Coordinator agent itself cannot fire it from a cheap rule. The user does
/// fire it explicitly via the dashboard, and the HTTP layer in
/// `mcp::coordinator::coordinator_act` runs the `apply` side effect for
/// user-fire actions even when `must_advise_only` is true (because the
/// HTTP POST itself IS the user-confirmed escalation). See
/// `is_user_fire_only_action` for the carve-out predicate.
///
/// Per coord-as-deconflicter plan §4.6, sessions whose `session_id` starts
/// with `"deconflicter-"` are restricted to `AdviseWithText` only — any
/// other action is gated (returns `true`) so the HTTP layer can reject it
/// with `403 FORBIDDEN`. For the deconflicter's own `AdviseWithText` the
/// function returns `false` so the audit row reads as auto-acted (advise IS
/// the primary action for the deconflicter, not an escalation).
pub(crate) fn must_advise_only(session_id: &str, action: &CoordinatorAction) -> bool {
    // Deconflicter sessions (Rust-driven, §4.6 of coord-as-deconflicter plan)
    // are restricted to AdviseWithText only — any other action returned true
    // here is rejected at the HTTP layer with 403.
    if session_id.starts_with("deconflicter-") {
        return !matches!(action, CoordinatorAction::AdviseWithText { .. });
    }
    matches!(
        action,
        CoordinatorAction::KillSession { .. }
            | CoordinatorAction::ForcePromoteToWorktree { .. }
            | CoordinatorAction::CancelTask { .. }
            | CoordinatorAction::Escalate { .. }
            | CoordinatorAction::EscalateWithText { .. }
            | CoordinatorAction::ForceFlipReadyDespiteBlocker { .. }
            | CoordinatorAction::IdleNoAction
    )
}

/// Subset of `must_advise_only` actions whose side effect MUST execute when
/// fired through the user-driven `/coordinator/act` HTTP endpoint. The
/// auto-act vs advise-only boundary holds for the Rust scheduler's own
/// rule fires, but a manual user fire of `force-flip-ready-despite-blocker`
/// has to actually flip the task — otherwise the user's click does nothing.
///
/// Other `must_advise_only` actions (KillSession, ForcePromote, CancelTask,
/// the bare Escalate variants) deliberately remain advisory-on-fire because
/// their actual side effects live in different surfaces (the Sessions panel
/// for kill, the worktree split UI for promote, etc.).
pub(crate) fn is_user_fire_only_action(action: &CoordinatorAction) -> bool {
    matches!(
        action,
        CoordinatorAction::ForceFlipReadyDespiteBlocker { .. }
    )
}

/// Returns the action's `target_id` — what the action operates on. Used
/// for the persisted decision row and for the escalation event payload.
pub(crate) fn action_target(action: &CoordinatorAction) -> Option<String> {
    match action {
        CoordinatorAction::AssignTask { task_id, .. } => Some(task_id.clone()),
        CoordinatorAction::PauseSession { session_id, .. } => Some(session_id.clone()),
        CoordinatorAction::MergeTask { task_id, .. } => Some(task_id.clone()),
        CoordinatorAction::ReassignNeedsFix { task_id, .. } => Some(task_id.clone()),
        CoordinatorAction::Escalate { target_id, .. } => Some(target_id.clone()),
        CoordinatorAction::KillSession { session_id, .. } => Some(session_id.clone()),
        CoordinatorAction::ForcePromoteToWorktree { task_id, .. } => Some(task_id.clone()),
        CoordinatorAction::CancelTask { task_id, .. } => Some(task_id.clone()),
        CoordinatorAction::AdviseWithText { target_id, .. } => target_id.clone(),
        CoordinatorAction::EscalateWithText { target_id, .. } => target_id.clone(),
        CoordinatorAction::ForceFlipReadyDespiteBlocker { task_id, .. } => Some(task_id.clone()),
        CoordinatorAction::IdleNoAction => None,
    }
}

pub(crate) fn action_reasoning(action: &CoordinatorAction) -> String {
    match action {
        CoordinatorAction::AssignTask {
            reasoning,
            task_id,
            session_id,
        } => reasoning
            .clone()
            .unwrap_or_else(|| format!("assign task {} → session {}", task_id, session_id)),
        CoordinatorAction::PauseSession { reason, .. } => reason.clone(),
        CoordinatorAction::MergeTask {
            reasoning, task_id, ..
        } => reasoning
            .clone()
            .unwrap_or_else(|| format!("merge task {}", task_id)),
        CoordinatorAction::ReassignNeedsFix { reasoning, .. }
        | CoordinatorAction::Escalate { reasoning, .. }
        | CoordinatorAction::KillSession { reasoning, .. }
        | CoordinatorAction::ForcePromoteToWorktree { reasoning, .. }
        | CoordinatorAction::CancelTask { reasoning, .. }
        | CoordinatorAction::AdviseWithText { reasoning, .. }
        | CoordinatorAction::EscalateWithText { reasoning, .. } => reasoning.clone(),
        CoordinatorAction::ForceFlipReadyDespiteBlocker {
            reasoning, task_id, ..
        } => reasoning
            .clone()
            .unwrap_or_else(|| format!("force-flip task {} despite blocking follow-up", task_id)),
        CoordinatorAction::IdleNoAction => "no cheap rule fired this iteration".to_string(),
    }
}

/// Apply a cheap-and-reversible action. Returns `Err` on irrecoverable
/// failure — that triggers an HTTP 500. Soft failures (e.g. session not
/// found) are logged and the row still persists with `auto_acted = true`,
/// since the *intent* was to auto-act.
pub(crate) async fn apply(
    state: &Arc<ApiState>,
    action: &CoordinatorAction,
) -> Result<(), (StatusCode, String)> {
    let pg = &state.app_state.pg_db;

    match action {
        CoordinatorAction::AssignTask {
            task_id,
            session_id,
            ..
        } => {
            // Idempotent — assign_task_to_session checks for `ready`/`needs_fix`
            // before flipping. We don't fail on `Ok(false)` because the agent
            // may be retrying; `Err` is reserved for real DB problems.
            pg.assign_task_to_session(task_id, session_id)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

            // productivity-coordinator-completion-reports §4 "Rule B
            // extension" / "Wiring note": after the DB flip, push the
            // structured assignment brief to the worker. Reads the upstream
            // reports stashed in `assignment_brief_extras` by Rule E (when
            // the task came in with deps-now-done) and prepends them to the
            // task description. Resume-from-pause variant detected via the
            // most-recent `WORKER_ADDED_DEPENDENCY` `coordinator_decisions`
            // row.
            //
            // Best-effort: any failure here is logged (the AssignTask
            // intent already succeeded at the DB layer) so a missing brief
            // doesn't fail the whole action. The user can re-fire from the
            // dashboard.
            if let Err(e) = dispatch_assignment_brief(state, task_id, session_id).await {
                warn!(
                    "AssignTask: brief dispatch failed for task {} session {}: {}",
                    task_id, session_id, e
                );
            }
        }
        CoordinatorAction::PauseSession { .. } => {
            // The pause side effect lives in the slash command (it has HTTP
            // affinity to the worker session). v1 logs the decision row and
            // returns; the agent issues the follow-up `/sessions/<id>/message`
            // POST. The decision row remains the source of truth.
        }
        CoordinatorAction::MergeTask { task_id, reasoning } => {
            // Per plan §4 Rule D: merge-task is the per-tab commit trigger.
            // We send the canonical commit prompt to the worker and flip the
            // task to `done`. The actual `git commit` runs in the worker's
            // own session (it owns the working tree); this endpoint just
            // requests it and records intent.
            //
            // Resolution path: latest review for the task → reviewed
            // session id → /sessions/<id>/message wrapper. We tolerate
            // "no review row" (returns Ok with no message sent — that's a
            // misuse the decision row will surface to the user).
            let latest_review = pg
                .get_reviews_for_task(task_id)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
                .into_iter()
                .next();

            // Flip the task's status to done. This is best-effort — the
            // worker may not yet have transitioned to `review`, in which
            // case we leave the row alone and the worker's own commit flow
            // picks up later.
            if let Err(e) = pg.transition_task_status(task_id, "review", "done").await {
                warn!(
                    "merge-task: transition_task_status review->done failed for {}: {}",
                    task_id, e
                );
            }

            if let Some(review) = latest_review {
                let prompt = format!(
                    "Please commit your work for task {} now. The review verdict is {} \
                     (confidence {:.2}). {}\n\nUse `git add -A && git commit -m \"<msg>\"` \
                     with a concise commit message that references the task.",
                    task_id,
                    review.verdict,
                    review.confidence,
                    reasoning.as_deref().unwrap_or("")
                );
                send_message_to_worker(state, &review.reviewed_session_id, &prompt).await;
            } else {
                warn!(
                    "merge-task for {} has no review rows; skipping commit prompt",
                    task_id
                );
            }
        }
        CoordinatorAction::ReassignNeedsFix { task_id, reasoning } => {
            // Per plan §4 Rule D: re-assign to the same worker with the
            // reviewer's `reasoning` as the new user message, prefixed
            // by the action's free-form `reasoning` (typically "retry N
            // of 3").
            let latest_review = pg
                .get_reviews_for_task(task_id)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
                .into_iter()
                .next();

            // Flip the task back to `assigned` so the worker's tooling
            // recognises it as live work. `review -> needs_fix -> assigned`
            // is the canonical retry path.
            if let Err(e) = pg
                .transition_task_status(task_id, "review", "needs_fix")
                .await
            {
                warn!(
                    "re-assign-needs-fix: transition review->needs_fix failed for {}: {}",
                    task_id, e
                );
            }
            if let Err(e) = pg
                .transition_task_status(task_id, "needs_fix", "assigned")
                .await
            {
                warn!(
                    "re-assign-needs-fix: transition needs_fix->assigned failed for {}: {}",
                    task_id, e
                );
            }

            if let Some(review) = latest_review {
                let prompt = format!(
                    "Reviewer feedback for task {} (verdict={}, confidence={:.2}):\n\n{}\n\n\
                     Coordinator note: {}\n\nPlease address the items above and resume work.",
                    task_id, review.verdict, review.confidence, review.reasoning, reasoning,
                );
                send_message_to_worker(state, &review.reviewed_session_id, &prompt).await;
            } else {
                warn!(
                    "re-assign-needs-fix for {} has no review rows; skipping reviewer feedback",
                    task_id
                );
            }
        }
        CoordinatorAction::AdviseWithText { .. } => {
            // The advisory IS the action. Persistence + the
            // `coordinator-advice` event the dispatcher emits below are the
            // only side effects; nothing on disk changes. Keeps `auto_acted
            // = true` so the row reads as a completed move in the audit
            // trail (per Phase 5 spec).
        }
        CoordinatorAction::ForceFlipReadyDespiteBlocker { task_id, .. } => {
            // User explicitly chose to override Rule E's blocking-follow-up
            // veto for this task. Flip pending → ready and let the next
            // Rule B tick re-assign with the upstream brief (which still
            // includes the blocker context, so the worker is informed).
            //
            // Per `must_advise_only` this variant returns `true`, but the
            // HTTP path in `mcp::coordinator::coordinator_act` carves it
            // out via `is_user_fire_only_action` so this branch executes
            // when a user-fired POST lands. The cheap-rules scheduler
            // never reaches this arm because no rule emits the variant.
            match pg.transition_task_status(task_id, "pending", "ready").await {
                Ok(true) => {}
                Ok(false) => {
                    warn!(
                        "force-flip-ready-despite-blocker: no-op (task {} not in pending)",
                        task_id
                    );
                }
                Err(e) => {
                    warn!(
                        "force-flip-ready-despite-blocker: transition pending->ready failed for {}: {}",
                        task_id, e
                    );
                }
            }
        }
        CoordinatorAction::IdleNoAction
        | CoordinatorAction::Escalate { .. }
        | CoordinatorAction::KillSession { .. }
        | CoordinatorAction::ForcePromoteToWorktree { .. }
        | CoordinatorAction::CancelTask { .. }
        | CoordinatorAction::EscalateWithText { .. } => {
            // These never reach apply (must_advise_only returns true), but
            // the match arm satisfies exhaustiveness without a catch-all
            // that could mask new variants.
        }
    }
    Ok(())
}

// =============================================================================
// Assignment-brief composer (productivity-coordinator-completion-reports §4)
// =============================================================================

/// Pure-function composer for the worker's first-message assignment brief.
///
/// Consumed by the `AssignTask` apply branch above (and by the slash command
/// path indirectly — see `coordinate.md` Rule B). Renders upstream completion
/// reports into the Markdown layout from
/// productivity-coordinator-completion-reports.md §4 "Rule B extension":
///
/// ```markdown
/// ## Upstream completion reports
///
/// <N> upstream tasks shipped before this one. …
///
/// ### Upstream task <id> — <description first line>
///
/// <upstream completion_report.summary_md verbatim>
///
/// **Deliverables:**
/// - [pr] <reference> — <description>
/// …
///
/// **Breaking changes:**
/// - **<area>**: <description>
///   <migration_steps_md inset>
///
/// **Open follow-ups (none blocking):**
/// - <description> [<priority>]
/// …
///
/// ---
///
/// <repeat for each upstream>
///
/// ---
///
/// ## Your task
///
/// <resume_prefix line if present>
/// <task description>
/// ```
///
/// Edge cases:
/// - `upstreams` empty → returns just the task description (no brief block).
///   The `## Your task` heading is omitted in that case so a downstream task
///   with no deps still reads as a normal first-message body.
/// - `resume_prefix` Some(s) → "**Resumed from pause** — {s}" is inserted as
///   a single line preceding the task description.
/// - A follow-up with `blockingForDependents=true` makes the heading read
///   "Open follow-ups (1 blocking)" / "(N blocking)". The blocker is kept
///   in-list with a `[BLOCKING]` tag so the worker doesn't need to flip
///   between sections.
///
/// Pure: no IO. Easy to unit-test (see `tests` module below).
pub(crate) fn compose_assignment_brief(
    task: &TaskRow,
    upstreams: &[(TaskRow, CompletionReport)],
    resume_prefix: Option<&str>,
) -> String {
    if upstreams.is_empty() {
        // Fresh assignment with no deps OR a resume case where deps weren't
        // waiting. Either way the brief block is empty; just optionally
        // prefix the resume line and return the description.
        let mut out = String::new();
        if let Some(p) = resume_prefix {
            out.push_str("**Resumed from pause** — ");
            out.push_str(p);
            out.push_str("\n\n");
        }
        out.push_str(&task.description);
        return out;
    }

    let mut out = String::new();

    // Header block — the lead paragraph reads naturally for 1, 2, or N
    // upstreams.
    let n = upstreams.len();
    let upstream_word = if n == 1 {
        "upstream task"
    } else {
        "upstream tasks"
    };
    out.push_str("## Upstream completion reports\n\n");
    out.push_str(&format!(
        "{} {} shipped before this one. Read their reports below before \
         starting; they may carry breaking changes or follow-ups you need \
         to handle.\n\n",
        n, upstream_word
    ));

    for (i, (upstream_task, report)) in upstreams.iter().enumerate() {
        // Heading uses the first non-empty line of the upstream's description
        // for readability — long descriptions span paragraphs. trim_end on
        // the first line so trailing whitespace doesn't leak.
        let first_line = upstream_task
            .description
            .lines()
            .next()
            .map(|s| s.trim_end())
            .unwrap_or("");
        out.push_str(&format!(
            "### Upstream task {} — {}\n\n",
            upstream_task.id, first_line
        ));

        // Verbatim summary_md.
        if !report.summary_md.is_empty() {
            out.push_str(&report.summary_md);
            if !report.summary_md.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }

        if !report.deliverables.is_empty() {
            out.push_str("**Deliverables:**\n");
            for d in &report.deliverables {
                out.push_str(&format!(
                    "- [{}] {} — {}\n",
                    d.kind, d.reference, d.description
                ));
            }
            out.push('\n');
        }

        if !report.breaking_changes.is_empty() {
            out.push_str("**Breaking changes:**\n");
            for bc in &report.breaking_changes {
                out.push_str(&format!("- **{}**: {}\n", bc.area, bc.description));
                if !bc.migration_steps_md.is_empty() {
                    // Inset the migration steps two spaces so they hang under
                    // the bullet in rendered Markdown. trim_end on each line
                    // keeps trailing whitespace out.
                    for line in bc.migration_steps_md.lines() {
                        out.push_str("  ");
                        out.push_str(line.trim_end());
                        out.push('\n');
                    }
                }
            }
            out.push('\n');
        }

        if !report.follow_ups.is_empty() {
            let blocking = report
                .follow_ups
                .iter()
                .filter(|fu| fu.blocking_for_dependents)
                .count();
            let heading = if blocking == 0 {
                "**Open follow-ups (none blocking):**".to_string()
            } else if blocking == 1 {
                "**Open follow-ups (1 blocking):**".to_string()
            } else {
                format!("**Open follow-ups ({} blocking):**", blocking)
            };
            out.push_str(&heading);
            out.push('\n');
            for fu in &report.follow_ups {
                if fu.blocking_for_dependents {
                    out.push_str(&format!(
                        "- [BLOCKING] {} [{}]\n",
                        fu.description, fu.priority
                    ));
                } else {
                    out.push_str(&format!("- {} [{}]\n", fu.description, fu.priority));
                }
            }
            out.push('\n');
        }

        // Per-upstream separator. The final upstream still gets one because
        // the next block is "## Your task".
        let is_last = i == upstreams.len() - 1;
        if !is_last {
            out.push_str("---\n\n");
        }
    }

    out.push_str("---\n\n## Your task\n\n");
    if let Some(p) = resume_prefix {
        out.push_str("**Resumed from pause** — ");
        out.push_str(p);
        out.push_str("\n\n");
    }
    out.push_str(&task.description);

    out
}

/// Phase 5 token budget proxy — at 4 bytes/token average, 50_000 bytes is
/// roughly 12_500 tokens. Comfortably under any worker first-message cap and
/// leaves room for the original task description plus any wrapper the worker
/// session prepends. Documented in plan §9 "Token budget on aggregated briefs".
///
/// Truncation strategy (see `compose_assignment_brief_with_budget`):
/// 1. Condense each upstream's `summary_md` to first 500 chars + ellipsis,
///    keeping structured fields verbatim.
/// 2. If still over budget, condense deliverable descriptions (80 chars) and
///    breaking-change descriptions (200 chars).
/// 3. If still over budget, hard-truncate the final string with a
///    `[…truncated to fit message budget]` suffix.
pub(crate) const MAX_BRIEF_BYTES: usize = 50_000;

const CONDENSED_SUMMARY_BYTES: usize = 500;
const CONDENSED_DELIVERABLE_DESC_BYTES: usize = 80;
const CONDENSED_BREAKING_DESC_BYTES: usize = 200;
const TRUNCATION_SUFFIX: &str = "\n\n[…truncated to fit message budget]";

/// Take the first `n_chars` characters of `s` (UTF-8 safe via `.chars()`).
/// If the original exceeded `n_chars`, append a single ellipsis "…".
fn condense_chars(s: &str, n_chars: usize) -> String {
    let mut out: String = s.chars().take(n_chars).collect();
    if s.chars().count() > n_chars {
        out.push('…');
    }
    out
}

/// Render the brief with the byte budget enforced. Pure function; its only
/// "side effect" is that callers can persist the condensed `summary_md`s
/// into `artifacts.condensedSummaryMd` for cache reuse on subsequent
/// briefings (see `dispatch_assignment_brief`).
///
/// Returns the rendered brief string. The `condensed_indices` out-param
/// (when `Some(&mut Vec)`) records the upstream indices whose `summary_md`
/// got condensed — caller writes those condensed bodies back to PG so the
/// next brief composition reuses them.
pub(crate) fn compose_assignment_brief_with_budget(
    task: &TaskRow,
    upstreams: &[(TaskRow, CompletionReport)],
    resume_prefix: Option<&str>,
    max_bytes: usize,
    condensed_indices: Option<&mut Vec<(usize, String)>>,
) -> String {
    // Tier 0: try the un-condensed brief first. Most briefs fit well under
    // the budget; spending a single render to confirm avoids ever paying the
    // cache-write cost when the budget is satisfied.
    let initial = compose_assignment_brief(task, upstreams, resume_prefix);
    if initial.len() <= max_bytes {
        return initial;
    }

    // Tier 1: condense each upstream's summary_md to CONDENSED_SUMMARY_BYTES
    // chars. Keep structured fields verbatim — those are the load-bearing
    // parts of the brief per plan §9.
    let mut tier1: Vec<(TaskRow, CompletionReport)> = Vec::with_capacity(upstreams.len());
    let mut condensed_bodies: Vec<(usize, String)> = Vec::new();
    for (i, (upstream_task, report)) in upstreams.iter().enumerate() {
        let mut report = report.clone();
        // If the upstream already cached a condensed_summary_md, prefer it.
        // Otherwise condense the original and cache the result.
        let cached = report
            .artifacts
            .get("condensedSummaryMd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(cached) = cached {
            if cached.len() < report.summary_md.len() {
                report.summary_md = cached;
            }
        } else if report.summary_md.chars().count() > CONDENSED_SUMMARY_BYTES {
            let condensed = condense_chars(&report.summary_md, CONDENSED_SUMMARY_BYTES);
            condensed_bodies.push((i, condensed.clone()));
            report.summary_md = condensed;
        }
        tier1.push((upstream_task.clone(), report));
    }
    let tier1_brief = compose_assignment_brief(task, &tier1, resume_prefix);
    if tier1_brief.len() <= max_bytes {
        if let Some(slot) = condensed_indices {
            *slot = condensed_bodies;
        }
        return tier1_brief;
    }

    // Tier 2: shrink deliverable / breaking-change descriptions.
    let mut tier2: Vec<(TaskRow, CompletionReport)> = tier1
        .into_iter()
        .map(|(t, mut r)| {
            for d in &mut r.deliverables {
                if d.description.chars().count() > CONDENSED_DELIVERABLE_DESC_BYTES {
                    d.description =
                        condense_chars(&d.description, CONDENSED_DELIVERABLE_DESC_BYTES);
                }
            }
            for bc in &mut r.breaking_changes {
                if bc.description.chars().count() > CONDENSED_BREAKING_DESC_BYTES {
                    bc.description = condense_chars(&bc.description, CONDENSED_BREAKING_DESC_BYTES);
                }
            }
            (t, r)
        })
        .collect();
    let tier2_brief = compose_assignment_brief(task, &tier2, resume_prefix);
    if tier2_brief.len() <= max_bytes {
        if let Some(slot) = condensed_indices {
            *slot = condensed_bodies;
        }
        return tier2_brief;
    }
    // Fall through — `tier2` already captures Tier 2 condensations; reuse the
    // owned vec to avoid an extra allocation in Tier 3 below.
    drop(tier2);

    // Tier 3: hard-truncate at max_bytes minus the suffix length so the
    // suffix fits cleanly. Be careful with UTF-8 boundaries — truncate at a
    // char boundary by walking back.
    warn!(
        "compose_assignment_brief_with_budget: brief still over budget after Tier 2; \
         hard-truncating to {} bytes (was {})",
        max_bytes,
        tier2_brief.len()
    );
    if let Some(slot) = condensed_indices {
        *slot = condensed_bodies;
    }
    let suffix = TRUNCATION_SUFFIX;
    let target = max_bytes.saturating_sub(suffix.len());
    let mut cut = target.min(tier2_brief.len());
    while cut > 0 && !tier2_brief.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = String::with_capacity(cut + suffix.len());
    out.push_str(&tier2_brief[..cut]);
    out.push_str(suffix);
    out
}

/// Pure-IO loader: reads the task row, the upstreams stashed in
/// `assignment_brief_extras`, the latest `WORKER_ADDED_DEPENDENCY` audit row
/// (for the resume prefix), and returns the composed Markdown brief.
///
/// Returns `Ok("")` when the task has no `assignment_brief_extras` AND no
/// upstreams in `depends_on` — i.e. there is nothing to brief, so the panel
/// (or AssignTask hook) renders/sends nothing.
///
/// Shared by `dispatch_assignment_brief` (for the AssignTask side effect)
/// and `commands::productivity::preview_assignment_brief` (for the
/// dashboard's "Briefing preview" panel) so the dashboard sees exactly
/// what the worker will see, by construction.
pub(crate) async fn build_assignment_brief(
    state: &Arc<ApiState>,
    task_id: &str,
) -> Result<String, String> {
    let pg = &state.app_state.pg_db;

    let task = pg
        .get_task_by_id(task_id)
        .await?
        .ok_or_else(|| format!("build_assignment_brief: no task {}", task_id))?;

    // Load the upstreams stashed by Rule E. None / null means no upstream
    // briefing is needed — fresh-assignment case (no deps, or deps were
    // never satisfied via Rule E).
    let extras = pg.get_assignment_brief_extras(task_id).await?;
    let mut upstream_pairs: Vec<(TaskRow, CompletionReport)> = Vec::new();
    if let Some(extras_json) = extras {
        if let Some(arr) = extras_json.get("upstreams").and_then(|v| v.as_array()) {
            for entry in arr {
                let uid = entry
                    .get("taskId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let report_val = entry.get("report").cloned();
                let (Some(uid), Some(report_val)) = (uid, report_val) else {
                    continue;
                };
                if report_val.is_null() {
                    continue;
                }
                let upstream_row = match pg.get_task_by_id(&uid).await? {
                    Some(t) => t,
                    None => {
                        warn!(
                            "build_assignment_brief: upstream task {} vanished, skipping",
                            uid
                        );
                        continue;
                    }
                };
                let report: CompletionReport = match serde_json::from_value(report_val) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(
                            "build_assignment_brief: failed to deserialize stashed report for upstream {}: {}",
                            uid, e
                        );
                        continue;
                    }
                };
                upstream_pairs.push((upstream_row, report));
            }
        }
    }

    // Resume-from-pause prefix: latest WORKER_ADDED_DEPENDENCY audit row for
    // this task. Format: "you added a dependency on task <upstream id> at
    // <timestamp>; here's the upstream's report and your original task
    // description." (per plan §4 "Worker-declared dependency replay")
    let resume_prefix_owned = match pg.latest_worker_added_dependency_for_task(task_id).await? {
        Some(decision) => {
            let upstream_hint = upstream_pairs
                .last()
                .map(|(t, _)| t.id.as_str())
                .unwrap_or("(unknown upstream)");
            Some(format!(
                "you added a dependency on task {} at {}; here's the upstream's report and your original task description.",
                upstream_hint, decision.created_at
            ))
        }
        None => None,
    };

    // Phase 5 token-budget audit (plan §9): use the budget-aware variant so
    // a 5-upstream brief that would otherwise blow the worker's first-message
    // budget gets condensed deterministically instead. Capture the indices
    // whose summaries were condensed so we can write them back into each
    // upstream's `artifacts.condensedSummaryMd` for reuse on the next brief.
    let mut condensed: Vec<(usize, String)> = Vec::new();
    let brief = compose_assignment_brief_with_budget(
        &task,
        &upstream_pairs,
        resume_prefix_owned.as_deref(),
        MAX_BRIEF_BYTES,
        Some(&mut condensed),
    );

    // Best-effort cache write: stash each condensed summary on the upstream's
    // own `completion_report.artifacts.condensedSummaryMd`. A failure here
    // doesn't fail the brief composition — next time we render we'll re-do
    // the truncation.
    for (idx, body) in condensed {
        if let Some((upstream_task, _)) = upstream_pairs.get(idx) {
            if let Err(e) = pg
                .write_completion_report_artifact(
                    &upstream_task.id,
                    "condensedSummaryMd",
                    serde_json::Value::String(body),
                )
                .await
            {
                warn!(
                    "build_assignment_brief: cache condensedSummaryMd for upstream {} failed: {}",
                    upstream_task.id, e
                );
            }
        }
    }

    Ok(brief)
}

/// Best-effort end-to-end wiring for an `AssignTask` side effect: load the
/// task row, the upstreams stashed in `assignment_brief_extras`, the latest
/// `WORKER_ADDED_DEPENDENCY` audit row (for the resume prefix), compose the
/// brief, push it to the worker, and clear the stash.
///
/// Returns the composed brief on success so callers (or future telemetry)
/// can inspect it; the AssignTask branch in `apply` discards the value and
/// only logs failures.
async fn dispatch_assignment_brief(
    state: &Arc<ApiState>,
    task_id: &str,
    session_id: &str,
) -> Result<String, String> {
    let pg = &state.app_state.pg_db;

    let brief = build_assignment_brief(state, task_id).await?;

    send_message_to_worker(state, session_id, &brief).await;

    // Order is load-bearing per Phase 5 audit (plan §9):
    //   (a) load extras, (b) compose brief — done in build_assignment_brief
    //   (c) send to worker — just above
    //   (d) clear extras — below.
    //
    // (c) success + (d) failure is the leak path. Retry once before logging
    // the warning so a single transient PG hiccup self-heals; on second
    // failure the startup-sweep at next runner boot
    // (`clear_stale_assignment_brief_extras`) will catch it as a backstop.
    if let Err(e) = pg.clear_assignment_brief_extras(task_id).await {
        warn!(
            "dispatch_assignment_brief: clear_assignment_brief_extras failed for {} (retrying once): {}",
            task_id, e
        );
        if let Err(e2) = pg.clear_assignment_brief_extras(task_id).await {
            warn!(
                "dispatch_assignment_brief: clear_assignment_brief_extras retry failed for {}: {} \
                 — assignment_brief_extras leak — manual SQL clear required (or wait for runner \
                 restart sweep)",
                task_id, e2
            );
        }
    }

    Ok(brief)
}

/// Best-effort `send_user_message` to a worker session. Errors are logged
/// (the action is auto-act, the decision row records intent) but do not
/// abort the calling action — the user can re-issue from the dashboard if
/// the message didn't land.
pub(crate) async fn send_message_to_worker(state: &Arc<ApiState>, session_id: &str, message: &str) {
    // Best-effort at this call site: the failure is already warn-logged inside.
    let _ = send_message_to_worker_via_handle(&state.app_handle, session_id, message).await;
}

/// The `AppHandle`-only variant of [`send_message_to_worker`]. `send_user_message`
/// only ever needs the `AppHandle` (to reach the `SessionManager` state), so
/// callers that hold an `AppHandle` but no `Arc<ApiState>` — e.g. the PR
/// shepherd's device-local author-notify (plan `2026-07-04-runner-pr-shepherd`
/// Phase 4), which runs inside the PR watcher with only `PrWatcherDeps` —
/// inject through here without constructing an `ApiState`. `send_message_to_worker`
/// delegates to this so there is exactly ONE injection primitive.
///
/// Returns whether the injection actually landed: failures are warn-logged AND
/// surfaced as `Err`, because some callers (the PR shepherd's one-per-head
/// notify claim) must not treat a swallowed failure as a delivered message.
pub(crate) async fn send_message_to_worker_via_handle(
    app_handle: &tauri::AppHandle,
    session_id: &str,
    message: &str,
) -> Result<(), String> {
    let session_manager = match app_handle.try_state::<Arc<crate::claude_session::SessionManager>>()
    {
        Some(sm) => sm.inner().clone(),
        None => {
            warn!("send_message_to_worker: SessionManager not available");
            return Err("SessionManager not available".to_string());
        }
    };

    if let Some(session) = session_manager.get(session_id) {
        // Ok(true) = sent immediately, Ok(false) = queued — both delivered.
        return match session.send_user_message(message) {
            Ok(_) => Ok(()),
            Err(e) => {
                warn!(
                    "send_message_to_worker: send_user_message failed for {}: {}",
                    session_id, e
                );
                Err(format!("send_user_message failed for {session_id}: {e}"))
            }
        };
    }

    // Phase 6: pty-backed Workers live in `worker_sessions`, not `sessions`.
    // Fall through and dispatch via WorkerSession::send_user_message, which
    // writes raw stdin keystrokes (CR-LF appended) to the pty.
    if let Some(worker) = session_manager.get_worker(session_id) {
        return match worker.send_user_message(message) {
            Ok(_) => Ok(()),
            Err(e) => {
                warn!(
                    "send_message_to_worker: worker send_user_message failed for {}: {}",
                    session_id, e
                );
                Err(format!(
                    "worker send_user_message failed for {session_id}: {e}"
                ))
            }
        };
    }

    warn!(
        "send_message_to_worker: no active session for task_run_id {}",
        session_id
    );
    Err(format!("no active session for task_run_id {session_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::pg::completion_reports::{
        BreakingChange, CompletionReport, Deliverable, FollowUp,
    };
    use std::collections::HashMap;

    fn make_task(id: &str, description: &str) -> TaskRow {
        TaskRow {
            id: id.to_string(),
            plan_id: "plan-1".to_string(),
            plan_version_hash: "h".to_string(),
            phase_name: "phase-1".to_string(),
            sequence_in_phase: 0,
            description: description.to_string(),
            expected_file_claims: Vec::new(),
            expected_dirs: Vec::new(),
            depends_on: Vec::new(),
            status: "ready".to_string(),
            assigned_session_id: None,
            started_at: None,
            completed_at: None,
            created_at: "2026-05-04T00:00:00Z".to_string(),
            updated_at: "2026-05-04T00:00:00Z".to_string(),
            notes: None,
        }
    }

    fn empty_report(summary: &str) -> CompletionReport {
        CompletionReport {
            summary_md: summary.to_string(),
            deliverables: Vec::new(),
            breaking_changes: Vec::new(),
            follow_ups: Vec::new(),
            artifacts: HashMap::new(),
        }
    }

    #[test]
    fn compose_assignment_brief_no_upstreams() {
        let task = make_task("t-1", "Write the gizmo.");
        let out = compose_assignment_brief(&task, &[], None);
        // No upstream block; just the task description verbatim.
        assert_eq!(out, "Write the gizmo.");
        assert!(!out.contains("## Upstream completion reports"));
        assert!(!out.contains("## Your task"));
    }

    #[test]
    fn compose_assignment_brief_no_upstreams_with_resume_prefix() {
        let task = make_task("t-1", "Write the gizmo.");
        let out = compose_assignment_brief(
            &task,
            &[],
            Some("you added a dependency on task t-99 at 2026-05-04T01:00:00Z."),
        );
        assert!(out.starts_with("**Resumed from pause** — you added a dependency"));
        assert!(out.contains("Write the gizmo."));
    }

    #[test]
    fn compose_assignment_brief_one_upstream_no_blockers() {
        let task = make_task("t-2", "Wire the gizmo to the widget.");
        let upstream = make_task("t-1", "Build the gizmo");
        let report = CompletionReport {
            summary_md: "Built and shipped the gizmo.".to_string(),
            deliverables: vec![Deliverable {
                kind: "pr".to_string(),
                reference: "https://github.com/x/y/pull/42".to_string(),
                description: "Add gizmo module".to_string(),
            }],
            breaking_changes: Vec::new(),
            follow_ups: vec![FollowUp {
                description: "Document the gizmo API".to_string(),
                priority: "important".to_string(),
                blocking_for_dependents: false,
            }],
            artifacts: HashMap::new(),
        };
        let out = compose_assignment_brief(&task, &[(upstream, report)], None);

        // Header
        assert!(
            out.starts_with("## Upstream completion reports"),
            "brief should open with the section heading; got: {}",
            out
        );
        assert!(out.contains("1 upstream task shipped before this one"));

        // Per-upstream
        assert!(out.contains("### Upstream task t-1 — Build the gizmo"));
        assert!(out.contains("Built and shipped the gizmo."));
        assert!(out.contains("**Deliverables:**"));
        assert!(out.contains("- [pr] https://github.com/x/y/pull/42 — Add gizmo module"));
        assert!(out.contains("**Open follow-ups (none blocking):**"));
        assert!(out.contains("- Document the gizmo API [important]"));

        // Trailing task block
        assert!(out.contains("## Your task"));
        assert!(out.ends_with("Wire the gizmo to the widget."));
    }

    #[test]
    fn compose_assignment_brief_breaking_changes_render() {
        let task = make_task("t-2", "Migrate consumers.");
        let upstream = make_task("t-1", "Refactor adapter API");
        let report = CompletionReport {
            summary_md: "Refactor done.".to_string(),
            deliverables: Vec::new(),
            breaking_changes: vec![BreakingChange {
                area: "adapter".to_string(),
                description: "Adapter::run signature changed".to_string(),
                migration_steps_md: "1. Update callers.\n2. Re-test.".to_string(),
            }],
            follow_ups: Vec::new(),
            artifacts: HashMap::new(),
        };
        let out = compose_assignment_brief(&task, &[(upstream, report)], None);

        assert!(out.contains("**Breaking changes:**"));
        assert!(out.contains("- **adapter**: Adapter::run signature changed"));
        // Migration steps inset two spaces under the bullet.
        assert!(out.contains("  1. Update callers."));
        assert!(out.contains("  2. Re-test."));
    }

    #[test]
    fn compose_assignment_brief_blocking_followup_marked() {
        let task = make_task("t-2", "Continue.");
        let upstream = make_task("t-1", "Earlier task");
        let report = CompletionReport {
            summary_md: "ok".to_string(),
            deliverables: Vec::new(),
            breaking_changes: Vec::new(),
            follow_ups: vec![
                FollowUp {
                    description: "Confirm with user before merging".to_string(),
                    priority: "critical".to_string(),
                    blocking_for_dependents: true,
                },
                FollowUp {
                    description: "Add docs".to_string(),
                    priority: "nice-to-have".to_string(),
                    blocking_for_dependents: false,
                },
            ],
            artifacts: HashMap::new(),
        };
        let out = compose_assignment_brief(&task, &[(upstream, report)], None);

        // Heading reflects the blocker count.
        assert!(out.contains("**Open follow-ups (1 blocking):**"));
        // Blocker bullet has the [BLOCKING] tag.
        assert!(out.contains("- [BLOCKING] Confirm with user before merging [critical]"));
        // Non-blocker bullet does NOT have the tag.
        assert!(out.contains("- Add docs [nice-to-have]"));
        assert!(!out.contains("[BLOCKING] Add docs"));
    }

    #[test]
    fn compose_assignment_brief_resume_prefix() {
        let task = make_task("t-2", "Wire the gizmo.");
        let upstream = make_task("t-1", "Build it");
        let report = empty_report("done");
        let out = compose_assignment_brief(
            &task,
            &[(upstream, report)],
            Some("you added a dependency on task t-1 at 2026-05-04T03:00:00Z; here's the upstream's report and your original task description."),
        );

        // The resume prefix lands above the task description, after the
        // "## Your task" heading.
        let task_section_idx = out
            .find("## Your task")
            .expect("output must contain '## Your task' heading");
        let resume_idx = out
            .find("**Resumed from pause**")
            .expect("output must contain resume prefix");
        let body_idx = out
            .find("Wire the gizmo.")
            .expect("output must contain the task description");

        assert!(task_section_idx < resume_idx);
        assert!(resume_idx < body_idx);
    }

    #[test]
    fn compose_assignment_brief_multi_upstream_separator() {
        // Two upstreams produce a "---" separator between them and another
        // before "## Your task".
        let task = make_task("t-3", "Integrate.");
        let u1 = make_task("t-1", "Component A");
        let u2 = make_task("t-2", "Component B");
        let r1 = empty_report("A done");
        let r2 = empty_report("B done");
        let out = compose_assignment_brief(&task, &[(u1, r1), (u2, r2)], None);

        // Lead text counts both.
        assert!(out.contains("2 upstream tasks shipped before this one"));
        // Each upstream's heading appears.
        assert!(out.contains("### Upstream task t-1 — Component A"));
        assert!(out.contains("### Upstream task t-2 — Component B"));
        // Two `---` lines: one between upstreams, one before "## Your task".
        let dash_count = out.matches("---").count();
        assert!(
            dash_count >= 2,
            "expected at least 2 separators, got {}: {}",
            dash_count,
            out
        );
        assert!(out.ends_with("Integrate."));
    }

    // -------------------------------------------------------------------
    // Phase 5: token-budget audit (`compose_assignment_brief_with_budget`).
    // -------------------------------------------------------------------

    #[test]
    fn budget_brief_5_upstreams_fits_under_max_brief_bytes() {
        // 5 upstream reports with realistic-but-modest content. The brief
        // should comfortably fit under MAX_BRIEF_BYTES without any
        // condensation tier kicking in.
        let task = make_task("t-final", "Finalize.");
        let upstreams: Vec<(TaskRow, CompletionReport)> = (0..5)
            .map(|i| {
                let upstream = make_task(&format!("t-{}", i), &format!("Upstream {}", i));
                let report = CompletionReport {
                    summary_md: format!(
                        "Shipped piece {}. About 100 chars of description here describing what got done in this task.",
                        i
                    ),
                    deliverables: vec![Deliverable {
                        kind: "pr".to_string(),
                        reference: format!("https://github.com/x/y/pull/{}", i),
                        description: "PR description".to_string(),
                    }],
                    breaking_changes: vec![],
                    follow_ups: vec![FollowUp {
                        description: "follow-up".to_string(),
                        priority: "nice-to-have".to_string(),
                        blocking_for_dependents: false,
                    }],
                    artifacts: HashMap::new(),
                };
                (upstream, report)
            })
            .collect();

        let brief =
            compose_assignment_brief_with_budget(&task, &upstreams, None, MAX_BRIEF_BYTES, None);
        assert!(
            brief.len() <= MAX_BRIEF_BYTES,
            "5-upstream brief should fit, got {} > {}",
            brief.len(),
            MAX_BRIEF_BYTES
        );
        assert!(!brief.contains("[…truncated"));
        // No condensation expected — verify we still have full summaries.
        assert!(brief.contains("Shipped piece 0."));
        assert!(brief.contains("Shipped piece 4."));
    }

    #[test]
    fn budget_brief_giant_summary_gets_condensed() {
        // One upstream with a giant summary_md (5000 chars). The Tier-0
        // un-condensed brief stays well under MAX_BRIEF_BYTES (50K) so
        // condensation does NOT trigger here — assert that and the
        // verbatim presence of the summary's start. To exercise the
        // condensation tier, drop the budget down so Tier 0 fails.
        let task = make_task("t-final", "Finalize.");
        let upstream = make_task("t-1", "Big upstream");
        let report = CompletionReport {
            summary_md: "A".repeat(5000),
            deliverables: vec![],
            breaking_changes: vec![],
            follow_ups: vec![],
            artifacts: HashMap::new(),
        };
        let mut condensed_indices: Vec<(usize, String)> = Vec::new();
        // Use a 1000-byte budget — Tier 0 (5000+ chars) blows it, Tier 1
        // condenses to ~500 chars and fits.
        let brief = compose_assignment_brief_with_budget(
            &task,
            &[(upstream, report)],
            None,
            1000,
            Some(&mut condensed_indices),
        );
        assert!(
            brief.len() <= 1000,
            "condensed brief should fit under budget, got {}",
            brief.len()
        );
        // Tier 1 should have condensed upstream index 0.
        assert_eq!(condensed_indices.len(), 1);
        assert_eq!(condensed_indices[0].0, 0);
        assert!(condensed_indices[0].1.ends_with('…'));
        // Hard truncation suffix should NOT appear (Tier 1 was sufficient).
        assert!(!brief.contains("[…truncated to fit message budget]"));
    }

    #[test]
    fn budget_brief_50_upstreams_hard_truncates() {
        // 50 upstreams × non-trivial reports cannot fit even after Tiers 1+2.
        // Final brief gets the truncation suffix.
        let task = make_task("t-final", "Finalize.");
        let upstreams: Vec<(TaskRow, CompletionReport)> = (0..50)
            .map(|i| {
                let upstream = make_task(&format!("t-{}", i), &format!("Upstream {}", i));
                let report = CompletionReport {
                    summary_md: "X".repeat(2000),
                    deliverables: (0..20)
                        .map(|j| Deliverable {
                            kind: "commit".to_string(),
                            reference: format!("sha-{}-{}", i, j),
                            description: "Y".repeat(400),
                        })
                        .collect(),
                    breaking_changes: (0..10)
                        .map(|j| BreakingChange {
                            area: format!("area-{}-{}", i, j),
                            description: "Z".repeat(800),
                            migration_steps_md: "M".repeat(1000),
                        })
                        .collect(),
                    follow_ups: vec![],
                    artifacts: HashMap::new(),
                };
                (upstream, report)
            })
            .collect();

        let brief =
            compose_assignment_brief_with_budget(&task, &upstreams, None, MAX_BRIEF_BYTES, None);
        assert!(
            brief.len() <= MAX_BRIEF_BYTES,
            "hard-truncated brief must not exceed budget, got {} > {}",
            brief.len(),
            MAX_BRIEF_BYTES
        );
        assert!(
            brief.ends_with("[…truncated to fit message budget]"),
            "hard-truncated brief must end with the truncation suffix"
        );
    }

    #[test]
    fn budget_brief_uses_cached_condensed_summary() {
        // When `artifacts.condensedSummaryMd` is already present and shorter
        // than the original summary_md, Tier 1 should prefer the cached
        // version (idempotent reuse — saves a re-truncation).
        let task = make_task("t-final", "Finalize.");
        let upstream = make_task("t-1", "Big upstream");
        let cached_summary = "[cached condensed summary that is much shorter]";
        let mut artifacts = HashMap::new();
        artifacts.insert(
            "condensedSummaryMd".to_string(),
            serde_json::Value::String(cached_summary.to_string()),
        );
        let report = CompletionReport {
            summary_md: "Z".repeat(5000),
            deliverables: vec![],
            breaking_changes: vec![],
            follow_ups: vec![],
            artifacts,
        };
        let mut condensed_indices: Vec<(usize, String)> = Vec::new();
        let brief = compose_assignment_brief_with_budget(
            &task,
            &[(upstream, report)],
            None,
            1000,
            Some(&mut condensed_indices),
        );
        assert!(
            brief.contains(cached_summary),
            "brief should use the cached condensed summary verbatim"
        );
        // No new condensation needed — we used the cache.
        assert!(
            condensed_indices.is_empty(),
            "cache hit should produce no new condensed_bodies entries"
        );
    }

    // -------------------------------------------------------------------
    // coord-as-deconflicter plan §4.6: per-session_id narrowing of
    // `must_advise_only` so deconflicter sessions are restricted to
    // AdviseWithText only.
    // -------------------------------------------------------------------

    #[test]
    fn deconflicter_session_restricted_to_advise() {
        let advise = CoordinatorAction::AdviseWithText {
            target_id: Some("t".into()),
            reasoning: "test".into(),
        };
        let assign = CoordinatorAction::AssignTask {
            task_id: "t".into(),
            session_id: "s".into(),
            reasoning: None,
        };

        // Deconflicter: advise is the primary action (NOT advise-only),
        // anything else is gated.
        assert!(!must_advise_only("deconflicter-rust", &advise));
        assert!(must_advise_only("deconflicter-rust", &assign));

        // Regular session: existing destructive-set behavior unchanged.
        assert!(!must_advise_only("worker-abc", &advise));
        assert!(!must_advise_only("worker-abc", &assign));
        let kill = CoordinatorAction::KillSession {
            session_id: "s".into(),
            reasoning: "test".into(),
        };
        assert!(must_advise_only("worker-abc", &kill));
    }
}
