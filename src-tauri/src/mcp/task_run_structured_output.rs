//! Structured output and status-stream endpoints for task runs.
//!
//! Exposes:
//! - `GET /task-runs/{id}/output-structured` — parses the concatenated output
//!   blob produced by the unified-workflow executor into a structured shape
//!   (stages, steps, verification iterations) so callers don't have to
//!   regex-grep the raw log.
//! - `GET /task-runs/{id}/status-stream` — text/event-stream that polls
//!   `pg_db.get_task_run` every ~1.5s and emits an event whenever
//!   status / sessions_count / error_message change, then a terminal event
//!   when the task reaches a terminal state.
//!
//! Implementation notes:
//! - The structured-output parser is purely additive — it reads the existing
//!   `task_run.output_log` and never mutates state. PG remains source of truth.
//! - The status stream uses polling rather than wiring a new broadcast channel
//!   through every PG-write site. This is a deliberate v1 choice; it can be
//!   upgraded to a broadcast subscription later without API changes.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::Json;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::database::TaskRun;
use crate::mcp::types::{ApiResponse, ApiState};

// =============================================================================
// Structured output types
// =============================================================================

/// One execution step inside a stage's setup phase (or a parsed AI prompt
/// output). The shape mirrors the per-step bullets in the raw output:
///   `  [OK] prompt - Use the test-wrapper... (1234ms)`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredStep {
    /// `"prompt"`, `"command"`, `"ui_bridge"`, or any other string the
    /// executor wrote in the bullet block. Free-form on purpose — we don't
    /// want a parser change to break callers when the executor adds a new
    /// step kind.
    pub kind: String,
    /// Human-readable step name, e.g. the prompt title.
    pub name: String,
    /// Whether this step succeeded. `false` on `[FAIL]`, `true` on `[OK]`.
    pub success: bool,
    /// Duration in ms if the executor wrote one (the `(1234ms)` suffix).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Raw output for this step if we found one. For prompts we surface the
    /// `--- AI Setup Output (...) ---` body; for command/ui_bridge steps we
    /// usually have no body and this is None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// One stage in the workflow — between two `=== STAGE N/M: "..." ===`
/// markers in the raw output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredStage {
    /// The exact stage header text (without the `===` decorations) — e.g.
    /// `Stage 1/1: Use the test-wrapper`.
    pub name: String,
    /// Overall stage success if a `--- Stage N Setup ---\nSuccess: true|false`
    /// block was found. None if the stage didn't emit one (typical for
    /// stages that only run agentic/verification, not setup).
    pub success: Option<bool>,
    /// Steps parsed from the `--- Stage N Setup ---` bullet block, augmented
    /// with prompt-output bodies parsed from `--- AI Setup Output (...) ---`
    /// blocks that appeared in the same stage.
    pub steps: Vec<StructuredStep>,
    /// If any of the AI Setup Output bodies parsed cleanly as a UnifiedWorkflow
    /// JSON (i.e. `{"deterministic_steps": [...], "agentic_steps": [...]}`),
    /// we surface the FIRST such parsed body here. Callers that want every
    /// generated workflow can iterate `steps[*].output` and parse themselves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_workflow_json: Option<serde_json::Value>,
}

/// One iteration of the verification-agentic loop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerificationIteration {
    pub iteration: u32,
    pub passed: u32,
    pub failed: u32,
    pub total: u32,
    /// `"PASSED"` or `"FAILED"` — the literal status from the marker line.
    /// None if we found a `=== Verification-Agentic Loop: Iteration N ===`
    /// header but never the corresponding `--- Verification (Iteration N): ...`
    /// summary (e.g. iteration is still in flight).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// Top-level structured-output response body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredOutput {
    pub stages: Vec<StructuredStage>,
    pub verification_iterations: Vec<VerificationIteration>,
    /// Length of the raw output_log we parsed. Lets callers fall back to
    /// `/task-runs/{id}/output` if they suspect the parser missed something.
    pub raw_output_length: usize,
}

// =============================================================================
// Parser
// =============================================================================

// Pre-compiled regexes — module-static so we don't pay compile cost per call.
static RE_STAGE_HEADER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"=== STAGE (\d+)/(\d+): "([^"]*)" ==="#).expect("stage header regex compiles")
});

