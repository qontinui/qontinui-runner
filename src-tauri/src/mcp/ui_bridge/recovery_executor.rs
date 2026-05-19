//! Automatic recovery executor for UI Bridge sync-action failures.
//!
//! Phase 5 of the 2026-05-18 *UI Bridge Diagnostic Discipline* plan. Consumes
//! the `failureDetails.suggestedActions: RecoverySuggestion[]` that Wave-1
//! Phase 3 now populates on sync control-action failures, picks the best
//! retryable command deterministically, executes it via the runner's existing
//! UI Bridge IPC action path, retries the original action once, and — only if
//! that still fails — degrades to the runner's existing LLM-driven recovery
//! path (`ai_recovery_attempt` IPC, exposed at `POST /ui-bridge/ai/recovery/
//! attempt`). It never panics on a missing/empty `suggestedActions` array.
//!
//! Telemetry `(errorCode, recovery_command_chosen, recovery_succeeded)` is
//! emitted through the runner's *existing* structured-event surface —
//! `PgDatabase::insert_ui_bridge_event` with `event_type = "recovery_attempted"`
//! and the tuple carried in the `metadata` JSON column (the same sink the
//! `execute_action` handler already uses for action telemetry). No new
//! transport is introduced.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::mcp::types::ApiState;

use super::diagnostics::{extract_error_code, CanonicalCode};
use super::request::ui_bridge_request_sync;

/// The single canonical recovery type (plan D6). Mirrors the SDK
/// `RecoverySuggestion = { suggestion, command?, confidence, retryable,
/// priority? }` shape that `ActionFailureDetails.suggestedActions`,
/// `StructuredFailureInfo.suggestedActions`, `ERROR_SUGGESTIONS`, and
/// `codes.json.recoveryTemplate` all serialize. Unknown extra fields are
/// ignored (forward-compatible with template additions).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RecoverySuggestion {
    /// Human-readable description of the suggested recovery (dual-audience —
    /// retained as a feature per plan goal #3, not for BC).
    pub suggestion: String,
    /// The machine-executable recovery command, when one exists. Entries
    /// without a command are advisory-only and never auto-executed.
    #[serde(default)]
    pub command: Option<String>,
    /// Confidence in `[0.0, 1.0]`.
    #[serde(default)]
    pub confidence: f64,
    /// Whether retrying the original action after this command is expected
    /// to help. Only `retryable == true` entries are auto-executed.
    #[serde(default)]
    pub retryable: bool,
    /// Optional deterministic ordering hint — lower runs first (plan D6
    /// retains this as a capability so selection is not confidence-only).
    #[serde(default)]
    pub priority: Option<u32>,
}

/// Outcome of an automatic recovery attempt. Returned to the caller so the
/// HTTP handler can decide whether to surface success or the original error.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryOutcome {
    /// `true` when either the structured command-retry path or the LLM
    /// fallback ultimately made the original action succeed.
    pub recovered: bool,
    /// Which path produced the (attempted) recovery.
    pub via: RecoveryVia,
    /// The recovery command that was chosen and executed, if any.
    pub command_chosen: Option<String>,
    /// The canonical error code we recovered from, as a `UB-` string.
    pub error_code: Option<String>,
    /// The post-recovery action result payload, when `recovered`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

/// Which recovery strategy produced the outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryVia {
    /// A structured `suggestedActions` command was executed and the original
    /// action retried.
    StructuredCommand,
    /// Fell back to the runner's existing LLM-driven recovery IPC.
    LlmFallback,
    /// No recovery path applied (no suggestions and LLM fallback also failed
    /// or was unavailable).
    None,
}

