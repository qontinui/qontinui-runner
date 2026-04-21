//! Script emitter for the "think-in-code" step-output indirection.
//!
//! When a TypeScript step handler's stdout exceeds ~8 KB, the
//! `ScriptedOutputHandler` base class (see
//! `qontinui-runner/src/lib/step-output-handlers/scripted-base.ts`) calls
//! out to this module via the `emit_extraction_script` Tauri command to
//! synthesise a one-line JavaScript expression that extracts only the
//! fields the downstream summariser actually needs.
//!
//! # Behaviour summary (Phase A of the script-emitter-wiring plan)
//!
//! - **Global kill switch.** Both the `QONTINUI_SCRIPTED_OUTPUT` env var
//!   (must be `"1"`) and the `scripted_output.enabled` settings flag must be
//!   on. Either one off returns [`EmitError::Disabled`] without touching
//!   the LLM.
//! - **Tier 1 LRU cache** (200 entries). Key =
//!   `sha256(goal + canonical(schemaHint) + outputPreview[..2048])`. Cache
//!   hits return immediately with `source = "cache"`.
//! - **Per-task_run cost ceiling.** 20 emitter calls per `task_run_id`, and
//!   a 10 000 input + 2 000 output token budget. Budgets are checked
//!   *before* the LLM call and only consumed on success — a rejected call
//!   never spends budget.
//! - **3-second LLM timeout** via `tokio::time::timeout`.
//! - **Circuit breaker reuse.** The inner `run_prompt_*` path consults the
//!   breaker against whichever provider the runner is configured to use
//!   (claude_cli, claude_api, …); we classify a breaker-open response by
//!   error-text match and return [`EmitError::BreakerOpen`]. We don't know
//!   the provider at call-site so we can't pre-check against the right one.
//! - **Telemetry.** Emits three activity-timeline events into `runner`
//!   scope: `scripted_output.cache_hit`, `scripted_output.llm_ok`,
//!   `scripted_output.fallback{reason}`. The TS side emits the other three
//!   (`attempted`, `worker_ok`, `bytes_avoided`).

use std::num::NonZeroUsize;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use lru::LruCache;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::ai_provider::routing::run_prompt_with_structured_output;
use crate::database::pg::PgDb;
use crate::database::types::ActivityTimelineInput;

// ============================================================================
// Tunables (per plan)
// ============================================================================

/// Default model — Haiku is the cheapest competent choice for a
/// "return a one-line JS expression" task. Override via the
/// `scripted_output.model` settings field.
pub const DEFAULT_MODEL: &str = "claude-haiku-4-5-20251001";

/// Tier 1 cache capacity. Small enough to keep memory trivial, large enough
/// to cover a workflow hammering the same step type dozens of times.
const TIER1_CAPACITY: usize = 200;

/// Max emitter calls per task_run.
const MAX_CALLS_PER_TASK_RUN: u32 = 20;

/// Max input tokens summed across all emitter calls for a task_run.
const MAX_INPUT_TOKENS_PER_TASK_RUN: u32 = 10_000;

/// Max output tokens summed across all emitter calls for a task_run.
const MAX_OUTPUT_TOKENS_PER_TASK_RUN: u32 = 2_000;

/// LLM call timeout. Deliberately tight so a slow model loses to the
/// truncation fallback rather than blocking the hot path.
const LLM_TIMEOUT: Duration = Duration::from_secs(3);

/// Only the first N bytes of the output preview contribute to the cache
/// key. Keeps keys stable across runs that differ only in tail data.
const CACHE_KEY_PREVIEW_BYTES: usize = 2048;

/// Env-var kill switch. When unset or "0", all calls return `Disabled`.
const ENV_KILL_SWITCH: &str = "QONTINUI_SCRIPTED_OUTPUT";

// ============================================================================
// Public types
// ============================================================================