static RE_AI_SETUP_OUTPUT: Lazy<Regex> = Lazy::new(|| {
    // Capture group 1: step name; group 2: body (greedy until next marker
    // line or end of stage block). We bound with a non-greedy match plus a
    // positive look-ahead is unavailable in `regex`; instead we capture
    // until the next `\n--- ` or `\n=== ` (or end of haystack).
    Regex::new(r"(?ms)^--- AI Setup Output \(([^)]+)\) ---\n(.*?)(?:\n(?:--- |=== )|\z)")
        .expect("ai setup output regex compiles")
});

static RE_STAGE_SETUP_HEADER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"--- Stage (\d+) Setup ---\nSteps: (\d+)\nSuccess: (true|false)\n")
        .expect("stage setup header regex compiles")
});

static RE_SETUP_BULLET: Lazy<Regex> = Lazy::new(|| {
    // Matches one bullet line:  `  [OK] prompt - Use foo (1234ms)`
    // Group 1: OK|FAIL,  group 2: kind,  group 3: name,  group 4: ms
    Regex::new(r"  \[(OK|FAIL)\] ([^\s-][^\s]*) - (.+?) \((\d+)ms\)")
        .expect("setup bullet regex compiles")
});

static RE_VERIFICATION_HEADER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"=== Verification-Agentic Loop: Iteration (\d+) ===")
        .expect("verification header regex compiles")
});

static RE_VERIFICATION_SUMMARY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"--- Verification \(Iteration (\d+)\): (PASSED|FAILED) \((\d+) passed, (\d+) failed, (\d+) total\) ---",
    )
    .expect("verification summary regex compiles")
});

/// Parse the raw concatenated `output_log` blob produced by the unified
/// workflow executor into a structured shape.
///
/// The parser is intentionally defensive:
/// - Empty input → empty stages and iterations.
/// - Malformed JSON inside an AI Setup Output body → step appears with
///   `output` set, but `generated_workflow_json` remains None.
/// - Unrecognized sections → silently skipped.
/// - A single malformed segment must never panic the whole parse.
pub fn parse_structured_output(raw: &str) -> StructuredOutput {
    let raw_output_length = raw.len();
    if raw.is_empty() {
        return StructuredOutput {
            stages: Vec::new(),
            verification_iterations: Vec::new(),
            raw_output_length,
        };
    }

    // -------- 1. Slice the raw blob into stage segments. --------
    //
    // A "stage segment" runs from one `=== STAGE N/M: "..." ===` marker up
    // to (but not including) the next stage marker, or end of input. If
    // the blob has no stage markers at all, we still return an empty
    // `stages` array — output before the first stage marker is implicit
    // setup/scaffolding and not part of the structured shape.

    let stage_match_locs: Vec<(usize, usize, String)> = RE_STAGE_HEADER
        .captures_iter(raw)
        .filter_map(|cap| {
            let m = cap.get(0)?;
            let header_text = format!(
                "Stage {}/{}: {}",
                cap.get(1)?.as_str(),
                cap.get(2)?.as_str(),
                cap.get(3)?.as_str(),
            );
            Some((m.start(), m.end(), header_text))
        })
        .collect();

    let mut stages: Vec<StructuredStage> = Vec::new();
    for i in 0..stage_match_locs.len() {
        let (_, header_end, ref name) = stage_match_locs[i];
        let segment_end = stage_match_locs
            .get(i + 1)
            .map(|next| next.0)
            .unwrap_or(raw.len());
        let segment = &raw[header_end..segment_end];
        stages.push(parse_stage_segment(name.clone(), segment));
    }

    // -------- 2. Parse verification-agentic loop iterations globally. --------
    //
    // Verification iterations are NOT stage-scoped in the raw output (the
    // header `=== Verification-Agentic Loop: Iteration N ===` doesn't
    // include a stage prefix), so we collect them across the whole blob.

    let mut verification_iterations: Vec<VerificationIteration> = Vec::new();
    for cap in RE_VERIFICATION_HEADER.captures_iter(raw) {
        let iter_num = cap
            .get(1)
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(0);
        verification_iterations.push(VerificationIteration {
            iteration: iter_num,
            passed: 0,
            failed: 0,
            total: 0,
            details: None,
        });
    }

    // Now fold in the summary lines. We match by iteration number rather
    // than positional order so out-of-order summaries (rare but possible
    // on retry) still apply correctly.
    for cap in RE_VERIFICATION_SUMMARY.captures_iter(raw) {
        let Some(iter_num) = cap.get(1).and_then(|m| m.as_str().parse::<u32>().ok()) else {
            continue;
        };
        let status = cap.get(2).map(|m| m.as_str().to_string());
        let passed = cap
            .get(3)
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(0);
        let failed = cap
            .get(4)
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(0);
        let total = cap
            .get(5)
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(0);

        if let Some(it) = verification_iterations
            .iter_mut()
            .find(|v| v.iteration == iter_num)
        {
            it.passed = passed;
            it.failed = failed;
            it.total = total;
            it.details = status;
        } else {
            // Summary without a header — surface anyway so callers see it.
            verification_iterations.push(VerificationIteration {
                iteration: iter_num,
                passed,
                failed,
                total,
                details: status,
            });
        }
    }

    StructuredOutput {
        stages,
        verification_iterations,
        raw_output_length,
    }
}