/// Select the recovery suggestion to execute from a failure response's
/// `failureDetails.suggestedActions`.
///
/// Rules (deterministic):
/// 1. Only entries with `retryable == true` **and** a non-empty `command`
///    are candidates.
/// 2. Tie-break by `priority` ascending (an entry with `priority` always
///    beats one without; lower number wins).
/// 3. Then by `confidence` descending.
/// 4. Then by `command` lexicographically (final total-order tiebreak so the
///    choice is fully deterministic across runs).
///
/// Returns `None` when there are no executable candidates (caller degrades to
/// the LLM fallback — never panics).
pub fn select_suggestion(suggestions: &[RecoverySuggestion]) -> Option<&RecoverySuggestion> {
    suggestions
        .iter()
        .filter(|s| s.retryable && s.command.as_deref().map(|c| !c.is_empty()).unwrap_or(false))
        .min_by(|a, b| {
            // priority: Some < None; lower number first.
            match (a.priority, b.priority) {
                (Some(pa), Some(pb)) => pa.cmp(&pb),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            // confidence descending
            .then_with(|| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            // command lexicographic — final total order
            .then_with(|| {
                a.command
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.command.as_deref().unwrap_or(""))
            })
        })
}

/// Parse `failureDetails.suggestedActions` from a UI Bridge failure response.
/// Missing/malformed → empty Vec (never panics — caller degrades gracefully).
pub fn parse_suggested_actions(data: &serde_json::Value) -> Vec<RecoverySuggestion> {
    let raw = data
        .get("failureDetails")
        .and_then(|fd| fd.get("suggestedActions"))
        // Some envelopes hoist suggestedActions to the top level.
        .or_else(|| data.get("suggestedActions"));
    match raw {
        Some(v) => serde_json::from_value::<Vec<RecoverySuggestion>>(v.clone()).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// Map a recovery `command` string to a UI Bridge IPC dispatch against the
/// failing element, and execute it via the runner's existing IPC action path
/// (`ui_bridge_request_sync`). Returns `Ok` on dispatch success.
///
/// The command vocabulary is the closed set used by `codes.json`'s
/// `recoveryTemplate` (plan §2.2): `scroll_into_view`, `wait_for_enabled`,
/// `resnapshot`/`discover`, `broaden_selector` (advisory — no IPC), and a
/// raw `execute_action:<action>` escape hatch. Unknown commands degrade to a
/// `discover` resnapshot (the safest universal "make refs fresh" action)
/// rather than failing — the original-action retry then re-validates.
async fn execute_recovery_command(
    state: &Arc<ApiState>,
    command: &str,
    element_id: &str,
) -> Result<serde_json::Value, String> {
    let cmd = command.trim();
    match cmd {
        "scroll_into_view" | "scrollIntoView" => {
            ui_bridge_request_sync(
                state,
                "execute_action",
                serde_json::json!({
                    "elementId": element_id,
                    "action": { "action": "scrollIntoView", "params": {} }
                }),
            )
            .await
        }
        "wait_for_enabled" | "waitForEnabled" => {
            ui_bridge_request_sync(
                state,
                "wait_for_element_state_predicate",
                serde_json::json!({
                    "params": {
                        "elementId": element_id,
                        "state": "enabled",
                        "timeoutMs": 5000,
                        "pollMs": 100,
                    }
                }),
            )
            .await
        }
        "resnapshot" | "discover" | "rediscover" => {
            ui_bridge_request_sync(
                state,
                "discover",
                serde_json::json!({ "options": { "interactiveOnly": false } }),
            )
            .await
        }
        "broaden_selector" | "broadenSelector" => {
            // Advisory only — there is no element-scoped IPC for "broaden the
            // selector"; surfacing the suggestion is the recovery. A fresh
            // discover still helps the subsequent retry, so do that.
            ui_bridge_request_sync(
                state,
                "discover",
                serde_json::json!({ "options": { "interactiveOnly": false } }),
            )
            .await
        }
        other if other.starts_with("execute_action:") => {
            let action = other.trim_start_matches("execute_action:").trim();
            ui_bridge_request_sync(
                state,
                "execute_action",
                serde_json::json!({
                    "elementId": element_id,
                    "action": { "action": action, "params": {} }
                }),
            )
            .await
        }
        _ => {
            warn!(
                "recovery_executor: unknown recovery command '{}', degrading to discover resnapshot",
                cmd
            );
            ui_bridge_request_sync(
                state,
                "discover",
                serde_json::json!({ "options": { "interactiveOnly": false } }),
            )
            .await
        }
    }
}

/// True when an IPC response (or original-action result) represents success.
fn is_ipc_success(data: &serde_json::Value) -> bool {
    // Absent `success` is treated as success (matches `wrap_ipc_result`'s
    // healthy-response rule). An explicit `false` is failure.
    data.get("success").and_then(|v| v.as_bool()) != Some(false)
}

/// Run the LLM-driven recovery fallback via the runner's *existing*
/// `ai_recovery_attempt` IPC (exposed at `POST /ui-bridge/ai/recovery/
/// attempt`). This is the path the runner already owns — we locate and reuse
/// it, never invent a new one.
async fn llm_fallback(
    state: &Arc<ApiState>,
    instruction: &str,
) -> Result<serde_json::Value, String> {
    let payload = serde_json::json!({
        "params": { "instruction": instruction }
    });
    let resp = ui_bridge_request_sync(state, "ai_recovery_attempt", payload).await?;
    if is_ipc_success(&resp) {
        Ok(resp)
    } else {
        Err(resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("LLM recovery failed")
            .to_string())
    }
}

/// Attempt automatic recovery for a failed UI Bridge action.
///
/// `failure` is the failure response body (must already be known to be a
/// failure). `element_id` is the action's target. `original_action` is the
/// `execute_action` payload to retry once after a structured recovery
/// command. `task_run_id` (when present) keys telemetry persistence.
///
/// Decision tree:
/// 1. Parse the canonical error code + `suggestedActions`.
/// 2. Pick the best retryable command (`select_suggestion`). If one exists:
///    execute it, then retry the original action once. On success → done
///    (`StructuredCommand`).
/// 3. Otherwise (or if the retry still fails) → LLM fallback IPC. On success
///    → done (`LlmFallback`).
/// 4. Otherwise → `RecoveryVia::None`, `recovered: false` (caller surfaces
///    the original error).
///
/// Telemetry is emitted exactly once per call regardless of branch.
pub async fn attempt_recovery(
    state: &Arc<ApiState>,
    failure: &serde_json::Value,
    element_id: &str,
    original_action: &serde_json::Value,
    task_run_id: Option<i64>,
) -> RecoveryOutcome {
    let error_code = extract_error_code(failure);
    let suggestions = parse_suggested_actions(failure);

    let mut chosen_command: Option<String> = None;
    let mut outcome = RecoveryOutcome {
        recovered: false,
        via: RecoveryVia::None,
        command_chosen: None,
        error_code: error_code.map(|c| c.as_str().to_string()),
        result: None,
    };

    // ── Tier 1: structured suggestedActions command + single retry ───────
    if let Some(sel) = select_suggestion(&suggestions) {
        let command = sel.command.clone().unwrap_or_default();
        chosen_command = Some(command.clone());
        info!(
            "recovery_executor: selected command '{}' (confidence={}, priority={:?}) for {:?}",
            command, sel.confidence, sel.priority, error_code
        );
        match execute_recovery_command(state, &command, element_id).await {
            Ok(_) => {
                // Retry the original action exactly once.
                match ui_bridge_request_sync(state, "execute_action", original_action.clone()).await
                {
                    Ok(retry) if is_ipc_success(&retry) => {
                        outcome.recovered = true;
                        outcome.via = RecoveryVia::StructuredCommand;
                        outcome.command_chosen = chosen_command.clone();
                        outcome.result = Some(retry);
                        emit_telemetry(state, &error_code, &chosen_command, true, task_run_id)
                            .await;
                        return outcome;
                    }
                    Ok(_) | Err(_) => {
                        warn!(
                            "recovery_executor: original action still failed after '{}'; \
                             degrading to LLM fallback",
                            command
                        );
                    }
                }
            }
            Err(e) => {
                warn!(
                    "recovery_executor: recovery command '{}' dispatch failed ({}); \
                     degrading to LLM fallback",
                    command, e
                );
            }
        }
    } else {
        info!(
            "recovery_executor: no executable suggestedActions for {:?}; \
             degrading to LLM fallback",
            error_code
        );
    }

    // ── Tier 2: existing LLM-driven recovery path ────────────────────────
    let instruction = failure
        .get("error")
        .and_then(|v| v.as_str())
        .map(|s| format!("recover from: {}", s))
        .unwrap_or_else(|| "recover from error state".to_string());
    match llm_fallback(state, &instruction).await {
        Ok(resp) => {
            outcome.recovered = true;
            outcome.via = RecoveryVia::LlmFallback;
            outcome.command_chosen = chosen_command.clone();
            outcome.result = Some(resp);
            emit_telemetry(state, &error_code, &chosen_command, true, task_run_id).await;
        }
        Err(e) => {
            warn!("recovery_executor: LLM fallback also failed: {}", e);
            outcome.via = RecoveryVia::None;
            outcome.command_chosen = chosen_command.clone();
            emit_telemetry(state, &error_code, &chosen_command, false, task_run_id).await;
        }
    }
    outcome
}

/// Emit `(errorCode, recovery_command_chosen, recovery_succeeded)` through the
/// runner's existing structured-event surface. Fire-and-forget PG write keyed
/// off `task_run_id` — matches `execute_action`'s telemetry pattern exactly
/// (no new transport). When `task_run_id` is absent the tuple is still logged
/// at info level (always observable) but not persisted (consistent with the
/// existing event-persistence gate).
async fn emit_telemetry(
    state: &Arc<ApiState>,
    error_code: &Option<CanonicalCode>,
    command_chosen: &Option<String>,
    succeeded: bool,
    task_run_id: Option<i64>,
) {
    let code_str = error_code.map(|c| c.as_str().to_string());
    info!(
        "recovery_telemetry: errorCode={:?} command={:?} succeeded={}",
        code_str, command_chosen, succeeded
    );

    let Some(tr_id) = task_run_id else {
        return;
    };

    let metadata = serde_json::json!({
        "errorCode": code_str,
        "recoveryCommandChosen": command_chosen,
        "recoverySucceeded": succeeded,
    })
    .to_string();

    let seq = state
        .ui_bridge_event_sequence
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pg_db = state.app_state.pg_db.clone();
    let action_label = command_chosen.clone();

    tokio::spawn(async move {
        match pg_db
            .insert_ui_bridge_event(
                Some(tr_id),
                seq,
                "recovery_attempted",
                None,
                None,
                None,
                action_label.as_deref(),
                None,
                None,
                None,
                succeeded,
                None,
                Some(&metadata),
            )
            .await
        {
            Ok(row_id) => info!("recovery telemetry persisted: row_id={}", row_id),
            Err(e) => warn!("recovery telemetry persist failed: {}", e),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sug(cmd: Option<&str>, conf: f64, retryable: bool, prio: Option<u32>) -> RecoverySuggestion {
        RecoverySuggestion {
            suggestion: format!("do {:?}", cmd),
            command: cmd.map(|s| s.to_string()),
            confidence: conf,
            retryable,
            priority: prio,
        }
    }

    #[test]
    fn selects_highest_confidence_retryable_with_command() {
        let s = vec![
            sug(Some("a"), 0.5, true, None),
            sug(Some("b"), 0.9, true, None),
            sug(Some("c"), 0.99, false, None), // not retryable — excluded
            sug(None, 1.0, true, None),        // no command — excluded
        ];
        let chosen = select_suggestion(&s).unwrap();
        assert_eq!(chosen.command.as_deref(), Some("b"));
    }

    #[test]
    fn priority_beats_confidence() {
        // Lower priority number wins even though its confidence is lower.
        let s = vec![
            sug(Some("low-prio"), 0.99, true, None),
            sug(Some("high-prio"), 0.40, true, Some(1)),
        ];
        let chosen = select_suggestion(&s).unwrap();
        assert_eq!(chosen.command.as_deref(), Some("high-prio"));
    }

    #[test]
    fn priority_tie_breaks_by_confidence_then_command() {
        let s = vec![
            sug(Some("zzz"), 0.7, true, Some(2)),
            sug(Some("aaa"), 0.7, true, Some(2)),
            sug(Some("mmm"), 0.8, true, Some(2)),
        ];
        // Same priority → highest confidence (mmm) wins.
        assert_eq!(
            select_suggestion(&s).unwrap().command.as_deref(),
            Some("mmm")
        );
        // Remove the confidence winner → lexicographic tiebreak (aaa < zzz).
        let s2 = vec![
            sug(Some("zzz"), 0.7, true, Some(2)),
            sug(Some("aaa"), 0.7, true, Some(2)),
        ];
        assert_eq!(
            select_suggestion(&s2).unwrap().command.as_deref(),
            Some("aaa")
        );
    }

    #[test]
    fn no_executable_candidates_returns_none() {
        let s = vec![
            sug(None, 1.0, true, None),
            sug(Some(""), 1.0, true, None),
            sug(Some("x"), 1.0, false, None),
        ];
        assert!(select_suggestion(&s).is_none());
        assert!(select_suggestion(&[]).is_none());
    }

    #[test]
    fn parse_suggested_actions_from_failure_details() {
        let data = json!({
            "success": false,
            "failureDetails": {
                "errorCode": "UB-ELEM-NOT-VISIBLE",
                "suggestedActions": [
                    { "suggestion": "Scroll the element into view",
                      "command": "scroll_into_view",
                      "confidence": 0.9, "retryable": true, "priority": 1 },
                    { "suggestion": "Broaden the selector",
                      "command": "broaden_selector",
                      "confidence": 0.4, "retryable": false }
                ]
            }
        });
        let parsed = parse_suggested_actions(&data);
        assert_eq!(parsed.len(), 2);
        let chosen = select_suggestion(&parsed).unwrap();
        assert_eq!(chosen.command.as_deref(), Some("scroll_into_view"));
    }

    #[test]
    fn parse_suggested_actions_missing_is_empty_never_panics() {
        assert!(parse_suggested_actions(&json!({})).is_empty());
        assert!(parse_suggested_actions(&json!({ "failureDetails": {} })).is_empty());
        // Malformed (not an array) → empty, no panic.
        assert!(parse_suggested_actions(
            &json!({ "failureDetails": { "suggestedActions": "oops" } })
        )
        .is_empty());
    }

    #[test]
    fn parse_suggested_actions_hoisted_top_level() {
        let data = json!({
            "suggestedActions": [
                { "suggestion": "x", "command": "discover",
                  "confidence": 0.5, "retryable": true }
            ]
        });
        assert_eq!(parse_suggested_actions(&data).len(), 1);
    }

    #[test]
    fn is_ipc_success_semantics() {
        assert!(is_ipc_success(&json!({ "success": true })));
        assert!(is_ipc_success(&json!({ "clicked": true }))); // absent == success
        assert!(!is_ipc_success(&json!({ "success": false })));
    }
}