/// Successful emitter result. Mirrors the TypeScript `EmitResult` shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmitResult {
    /// The synthesised one-line JS expression. Operates on a variable
    /// named `output` (the raw stdout string, matching the name the TS
    /// `script-worker.ts` sandbox binds) and must return a
    /// JSON-serialisable value.
    pub expression: String,
    /// Model that produced the expression (or produced the cached entry).
    pub model_id: String,
    /// Input tokens consumed on this specific call (0 for cache hits).
    pub tokens_in: u32,
    /// Output tokens consumed on this specific call (0 for cache hits).
    pub tokens_out: u32,
    /// Where the expression came from: `"cache"` or `"llm"`.
    pub source: String,
    /// Which cache tier served this (only Tier 1 exists today). `None` for
    /// LLM responses.
    pub cache_tier: Option<u8>,
}

/// Per-task_run running counters exposed via [`get_task_run_stats`] for
/// the `/task_runs/:id` endpoint (wiring TODO, see bottom of this file).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRunEmitterStats {
    pub calls: u32,
    pub tokens_in: u32,
    pub tokens_out: u32,
}

/// Categorised failure modes. The TS shim maps every variant onto the
/// truncation fallback path; this exists so telemetry can distinguish
/// them.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EmitError {
    /// Too many emitter calls already spent on this task_run.
    #[error("task_run call cap exceeded ({cap})")]
    CostCap { cap: u32 },
    /// Input or output token budget for this task_run exhausted.
    #[error("task_run token budget exhausted (dimension={dimension})")]
    TokenBudget { dimension: String },
    /// LLM call exceeded [`LLM_TIMEOUT`].
    #[error("LLM call timed out after {seconds}s")]
    Timeout { seconds: u64 },
    /// The shared Claude API circuit breaker is Open.
    #[error("LLM circuit breaker is open")]
    BreakerOpen,
    /// The global kill switch (env var or settings flag) is off.
    #[error("scripted output path is disabled")]
    Disabled,
    /// LLM returned an error response (network, auth, etc.).
    #[error("LLM error: {0}")]
    LlmError(String),
    /// LLM response didn't include a usable `expression` field.
    #[error("invalid LLM response: {0}")]
    InvalidResponse(String),
}

// ============================================================================
// State: cache + per-run stats
// ============================================================================

fn tier1_cache() -> &'static Mutex<LruCache<String, CachedEntry>> {
    static CACHE: OnceLock<Mutex<LruCache<String, CachedEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(LruCache::new(
            NonZeroUsize::new(TIER1_CAPACITY).expect("TIER1_CAPACITY is non-zero"),
        ))
    })
}

fn task_run_stats_map() -> &'static DashMap<String, TaskRunEmitterStats> {
    static MAP: OnceLock<DashMap<String, TaskRunEmitterStats>> = OnceLock::new();
    MAP.get_or_init(DashMap::new)
}

#[derive(Clone)]
struct CachedEntry {
    expression: String,
    model_id: String,
    #[allow(dead_code)] // surfaced by tests only for now
    created_at_epoch_s: u64,
}

/// Accessor for the per-task_run running counters. Safe to call from any
/// context; returns `None` for unknown task_run_ids.
///
/// **Phase B reference:** the `/task_runs/:id` handler should call this
/// and merge the result into its JSON response as `emitter_stats`.
pub fn get_task_run_stats(task_run_id: &str) -> Option<TaskRunEmitterStats> {
    task_run_stats_map()
        .get(task_run_id)
        .map(|r| r.value().clone())
}


// ============================================================================
// Entry point
// ============================================================================