/// Parse a single stage segment (text between two stage headers).
fn parse_stage_segment(name: String, segment: &str) -> StructuredStage {
    let mut steps: Vec<StructuredStep> = Vec::new();
    let mut success: Option<bool> = None;
    let mut generated_workflow_json: Option<serde_json::Value> = None;

    // -------- 1. Pull AI Setup Output blocks (prompt step bodies). --------
    //
    // We index them by step name (group 1) so we can attach the body to
    // the matching bullet later. Bullets carry the canonical kind/duration
    // metadata; the AI Setup Output blocks carry the actual output text.
    use std::collections::HashMap;
    let mut prompt_bodies: HashMap<String, String> = HashMap::new();
    for cap in RE_AI_SETUP_OUTPUT.captures_iter(segment) {
        let Some(step_name) = cap.get(1).map(|m| m.as_str().to_string()) else {
            continue;
        };
        let body = cap.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
        // Try to parse as a UnifiedWorkflow JSON. If it looks like one
        // (`deterministic_steps` array present), surface as the stage's
        // `generated_workflow_json` (first one wins).
        if generated_workflow_json.is_none() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(body.trim()) {
                if v.get("deterministic_steps").is_some() || v.get("agentic_steps").is_some() {
                    generated_workflow_json = Some(v);
                }
            }
        }
        prompt_bodies.insert(step_name, body);
    }

    // -------- 2. Parse the `--- Stage N Setup ---` block. --------
    if let Some(header_cap) = RE_STAGE_SETUP_HEADER.captures(segment) {
        success = header_cap
            .get(3)
            .map(|m| m.as_str() == "true");

        // Bullets follow the header. Find the byte offset of the line after
        // the header and walk bullet lines until we hit one that doesn't
        // match (or end-of-segment). We bound the scan to whatever comes
        // before the next `--- ` or `=== ` marker to avoid eating into the
        // next section.
        let header_end = header_cap.get(0).map(|m| m.end()).unwrap_or(0);
        let bullets_region_end = segment[header_end..]
            .find("\n--- ")
            .or_else(|| segment[header_end..].find("\n=== "))
            .map(|rel| header_end + rel)
            .unwrap_or(segment.len());
        let bullets_region = &segment[header_end..bullets_region_end];

        for cap in RE_SETUP_BULLET.captures_iter(bullets_region) {
            let ok = cap.get(1).map(|m| m.as_str() == "OK").unwrap_or(false);
            let kind = cap
                .get(2)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let step_name = cap
                .get(3)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let duration_ms = cap.get(4).and_then(|m| m.as_str().parse::<u64>().ok());
            let output = prompt_bodies.remove(&step_name);

            steps.push(StructuredStep {
                kind,
                name: step_name,
                success: ok,
                duration_ms,
                output,
            });
        }
    }

    // -------- 3. Surface any prompt bodies that didn't get attached to a
    // bullet (e.g. setup block was missing/malformed). They become bare
    // prompt steps with success=true (we have no signal otherwise).
    for (step_name, body) in prompt_bodies.into_iter() {
        steps.push(StructuredStep {
            kind: "prompt".to_string(),
            name: step_name,
            success: true,
            duration_ms: None,
            output: Some(body),
        });
    }

    StructuredStage {
        name,
        success,
        steps,
        generated_workflow_json,
    }
}

