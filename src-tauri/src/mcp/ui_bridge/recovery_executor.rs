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
    /// fallback ultimately made the ORIGINAL action succeed, re-verified by an
    /// independent retry ([`original_action_now_succeeds`]).
    ///
    /// NOT the frontend's `attemptSucceeded` (the `ai_recovery_attempt`
    /// payload, which decides `/ai/recovery/attempt`'s HTTP status in
    /// `ai_analyze::as_recovery_failure`). That one says only "the scoped
    /// executor acted on the addressed element"; this one is the stricter bar.
    /// They legitimately disagree — pinned by
    /// `scoped_attempt_success_does_not_imply_recovered`.
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

/// Env var gating the LLM-driven recovery fallback. **Unset means OFF.**
///
/// The fallback hands a free-text instruction to an on-page NL executor that
/// searched the WHOLE element tree, so a failed action could — and did — end
/// up typing into an unrelated element (a command-palette input picked up a
/// value from a failed write elsewhere on the page). Guessing which element
/// the operator meant is not a recovery primitive, so the path is opt-in:
/// nothing runs it unless a human sets this variable.
pub const LLM_RECOVERY_ENV: &str = "QONTINUI_UI_BRIDGE_LLM_RECOVERY";

/// Parse the [`LLM_RECOVERY_ENV`] value. Split from the `std::env` read so the
/// default-OFF decision is unit-testable without mutating process env.
pub fn llm_recovery_flag_enabled(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// Whether the LLM recovery fallback is enabled for this process.
fn llm_recovery_enabled() -> bool {
    llm_recovery_flag_enabled(std::env::var(LLM_RECOVERY_ENV).ok().as_deref())
}

/// Actions that MUTATE input state — the ONE declaration of the list.
///
/// Two enforcement points refuse these as recovery steps, deliberately, so
/// neither is a single point of failure across the IPC trust boundary: the
/// runner's [`is_write_action`] here, and the frontend's `isWriteAction` in
/// `src/hooks/ui-bridge-events/recoveryScope.ts`. The CHECKS stay two; the LIST
/// is one. The frontend does not restate it — it imports
/// [`WRITE_ACTIONS_FIXTURE`], which is generated from this constant and
/// drift-gated by `write_actions_fixture_matches_rust_declaration`.
///
/// Entries are lowercase, because both checks compare the trimmed, lowercased
/// action name.
pub const WRITE_ACTIONS: &[&str] = &[
    "type",
    "settext",
    "setvalue",
    "fill",
    "input",
    "paste",
    "append",
    "clear",
    "select",
    "selectoption",
    "check",
    "uncheck",
    "toggle",
    "submit",
    "upload",
    "setfiles",
    "sendkeys",
    "presskey",
    "writetoterminal",
];

/// The generated JSON the frontend imports its write-action list from,
/// relative to the repository root. Regenerate with
/// `QONTINUI_UPDATE_WRITE_ACTIONS=1 cargo test --bin qontinui-runner
/// write_actions_fixture_matches_rust_declaration`.
pub const WRITE_ACTIONS_FIXTURE: &str = "src/hooks/ui-bridge-events/writeActions.generated.json";

/// Whether `action` MUTATES input state. A recovery step may never dispatch one.
///
/// Recovery exists to make a previously-addressed action possible again —
/// scroll it into view, wait for it to enable, refresh stale refs. Writing on
/// the caller behalf is not that: it invents input the caller never asked
/// for, and (before the scoping fix) could land it on an element the caller
/// never addressed. Repositioning stays allowed; writes do not.
pub fn is_write_action(action: &str) -> bool {
    let a = action.trim().to_ascii_lowercase();
    WRITE_ACTIONS.contains(&a.as_str())
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
            // The escape hatch is the one Tier-1 command that can name an
            // arbitrary action, so it is where an auto-write would get in.
            if is_write_action(action) {
                warn!(
                    "recovery_executor: refusing write-class recovery action '{}' — \
                     recovery never writes on the caller behalf",
                    action
                );
                return Err(format!(
                    "recovery refused: '{action}' mutates input state; recovery may not write"
                ));
            }
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

/// The OUTER recovery verdict, shared by both tiers: given the result of
/// retrying the ORIGINAL action, `Some(result)` iff it now succeeds. This — not
/// whether a recovery step (or the frontend's scoped attempt) answered — is
/// what sets [`RecoveryOutcome::recovered`].
fn original_action_now_succeeds(
    retry: Result<serde_json::Value, String>,
) -> Option<serde_json::Value> {
    retry.ok().filter(is_ipc_success)
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
    element_id: &str,
) -> Result<serde_json::Value, String> {
    // `elementId` is REQUIRED, not decorative: the frontend handler scopes its
    // element search to this id and refuses to run unscoped. Without it the
    // executor searched every discovered element and could act on one the
    // caller never addressed.
    let payload = serde_json::json!({
        "params": { "instruction": instruction, "elementId": element_id }
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
                match original_action_now_succeeds(
                    ui_bridge_request_sync(state, "execute_action", original_action.clone()).await,
                ) {
                    Some(retry) => {
                        outcome.recovered = true;
                        outcome.via = RecoveryVia::StructuredCommand;
                        outcome.command_chosen = chosen_command.clone();
                        outcome.result = Some(retry);
                        emit_telemetry(state, &error_code, &chosen_command, true, task_run_id)
                            .await;
                        return outcome;
                    }
                    None => {
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

    // ── Tier 2: existing LLM-driven recovery path (opt-in) ───────────────
    outcome.command_chosen = chosen_command.clone();
    if !llm_recovery_enabled() {
        info!(
            "recovery_executor: LLM fallback disabled (set {}=1 to enable); \
             reporting the original failure for {:?}",
            LLM_RECOVERY_ENV, error_code
        );
        outcome.via = RecoveryVia::None;
        emit_telemetry(state, &error_code, &chosen_command, false, task_run_id).await;
        return outcome;
    }

    let instruction = failure
        .get("error")
        .and_then(|v| v.as_str())
        .map(|s| format!("recover from: {}", s))
        .unwrap_or_else(|| "recover from error state".to_string());
    match llm_fallback(state, &instruction, element_id).await {
        Ok(resp) => {
            // `recovered` is NOT "the recovery IPC answered". It is "the
            // original action now succeeds" — the same bar Tier 1 clears
            // above. Hardcoding it true reported success for an LLM step that
            // had touched an unrelated element and left the addressed action
            // just as broken as before.
            let retry = original_action_now_succeeds(
                ui_bridge_request_sync(state, "execute_action", original_action.clone()).await,
            );
            match retry {
                Some(retry) => {
                    outcome.recovered = true;
                    outcome.via = RecoveryVia::LlmFallback;
                    outcome.result = Some(retry);
                    emit_telemetry(state, &error_code, &chosen_command, true, task_run_id).await;
                }
                None => {
                    warn!(
                        "recovery_executor: LLM fallback ran but the original action still \
                         fails — reporting recovered:false ({:?})",
                        resp.get("error")
                    );
                    outcome.via = RecoveryVia::None;
                    emit_telemetry(state, &error_code, &chosen_command, false, task_run_id).await;
                }
            }
        }
        Err(e) => {
            warn!("recovery_executor: LLM fallback also failed: {}", e);
            outcome.via = RecoveryVia::None;
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

    /// The LLM fallback is OPT-IN. Unset — the state every runner ships in —
    /// must read as OFF, and so must every not-quite-yes spelling: a fallback
    /// that writes to elements the caller never addressed does not get to
    /// enable itself on a typo.
    #[test]
    fn llm_recovery_defaults_off_and_only_explicit_yes_enables_it() {
        assert!(!llm_recovery_flag_enabled(None), "unset is OFF");
        for off in ["", "  ", "0", "false", "no", "off", "maybe", "tru"] {
            assert!(
                !llm_recovery_flag_enabled(Some(off)),
                "{off:?} must not enable the LLM fallback"
            );
        }
        for on in ["1", "true", "TRUE", " yes ", "On"] {
            assert!(
                llm_recovery_flag_enabled(Some(on)),
                "{on:?} must enable the LLM fallback"
            );
        }
    }

    /// Recovery may reposition, wait, and resnapshot — it may never write.
    /// The `execute_action:<name>` escape hatch was the one Tier-1 command
    /// able to name an arbitrary action, i.e. the way an auto-write got in.
    #[test]
    fn write_class_actions_are_refused_as_recovery_steps() {
        for w in [
            "type",
            "setValue",
            "fill",
            "paste",
            "clear",
            "sendKeys",
            "writeToTerminal",
            "  TYPE  ",
        ] {
            assert!(is_write_action(w), "{w:?} mutates input state");
        }
        for ok in [
            "click",
            "focus",
            "blur",
            "scrollIntoView",
            "hover",
            "discover",
        ] {
            assert!(
                !is_write_action(ok),
                "{ok:?} is a repositioning action, not a write"
            );
        }
    }

    /// The frontend's `WRITE_ACTIONS` is imported from a JSON fixture
    /// generated from [`WRITE_ACTIONS`]. This is the drift gate: the fixture
    /// must equal the Rust declaration exactly (same members, same order), and
    /// the failure names the symmetric difference, so a string added to EITHER
    /// side alone — the Rust constant, or the fixture the frontend reads —
    /// fails here with the missing member named.
    #[test]
    fn write_actions_fixture_matches_rust_declaration() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(WRITE_ACTIONS_FIXTURE);
        let expected = format!(
            "{}\n",
            serde_json::to_string_pretty(WRITE_ACTIONS).expect("serialize WRITE_ACTIONS")
        );
        if std::env::var("QONTINUI_UPDATE_WRITE_ACTIONS").as_deref() == Ok("1") {
            std::fs::write(&path, &expected).expect("write write-actions fixture");
            return;
        }
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let fixture: Vec<String> =
            serde_json::from_str(&raw).expect("write-actions fixture is a JSON string array");
        let rust: std::collections::BTreeSet<&str> = WRITE_ACTIONS.iter().copied().collect();
        let json: std::collections::BTreeSet<&str> = fixture.iter().map(String::as_str).collect();
        let only_rust: Vec<_> = rust.difference(&json).collect();
        let only_json: Vec<_> = json.difference(&rust).collect();
        assert!(
            only_rust.is_empty() && only_json.is_empty(),
            "write-action lists diverge across the IPC seam — only in Rust \
             WRITE_ACTIONS: {only_rust:?}; only in {WRITE_ACTIONS_FIXTURE} (the \
             frontend's list): {only_json:?}. Regenerate the fixture with \
             QONTINUI_UPDATE_WRITE_ACTIONS=1 cargo test --bin qontinui-runner \
             write_actions_fixture_matches_rust_declaration"
        );
        assert_eq!(
            raw, expected,
            "{WRITE_ACTIONS_FIXTURE} has the right members but is not the generated \
             form (order/formatting); regenerate it"
        );
    }

    #[test]
    fn write_actions_are_lowercase_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for a in WRITE_ACTIONS {
            assert_eq!(
                *a,
                a.trim().to_ascii_lowercase(),
                "{a:?} must be normalized"
            );
            assert!(seen.insert(*a), "{a:?} is listed twice");
            assert!(is_write_action(a));
        }
    }

    /// Plan 2026-08-23-single-source-derived-facts item 8: the inner and outer
    /// verdicts answer different questions and MAY disagree — by design. A
    /// scoped attempt that acted on the addressed element (frontend
    /// `attemptSucceeded: true`, so `/ai/recovery/attempt` answers 200) while
    /// the original action still fails on retry is inner-true / outer-false.
    /// Pinned so nobody "fixes" it by unifying the two values.
    #[test]
    fn scoped_attempt_success_does_not_imply_recovered() {
        // Inner: the frontend's happy-path payload for the scoped attempt.
        let attempt = json!({ "attemptSucceeded": true, "elementId": "btn-1" });
        let inner = super::super::ai_analyze::as_recovery_failure(Ok(axum::Json(
            crate::mcp::types::ApiResponse::success(attempt.clone()),
        )));
        assert!(
            inner.is_ok(),
            "the scoped attempt succeeded, so the route answers 200"
        );
        assert_eq!(
            attempt[super::super::ai_analyze::ATTEMPT_SUCCEEDED],
            json!(true)
        );

        // Outer: the independent retry of the ORIGINAL action still fails.
        let retry_still_failing = Ok(json!({ "success": false, "error": "still disabled" }));
        assert!(
            original_action_now_succeeds(retry_still_failing).is_none(),
            "the original action is still broken, so RecoveryOutcome.recovered stays false"
        );

        // And the outer verdict does go true when the retry succeeds.
        assert!(original_action_now_succeeds(Ok(json!({ "success": true }))).is_some());
        assert!(original_action_now_succeeds(Err("ipc timeout".into())).is_none());
    }

    #[test]
    fn is_ipc_success_semantics() {
        assert!(is_ipc_success(&json!({ "success": true })));
        assert!(is_ipc_success(&json!({ "clicked": true }))); // absent == success
        assert!(!is_ipc_success(&json!({ "success": false })));
    }
}