/// Synthesise (or cache-hit) an extraction expression.
///
/// See module docs for full behaviour. All failures are returned as
/// [`EmitError`] — the caller (Tauri command wrapper) maps those to
/// a `Result<_, String>` for the frontend.
pub async fn emit_extraction_script(
    goal: String,
    schema_hint: serde_json::Value,
    output_preview: String,
    task_run_id: Option<String>,
) -> Result<EmitResult, EmitError> {
    // -------------------------------------------------------------- 1. Gates
    if !env_kill_switch_on() {
        debug!("scripted-output emitter disabled via env kill switch");
        emit_fallback_event(&task_run_id, "disabled");
        return Err(EmitError::Disabled);
    }
    if !settings_flag_on() {
        debug!("scripted-output emitter disabled via settings flag");
        emit_fallback_event(&task_run_id, "disabled");
        return Err(EmitError::Disabled);
    }

    // -------------------------------------------------------- 2. Tier 1 cache
    let cache_key = build_cache_key(&goal, &schema_hint, &output_preview);
    if let Some(hit) = lookup_cached(&cache_key) {
        info!("scripted-output emitter: Tier 1 cache hit");
        emit_cache_hit_event(&task_run_id);
        return Ok(EmitResult {
            expression: hit.expression,
            model_id: hit.model_id,
            tokens_in: 0,
            tokens_out: 0,
            source: "cache".to_string(),
            cache_tier: Some(1),
        });
    }

    // -------------------------------------------- 3. Per-task_run cost ceiling
    // Check budget BEFORE consuming any of it. Rejected calls don't spend.
    if let Some(ref tr) = task_run_id {
        if let Err(kind) = check_budget_available(tr) {
            match kind {
                BudgetRejection::CallCap => {
                    emit_fallback_event(&task_run_id, "cost_cap");
                    return Err(EmitError::CostCap {
                        cap: MAX_CALLS_PER_TASK_RUN,
                    });
                }
                BudgetRejection::InputTokens => {
                    emit_fallback_event(&task_run_id, "token_budget");
                    return Err(EmitError::TokenBudget {
                        dimension: "input".to_string(),
                    });
                }
                BudgetRejection::OutputTokens => {
                    emit_fallback_event(&task_run_id, "token_budget");
                    return Err(EmitError::TokenBudget {
                        dimension: "output".to_string(),
                    });
                }
            }
        }
    }

    // -------------------------------------------------------- 4. Breaker peek
    // We don't know which provider the runner is configured to use
    // (claude_cli, claude_api, …) until `run_prompt_with_structured_output`
    // resolves it from settings. The inner path does its own breaker check
    // against the right provider — we classify a `BreakerOpen` result by
    // error-text match below rather than pre-checking the wrong provider
    // and masking the real state.

    // ------------------------------------------------------- 5. Model + prompt
    let model = resolve_model();
    let schema = response_schema();
    let system_and_user = build_prompt(&goal, &schema_hint, &output_preview);

    // ---------------------------------------------- 6. Issue LLM call w/ timeout
    // `run_prompt_with_structured_output` is blocking; wrap in spawn_blocking
    // as the rest of the codebase does.
    let model_owned = model.clone();
    let llm_call = async move {
        let model_for_task = model_owned.clone();
        tokio::task::spawn_blocking(move || {
            run_prompt_with_structured_output(
                &system_and_user,
                &schema,
                "scripted_output_extraction",
                None, // no Doctor handle — this is a short synthetic call
                Some(&model_for_task),
                None, // let the runner's configured AI provider decide (claude_cli, claude_api, …)
                Some(0.0),      // deterministic
                Some(512),      // cap: expression is < 512 tokens always
            )
        })
        .await
    };

    let ai_response = match timeout(LLM_TIMEOUT, llm_call).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(join_err)) => {
            warn!("scripted-output emitter: spawn_blocking join error: {}", join_err);
            emit_fallback_event(&task_run_id, "emitter_error");
            return Err(EmitError::LlmError(format!(
                "spawn_blocking join error: {}",
                join_err
            )));
        }
        Err(_elapsed) => {
            warn!(
                "scripted-output emitter: LLM call timed out after {:?}",
                LLM_TIMEOUT
            );
            emit_fallback_event(&task_run_id, "timeout");
            return Err(EmitError::Timeout {
                seconds: LLM_TIMEOUT.as_secs(),
            });
        }
    };

    if !ai_response.success {
        let err_text = ai_response.error.clone().unwrap_or_default();
        // Heuristic: the retry layer's breaker rejections use this marker
        // in their error message. When the breaker opened *during* our
        // call, we want the dedicated `breaker_open` event.
        if err_text.contains("circuit breaker is open") || err_text.contains("Breaker open") {
            emit_fallback_event(&task_run_id, "breaker_open");
            return Err(EmitError::BreakerOpen);
        }
        warn!(
            "scripted-output emitter: LLM returned error: {}",
            truncate_for_log(&err_text)
        );
        emit_fallback_event(&task_run_id, "emitter_error");
        return Err(EmitError::LlmError(err_text));
    }

    // ------------------------------------------------- 7. Parse & validate
    let expression = match extract_expression(&ai_response.output) {
        Ok(e) => e,
        Err(why) => {
            warn!("scripted-output emitter: invalid LLM response: {}", why);
            emit_fallback_event(&task_run_id, "emitter_error");
            return Err(EmitError::InvalidResponse(why));
        }
    };

    let tokens_in = ai_response.input_tokens.unwrap_or(0) as u32;
    let tokens_out = ai_response.output_tokens.unwrap_or(0) as u32;

    // ----------------------------------------------- 8. Record budget spend
    if let Some(ref tr) = task_run_id {
        consume_budget(tr, tokens_in, tokens_out);
    }

    // ----------------------------------------- 9. Cache the successful result
    store_cached(&cache_key, &expression, &model);

    // ------------------------------------------------ 10. Emit success telemetry
    emit_llm_ok_event(&task_run_id, tokens_in, tokens_out, &model);

    Ok(EmitResult {
        expression,
        model_id: model,
        tokens_in,
        tokens_out,
        source: "llm".to_string(),
        cache_tier: None,
    })
}