// =============================================================================
// HTTP handler — GET /task-runs/{id}/output-structured
// =============================================================================

/// Returns a parsed view of the task run's output_log.
pub async fn get_task_output_structured(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<StructuredOutput>>, (StatusCode, String)> {
    // Verify the task exists and grab its output in one PG roundtrip.
    let task_run = state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let parsed = parse_structured_output(&task_run.output_log);
    Ok(Json(ApiResponse::success(parsed)))
}

// =============================================================================
// SSE handler — GET /task-runs/{id}/status-stream
// =============================================================================

/// Snapshot of fields we watch for changes. Equality is the diff signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusSnapshot {
    status: String,
    sessions_count: u32,
    error_message: Option<String>,
    updated_at: String,
}

impl StatusSnapshot {
    fn from_task_run(tr: &TaskRun) -> Self {
        Self {
            status: tr.status.clone(),
            sessions_count: tr.sessions_count,
            error_message: tr.error_message.clone(),
            updated_at: tr.updated_at.clone(),
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "completed" | "complete" | "failed" | "stopped" | "error" | "cancelled"
        )
    }

    fn to_event_data(&self) -> serde_json::Value {
        serde_json::json!({
            "status": self.status,
            "sessions_count": self.sessions_count,
            "error_message": self.error_message,
            "updated_at": self.updated_at,
        })
    }
}

/// Pure diff helper. Returns `Some(Event)` if a status event should be
/// emitted (state changed or first emit), `None` if nothing has changed.
///
/// Extracted so we can unit-test the diff logic without spinning up a
/// full SSE pipeline.
pub(crate) fn diff_task_state(
    prev: Option<&StatusSnapshot>,
    curr: &StatusSnapshot,
) -> Option<Event> {
    let changed = match prev {
        None => true,
        Some(p) => p != curr,
    };
    if !changed {
        return None;
    }
    let data = curr.to_event_data().to_string();
    Some(Event::default().event("status").data(data))
}

/// Build the terminal event for a finished task.
fn build_terminal_event(snapshot: &StatusSnapshot) -> Event {
    let data = snapshot.to_event_data().to_string();
    Event::default().event("terminal").data(data)
}