// ============================================================================
// Budget helpers
// ============================================================================

enum BudgetRejection {
    CallCap,
    InputTokens,
    OutputTokens,
}

fn check_budget_available(task_run_id: &str) -> Result<(), BudgetRejection> {
    let map = task_run_stats_map();
    let entry = map.entry(task_run_id.to_string()).or_default();
    let stats = entry.value();
    if stats.calls >= MAX_CALLS_PER_TASK_RUN {
        return Err(BudgetRejection::CallCap);
    }
    if stats.tokens_in >= MAX_INPUT_TOKENS_PER_TASK_RUN {
        return Err(BudgetRejection::InputTokens);
    }
    if stats.tokens_out >= MAX_OUTPUT_TOKENS_PER_TASK_RUN {
        return Err(BudgetRejection::OutputTokens);
    }
    Ok(())
}

fn consume_budget(task_run_id: &str, tokens_in: u32, tokens_out: u32) {
    let map = task_run_stats_map();
    map.entry(task_run_id.to_string())
        .and_modify(|s| {
            s.calls = s.calls.saturating_add(1);
            s.tokens_in = s.tokens_in.saturating_add(tokens_in);
            s.tokens_out = s.tokens_out.saturating_add(tokens_out);
        })
        .or_insert(TaskRunEmitterStats {
            calls: 1,
            tokens_in,
            tokens_out,
        });
}

// ============================================================================
// Cache helpers
// ============================================================================

fn lookup_cached(key: &str) -> Option<CachedEntry> {
    let mut guard = tier1_cache().lock().ok()?;
    guard.get(key).cloned()
}

fn store_cached(key: &str, expression: &str, model_id: &str) {
    let Ok(mut guard) = tier1_cache().lock() else {
        return;
    };
    let created_at_epoch_s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    guard.put(
        key.to_string(),
        CachedEntry {
            expression: expression.to_string(),
            model_id: model_id.to_string(),
            created_at_epoch_s,
        },
    );
}

/// Build the Tier-1 cache key. The schema hint is canonicalised (sorted
/// keys, whitespace-free JSON) so two calls with the same logical shape
/// hit the same entry regardless of key order.
fn build_cache_key(
    goal: &str,
    schema_hint: &serde_json::Value,
    output_preview: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(goal.as_bytes());
    hasher.update(b"|");
    hasher.update(canonical_json(schema_hint).as_bytes());
    hasher.update(b"|");
    let preview_slice = if output_preview.len() > CACHE_KEY_PREVIEW_BYTES {
        // Slice at a char boundary to avoid panics on multi-byte preview
        // content. `floor_char_boundary` is nightly-only so do it by hand.
        let mut cut = CACHE_KEY_PREVIEW_BYTES;
        while cut > 0 && !output_preview.is_char_boundary(cut) {
            cut -= 1;
        }
        &output_preview[..cut]
    } else {
        output_preview
    };
    hasher.update(preview_slice.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Serialize a serde_json::Value with deterministic key ordering.
fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut s = String::from("{");
            for (i, (k, v)) in entries.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&serde_json::to_string(k).unwrap_or_default());
                s.push(':');
                s.push_str(&canonical_json(v));
            }
            s.push('}');
            s
        }
        serde_json::Value::Array(items) => {
            let mut s = String::from("[");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&canonical_json(item));
            }
            s.push(']');
            s
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

// ============================================================================
// Prompt + schema
// ============================================================================

fn resolve_model() -> String {
    // Per-runner override via settings. We reuse the generic
    // `config_facade::get_setting` plumbing with a dedicated struct so this
    // doesn't require mutating the `AiSettings` type.
    let cfg: ScriptedOutputSettings = crate::config_facade::get_setting();
    cfg.model.unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

fn settings_flag_on() -> bool {
    let cfg: ScriptedOutputSettings = crate::config_facade::get_setting();
    cfg.enabled
}

fn env_kill_switch_on() -> bool {
    matches!(std::env::var(ENV_KILL_SWITCH).ok().as_deref(), Some("1"))
}

/// Settings block for the scripted-output subsystem. Intentionally kept
/// small: a bool and an optional model override. Lives in the persistent
/// `settings.json` so ops can flip the kill switch without a restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptedOutputSettings {
    #[serde(default = "default_scripted_output_enabled")]
    pub enabled: bool,
    /// Optional per-runner model override (e.g. `"claude-sonnet-4-5"` for
    /// extraction-heavy deployments). `None` → [`DEFAULT_MODEL`].
    #[serde(default)]
    pub model: Option<String>,
}

fn default_scripted_output_enabled() -> bool {
    true
}

impl Default for ScriptedOutputSettings {
    fn default() -> Self {
        Self {
            enabled: default_scripted_output_enabled(),
            model: None,
        }
    }
}

/// JSON schema enforced on the LLM response. The server-side structured
/// output path (`run_prompt_with_structured_output`) uses this verbatim.
fn response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "expression": {
                "type": "string",
                "description": "A single-line JavaScript expression that, given variable `output` (a string containing the raw stdout), returns the requested value per the schema. Must be a single expression (no statements, no semicolons, no returns)."
            }
        },
        "required": ["expression"],
        "additionalProperties": false
    })
}

/// Compose the prompt. We send a short system preamble followed by a
/// user block carrying `{goal, schemaHint, outputPreview}`.
fn build_prompt(
    goal: &str,
    schema_hint: &serde_json::Value,
    output_preview: &str,
) -> String {
    let preamble = concat!(
        "You synthesize a single-line JavaScript expression that, given a ",
        "variable `output` (a string containing the raw stdout of a shell/step ",
        "command), returns the smallest value the caller asked for, ",
        "conforming to the provided schema hint.\n\n",
        "Rules:\n",
        "- Output JSON only, matching {\"expression\": \"<js>\"}.\n",
        "- The expression MUST be a single JavaScript expression — no statements, ",
        "no semicolons, no `return`, no `let`/`const`/`var`.\n",
        "- Reference the variable by its literal name `output` (the TS sandbox ",
        "binds the raw stdout to that exact identifier).\n",
        "- Prefer concise extractions: split lines, slice, regex match, map, ",
        "filter. Do not invent fields that aren't in the preview.\n",
        "- If you cannot extract usefully, return the trivial expression ",
        "`output.slice(0, 4000)` — never an empty string.\n",
    );
    let user = format!(
        "goal:\n{}\n\nschemaHint (keys + types):\n{}\n\noutputPreview (first ~2KB):\n{}",
        goal,
        serde_json::to_string_pretty(schema_hint).unwrap_or_else(|_| "{}".to_string()),
        output_preview
    );
    format!("{preamble}\n---\n{user}")
}