/// SSE stream of task-run status changes. Polls PG every 1.5s, emits an
/// initial event immediately, then one event per change, and finally a
/// `terminal` event when the task reaches a terminal state. Capped at 30
/// minutes total stream lifetime.
pub async fn status_stream(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<
    Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>>,
    (StatusCode, String),
> {
    // Verify the task exists up front — gives a real 404 instead of an
    // SSE that emits nothing.
    let _ = state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let task_id = id.clone();
    let pg_db = state.app_state.pg_db.clone();
    let max_lifetime = Duration::from_secs(30 * 60);
    let poll_interval = Duration::from_millis(1500);

    let stream = async_stream::stream! {
        let started_at = std::time::Instant::now();
        let mut prev: Option<StatusSnapshot> = None;

        loop {
            // Hard cap on total lifetime — protects against forgotten clients.
            if started_at.elapsed() >= max_lifetime {
                let timeout_event = Event::default()
                    .event("timeout")
                    .data(r#"{"reason":"30min stream lifetime exceeded"}"#);
                yield Ok::<_, std::convert::Infallible>(timeout_event);
                break;
            }

            match pg_db.get_task_run(&task_id).await {
                Ok(Some(tr)) => {
                    let snap = StatusSnapshot::from_task_run(&tr);
                    if let Some(ev) = diff_task_state(prev.as_ref(), &snap) {
                        yield Ok(ev);
                    }
                    if snap.is_terminal() {
                        yield Ok(build_terminal_event(&snap));
                        break;
                    }
                    prev = Some(snap);
                }
                Ok(None) => {
                    // Task disappeared between handler verification and now.
                    let err_event = Event::default()
                        .event("error")
                        .data(r#"{"error":"task run no longer exists"}"#);
                    yield Ok(err_event);
                    break;
                }
                Err(e) => {
                    let payload = serde_json::json!({ "error": e }).to_string();
                    let err_event = Event::default().event("error").data(payload);
                    yield Ok(err_event);
                    break;
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue 1, test (1): empty input → empty stages and iterations.
    #[test]
    fn parse_empty_input_returns_empty() {
        let parsed = parse_structured_output("");
        assert!(parsed.stages.is_empty());
        assert!(parsed.verification_iterations.is_empty());
        assert_eq!(parsed.raw_output_length, 0);
    }

    /// Issue 1, test (2): real-shape input parses into the documented shape.
    #[test]
    fn parse_real_shape_input() {
        // Composed from the actual format strings used by the executor:
        //  - loop_controller.rs:882   "\n\n{}\n=== STAGE {}/{}: \"{}\" ===\n{}\n"
        //  - phases/setup.rs:359      "\n--- AI Setup Output ({}) ---\n{}\n"
        //  - loop_controller.rs:970   "\n--- Stage {} Setup ---\nSteps: {}\nSuccess: {}\n"
        //  - loop_controller.rs:977   "  [{}] {} - {} ({}ms)\n"
        //  - loop_handlers.rs:491     "\n\n=== Verification-Agentic Loop: Iteration {} ===\n"
        //  - loop_handlers.rs:887     "\n--- Verification (Iteration {}): {} ({} passed, {} failed, {} total) ---\n"
        let raw = "\n\n============================================================\n\
            === STAGE 1/1: \"Use the test-wrapper\" ===\n\
            ============================================================\n\
            \n--- AI Setup Output (Use the test-wrapper) ---\n\
            {\"deterministic_steps\":[],\"agentic_steps\":[{\"prompt\":\"do thing\"}]}\n\
            \n--- Stage 1 Setup ---\n\
            Steps: 1\n\
            Success: true\n\
            \x20\x20[OK] prompt - Use the test-wrapper (1234ms)\n\
            \n\n=== Verification-Agentic Loop: Iteration 1 ===\n\
            \n--- Verification (Iteration 1): PASSED (3 passed, 0 failed, 3 total) ---\n";

        let parsed = parse_structured_output(raw);
        assert_eq!(parsed.stages.len(), 1, "expected 1 stage, got {}", parsed.stages.len());
        let stage = &parsed.stages[0];
        assert_eq!(stage.name, "Stage 1/1: Use the test-wrapper");
        assert_eq!(stage.success, Some(true));
        assert_eq!(stage.steps.len(), 1, "expected 1 step in stage");
        let step = &stage.steps[0];
        assert_eq!(step.kind, "prompt");
        assert_eq!(step.name, "Use the test-wrapper");
        assert!(step.success);
        assert_eq!(step.duration_ms, Some(1234));
        assert!(
            step.output.as_deref().unwrap_or("").contains("agentic_steps"),
            "expected step output to carry the AI Setup Output body"
        );
        assert!(
            stage.generated_workflow_json.is_some(),
            "expected the JSON-shaped AI body to be surfaced as generated_workflow_json"
        );

        assert_eq!(parsed.verification_iterations.len(), 1);
        let it = &parsed.verification_iterations[0];
        assert_eq!(it.iteration, 1);
        assert_eq!(it.passed, 3);
        assert_eq!(it.failed, 0);
        assert_eq!(it.total, 3);
        assert_eq!(it.details.as_deref(), Some("PASSED"));
    }

    /// Issue 1, test (3): malformed JSON inside the AI Setup Output body
    /// still surfaces the step (with output set) but skips the
    /// generated_workflow_json field. Must not panic.
    #[test]
    fn parse_malformed_ai_setup_output_does_not_panic() {
        let raw = "=== STAGE 1/1: \"Try malformed\" ===\n\
            \n--- AI Setup Output (Try malformed) ---\n\
            this { is not :: valid json at all }}}}\n\
            \n--- Stage 1 Setup ---\n\
            Steps: 1\n\
            Success: false\n\
            \x20\x20[FAIL] prompt - Try malformed (50ms)\n";
        let parsed = parse_structured_output(raw);
        assert_eq!(parsed.stages.len(), 1);
        let stage = &parsed.stages[0];
        assert_eq!(stage.success, Some(false));
        assert_eq!(stage.steps.len(), 1);
        assert!(!stage.steps[0].success);
        assert!(stage.steps[0].output.is_some(), "output body should still be attached");
        assert!(
            stage.generated_workflow_json.is_none(),
            "malformed JSON must not surface as generated_workflow_json"
        );
    }

    /// Issue 1, test (4): multi-iteration verification block — every
    /// iteration is parsed, including ones that have only a header but no
    /// summary yet (in-flight).
    #[test]
    fn parse_multi_iteration_verification() {
        let raw = "=== STAGE 1/1: \"x\" ===\n\
            \n\n=== Verification-Agentic Loop: Iteration 1 ===\n\
            \n--- Verification (Iteration 1): FAILED (1 passed, 2 failed, 3 total) ---\n\
            \n\n=== Verification-Agentic Loop: Iteration 2 ===\n\
            \n--- Verification (Iteration 2): FAILED (2 passed, 1 failed, 3 total) ---\n\
            \n\n=== Verification-Agentic Loop: Iteration 3 ===\n\
            \n--- Verification (Iteration 3): PASSED (3 passed, 0 failed, 3 total) ---\n";
        let parsed = parse_structured_output(raw);
        assert_eq!(parsed.verification_iterations.len(), 3);
        let i1 = &parsed.verification_iterations[0];
        assert_eq!(i1.iteration, 1);
        assert_eq!(i1.passed, 1);
        assert_eq!(i1.failed, 2);
        assert_eq!(i1.details.as_deref(), Some("FAILED"));
        let i3 = &parsed.verification_iterations[2];
        assert_eq!(i3.iteration, 3);
        assert_eq!(i3.passed, 3);
        assert_eq!(i3.details.as_deref(), Some("PASSED"));
    }

    /// Issue 1, edge case: a verification header with no matching summary
    /// (iteration still in flight) parses with default zeros and no details.
    #[test]
    fn parse_in_flight_verification_iteration() {
        let raw = "=== Verification-Agentic Loop: Iteration 1 ===\n";
        let parsed = parse_structured_output(raw);
        assert_eq!(parsed.verification_iterations.len(), 1);
        let it = &parsed.verification_iterations[0];
        assert_eq!(it.iteration, 1);
        assert_eq!(it.passed, 0);
        assert_eq!(it.failed, 0);
        assert_eq!(it.total, 0);
        assert!(it.details.is_none());
    }

    /// Issue 2: diff helper emits an event on first call, suppresses no-ops,
    /// and emits again when state actually changes.
    #[test]
    fn diff_task_state_emits_on_first_call_and_change() {
        let s1 = StatusSnapshot {
            status: "running".to_string(),
            sessions_count: 1,
            error_message: None,
            updated_at: "2026-05-01T00:00:00Z".to_string(),
        };
        // First call (no prev) → must emit.
        assert!(
            diff_task_state(None, &s1).is_some(),
            "first call must emit an initial state event"
        );
        // Same snapshot → must NOT emit.
        assert!(
            diff_task_state(Some(&s1), &s1).is_none(),
            "no-change must not emit"
        );
        // Sessions_count changed → must emit.
        let s2 = StatusSnapshot {
            sessions_count: 2,
            ..s1.clone()
        };
        assert!(
            diff_task_state(Some(&s1), &s2).is_some(),
            "sessions_count change must emit"
        );
        // Status changed → must emit.
        let s3 = StatusSnapshot {
            status: "completed".to_string(),
            ..s1.clone()
        };
        assert!(diff_task_state(Some(&s1), &s3).is_some());
    }

    #[test]
    fn snapshot_terminal_detection() {
        for status in &["completed", "failed", "stopped", "error", "cancelled"] {
            let s = StatusSnapshot {
                status: status.to_string(),
                sessions_count: 0,
                error_message: None,
                updated_at: String::new(),
            };
            assert!(s.is_terminal(), "{} must be terminal", status);
        }
        let s = StatusSnapshot {
            status: "running".to_string(),
            sessions_count: 0,
            error_message: None,
            updated_at: String::new(),
        };
        assert!(!s.is_terminal());
    }
}