/// Parse `{"expression": "..."}` out of the model's output string.
/// The structured-output path returns the schema-compliant JSON directly
/// as the response content.
fn extract_expression(raw: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(raw.trim())
        .map_err(|e| format!("response is not JSON: {}", e))?;
    let expr = v
        .get("expression")
        .and_then(|e| e.as_str())
        .ok_or_else(|| "response missing `expression` string field".to_string())?;
    let expr = expr.trim();
    if expr.is_empty() {
        return Err("response `expression` was empty".to_string());
    }
    // Mild safety rails: reject obvious multi-statement expressions.
    // The vm sandbox on the TS side catches the rest.
    if expr.contains(';') && !expr.trim_end_matches(';').ends_with(')') {
        return Err("response `expression` contains ';' (not a single expression)".to_string());
    }
    Ok(expr.to_string())
}

fn truncate_for_log(s: &str) -> String {
    if s.len() <= 200 {
        s.to_string()
    } else {
        format!("{}…", &s[..200.min(s.len())])
    }
}

// ============================================================================
// Telemetry (activity_timeline events)
// ============================================================================

fn emit_cache_hit_event(task_run_id: &Option<String>) {
    spawn_activity_event(
        task_run_id.clone(),
        "scripted_output.cache_hit".to_string(),
        serde_json::json!({ "tier": 1 }),
    );
}

fn emit_llm_ok_event(
    task_run_id: &Option<String>,
    tokens_in: u32,
    tokens_out: u32,
    model: &str,
) {
    spawn_activity_event(
        task_run_id.clone(),
        "scripted_output.llm_ok".to_string(),
        serde_json::json!({
            "tokens_in": tokens_in,
            "tokens_out": tokens_out,
            "model": model,
        }),
    );
}

fn emit_fallback_event(task_run_id: &Option<String>, reason: &str) {
    spawn_activity_event(
        task_run_id.clone(),
        "scripted_output.fallback".to_string(),
        serde_json::json!({ "reason": reason }),
    );
}

/// Emit a scripted-output telemetry event that originates from the TS base
/// handler (`ScriptedOutputHandler.summarizeViaScript`). Exposed as the
/// `emit_scripted_output_event` Tauri command so the TS side can reuse the
/// FK-aware insert path rather than going through `insert_activity_entry`
/// (which runs PII scrubbing and has no FK pre-check).
///
/// Only the three TS-originated event names are accepted, plus the
/// `bad_expression` flavour of `fallback`; anything else is rejected so a
/// compromised/misbehaving caller can't spam arbitrary `scripted_output.*`
/// rows.
pub fn emit_ts_originated_event(
    name: &str,
    metadata: serde_json::Value,
    task_run_id: Option<String>,
) -> Result<(), String> {
    const ALLOWED: &[&str] = &[
        "scripted_output.attempted",
        "scripted_output.worker_ok",
        "scripted_output.bytes_avoided",
        "scripted_output.fallback",
    ];
    if !ALLOWED.contains(&name) {
        return Err(format!(
            "emit_ts_originated_event: event name '{}' is not one of {:?}",
            name, ALLOWED
        ));
    }
    spawn_activity_event(task_run_id, name.to_string(), metadata);
    Ok(())
}

/// Fire-and-forget telemetry write. Errors are logged at `debug` — a
/// telemetry failure must never disrupt the emitter path.
fn spawn_activity_event(
    task_run_id: Option<String>,
    event_name: String,
    metadata: serde_json::Value,
) {
    let Some(pg) = PgDb::try_global() else {
        debug!("scripted-output telemetry: PgDb not initialised yet; skipping '{}'", event_name);
        return;
    };
    // activity_timeline.task_run_id has a FK to task_runs(id). An "unassigned"
    // sentinel or a pre-task-run ad-hoc id would fail the FK check and the
    // insert would be silently dropped. Fold the original value into
    // metadata_json for debuggability; spawn a task that validates the FK by
    // pre-check, and falls back to NULL on miss.
    let mut metadata_with_tr = metadata;
    if let (serde_json::Value::Object(map), Some(s)) =
        (&mut metadata_with_tr, task_run_id.as_ref())
    {
        map.insert(
            "task_run_id_raw".to_string(),
            serde_json::Value::String(s.clone()),
        );
    }
    let meta_str = serde_json::to_string(&metadata_with_tr).unwrap_or_else(|_| "{}".to_string());

    tokio::spawn(async move {
        // If a task_run_id was passed, verify it exists before using it —
        // otherwise let the FK column be NULL so the event is still recorded.
        let resolved_tr = match task_run_id.as_deref() {
            None => None,
            Some(tr) => match pg.task_run_exists(tr).await {
                Ok(true) => Some(tr.to_string()),
                Ok(false) => None,
                Err(e) => {
                    debug!(
                        "scripted-output telemetry: task_run lookup failed for '{}': {}; recording with NULL task_run_id",
                        tr, e
                    );
                    None
                }
            },
        };
        let input = ActivityTimelineInput {
            // text_content doubles as the event name so existing FTS queries
            // can find it ("scripted_output.llm_ok", etc.).
            text_content: event_name.clone(),
            source_type: "scripted_output".to_string(),
            capture_mode: "runner".to_string(),
            app_name: Some("runner".to_string()),
            window_title: None,
            url: None,
            task_run_id: resolved_tr,
            screenshot_path: None,
            element_count: None,
            confidence: None,
            metadata_json: Some(meta_str),
        };
        if let Err(e) = pg.insert_activity_entry(&input).await {
            debug!(
                "scripted-output telemetry insert failed for '{}': {}",
                event_name, e
            );
        }
    });
}

// ============================================================================
// TODO: /task_runs/:id wiring
// ============================================================================
//
// Phase A does NOT surface `TaskRunEmitterStats` on the `/task_runs/:id`
// endpoint. That wiring belongs in whichever handler renders the
// task-run detail page. Candidates to audit:
//   - `qontinui-runner/src-tauri/src/graphql/` (if we expose task_runs via
//     GraphQL) — add an `emitterStats` field that calls
//     `crate::step_output::script_emitter::get_task_run_stats(...)`.
//   - `qontinui-runner/src-tauri/src/commands/` — any REST-ish handler
//     over task_runs can merge the stats into its response JSON.
//
// The accessor signature is stable: see `get_task_run_stats` above.

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_task_run_id(tag: &str) -> String {
        format!(
            "test-{}-{}",
            tag,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        )
    }

    #[test]
    fn cache_key_is_stable_across_key_order() {
        let hint_a = serde_json::json!({ "a": "string", "b": "number" });
        let hint_b = serde_json::json!({ "b": "number", "a": "string" });
        let k1 = build_cache_key("goal", &hint_a, "preview");
        let k2 = build_cache_key("goal", &hint_b, "preview");
        assert_eq!(k1, k2, "cache key should be stable under schema key reorder");
    }

    #[test]
    fn cache_key_differs_when_preview_differs_within_2kb() {
        let hint = serde_json::json!({ "a": "string" });
        let k1 = build_cache_key("g", &hint, "hello");
        let k2 = build_cache_key("g", &hint, "world");
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_ignores_preview_past_2kb() {
        let hint = serde_json::json!({});
        let base = "x".repeat(CACHE_KEY_PREVIEW_BYTES);
        let k1 = build_cache_key("g", &hint, &base);
        let k2 = build_cache_key("g", &hint, &(base.clone() + "TAIL"));
        assert_eq!(k1, k2, "bytes past the cache-key cutoff must not change the key");
    }

    #[test]
    fn extract_expression_valid() {
        let raw = r#"{"expression": "output.slice(0, 4000)"}"#;
        assert_eq!(extract_expression(raw).unwrap(), "output.slice(0, 4000)");
    }

    #[test]
    fn extract_expression_missing_field() {
        let raw = r#"{"other": "x"}"#;
        assert!(extract_expression(raw).is_err());
    }

    #[test]
    fn extract_expression_empty_string() {
        let raw = r#"{"expression": "   "}"#;
        assert!(extract_expression(raw).is_err());
    }

    #[test]
    fn extract_expression_rejects_multi_statement() {
        let raw = r#"{"expression": "const x = 1; x + 1"}"#;
        assert!(extract_expression(raw).is_err());
    }

    // The tests below deliberately avoid calling `reset_task_run_stats_for_tests`
    // / `reset_cache_for_tests` — those wipe the *global* LRU / DashMap and race
    // with any other test running in parallel that touches the same state. Each
    // test instead uses a unique-per-call task_run_id / cache key so it only
    // reads back what it itself wrote.

    #[test]
    fn budget_rejects_after_call_cap() {
        let tr = unique_task_run_id("callcap");
        for _ in 0..MAX_CALLS_PER_TASK_RUN {
            assert!(check_budget_available(&tr).is_ok());
            consume_budget(&tr, 1, 1);
        }
        match check_budget_available(&tr) {
            Err(BudgetRejection::CallCap) => {}
            _ => panic!("expected CallCap rejection"),
        }
        let stats = get_task_run_stats(&tr).expect("stats should exist");
        assert_eq!(stats.calls, MAX_CALLS_PER_TASK_RUN);
    }

    #[test]
    fn budget_rejects_on_input_tokens() {
        let tr = unique_task_run_id("intokens");
        consume_budget(&tr, MAX_INPUT_TOKENS_PER_TASK_RUN, 0);
        match check_budget_available(&tr) {
            Err(BudgetRejection::InputTokens) => {}
            _ => panic!("expected InputTokens rejection"),
        }
    }

    #[test]
    fn budget_rejects_on_output_tokens() {
        let tr = unique_task_run_id("outtokens");
        consume_budget(&tr, 0, MAX_OUTPUT_TOKENS_PER_TASK_RUN);
        match check_budget_available(&tr) {
            Err(BudgetRejection::OutputTokens) => {}
            _ => panic!("expected OutputTokens rejection"),
        }
    }

    #[test]
    fn cache_put_then_lookup() {
        // Use a unique key so we don't race with any other test poking the
        // cache — skip the global reset for the same reason noted above.
        let key = build_cache_key(
            &format!("g-{}", unique_task_run_id("cache")),
            &serde_json::json!({}),
            "p",
        );
        assert!(lookup_cached(&key).is_none());
        store_cached(&key, "output.slice(0, 10)", "claude-haiku-x");
        let hit = lookup_cached(&key).expect("hit");
        assert_eq!(hit.expression, "output.slice(0, 10)");
        assert_eq!(hit.model_id, "claude-haiku-x");
    }

    #[test]
    fn canonical_json_sorts_keys_recursively() {
        let v = serde_json::json!({
            "z": 1,
            "a": { "y": 2, "b": 3 }
        });
        let s = canonical_json(&v);
        assert_eq!(s, r#"{"a":{"b":3,"y":2},"z":1}"#);
    }

    #[test]
    fn env_kill_switch_reports_off_by_default() {
        // The cargo-test environment doesn't set QONTINUI_SCRIPTED_OUTPUT,
        // and `env_kill_switch_on` should therefore report `false`.
        // (We deliberately don't *mutate* the env from the test —
        // that's an `unsafe fn` in recent Rust, and brittle under
        // cargo's parallel test runner.)
        if std::env::var(ENV_KILL_SWITCH).ok().as_deref() == Some("1") {
            // External CI harness set it on — nothing to assert here.
            return;
        }
        assert!(!env_kill_switch_on());
    }
}
