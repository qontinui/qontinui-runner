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
const LLM_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// Which provider served this call: `"claude_api"`, `"claude_api_warm"`,
    /// or `"cache"`. Lets the TS panel format `model@provider`.
    pub provider: String,
    /// Tokens written to Anthropic prompt cache (1.25× base input price). Only
    /// non-zero when the warm provider's system prefix crossed the minimum
    /// cacheable size on this call.
    pub cache_creation_tokens: u32,
    /// Tokens read from Anthropic prompt cache (0.1× base input price).
    /// Only non-zero after the cache is populated and the ≥2s propagation
    /// window has elapsed.
    pub cache_read_tokens: u32,
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
            provider: "cache".to_string(),
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
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
    let provider_mode = resolve_provider_mode();
    // Split the prompt so the stable `system_prefix` (preamble + worked
    // examples + version suffix) can be marked for caching, while the
    // per-call `{goal, schemaHint, outputPreview}` rides in the user message
    // without polluting the cache key.
    let (system_prefix, user_message) = build_prompt_split(&goal, &schema_hint, &output_preview);

    // ---------------------------------------------- 6. Issue LLM call w/ timeout
    // All provider paths are blocking; wrap in spawn_blocking as the rest of
    // the codebase does. We dispatch on `provider_mode` inside the closure so
    // the hot-path is symmetric across modes.
    let model_owned = model.clone();
    let ai_settings_snapshot = crate::settings::get_ai_settings();
    let schema_for_call = schema.clone();
    let system_prefix_for_call = system_prefix.clone();
    let user_message_for_call = user_message.clone();

    // Snapshot Gemma-local settings up front so the spawn_blocking closure
    // doesn't need to reach back through the Tokio context. Cheap owned
    // clones; the closure gets its own copies and the outer function still
    // has access for the post-failure diagnostics.
    let gemma_settings_snapshot: ScriptedOutputSettings = crate::config_facade::get_setting();
    let gemma_settings_for_call = gemma_settings_snapshot.clone();

    let llm_call = async move {
        tokio::task::spawn_blocking(move || {
            call_provider(
                provider_mode,
                &system_prefix_for_call,
                &user_message_for_call,
                &ai_settings_snapshot.claude_api,
                &gemma_settings_for_call,
                &model_owned,
                &schema_for_call,
            )
        })
        .await
    };

    let (ai_response, provider_label) = match timeout(LLM_TIMEOUT, llm_call).await {
        Ok(Ok((resp, label))) => (resp, label),
        Ok(Err(join_err)) => {
            warn!(
                "scripted-output emitter: spawn_blocking join error: {}",
                join_err
            );
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

        // Unreachable credentials/server outcome after the full cascade.
        // On Auto this already means every configured lane was tried and
        // no usable credential or local endpoint was found, so degrade
        // cleanly to Disabled. On a *forced* lane (ClaudeApiWarm /
        // GemmaLocalWarm), surface a loud LlmError — the user asked
        // specifically for that path.
        let warm_missing =
            crate::ai_provider::claude_api_warm::is_no_credential_error(&ai_response);
        let gemma_missing = crate::ai_provider::gemma_local_warm::is_no_server_error(&ai_response);
        if warm_missing || gemma_missing {
            match provider_mode {
                ScriptedOutputProviderMode::Auto => {
                    debug!(
                        "scripted-output emitter: all Auto lanes unavailable ({}); returning Disabled",
                        if warm_missing {
                            "no Claude credential, Gemma local server unreachable"
                        } else {
                            "Gemma local server unreachable"
                        }
                    );
                    emit_fallback_event(&task_run_id, "disabled");
                    return Err(EmitError::Disabled);
                }
                ScriptedOutputProviderMode::ClaudeApiWarm => {
                    warn!(
                        "scripted-output emitter: ClaudeApiWarm forced but no warm-provider credentials configured"
                    );
                    emit_fallback_event(&task_run_id, "emitter_error");
                    return Err(EmitError::LlmError(
                        "no warm-provider credentials configured".to_string(),
                    ));
                }
                ScriptedOutputProviderMode::GemmaLocalWarm => {
                    warn!(
                        "scripted-output emitter: GemmaLocalWarm forced but local server {} unreachable",
                        gemma_settings_snapshot.gemma_local_endpoint
                    );
                    emit_fallback_event(&task_run_id, "emitter_error");
                    return Err(EmitError::LlmError(format!(
                        "gemma-local server at {} unreachable",
                        gemma_settings_snapshot.gemma_local_endpoint
                    )));
                }
                ScriptedOutputProviderMode::ClaudeApi => {
                    // Legacy path wouldn't emit the warm marker — unreachable.
                }
            }
        }

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
    let cache_creation_tokens = ai_response.cache_creation_tokens.unwrap_or(0) as u32;
    let cache_read_tokens = ai_response.cache_read_tokens.unwrap_or(0) as u32;

    // ----------------------------------------------- 8. Record budget spend
    if let Some(ref tr) = task_run_id {
        consume_budget(tr, tokens_in, tokens_out);
    }

    // ----------------------------------------- 9. Cache the successful result
    store_cached(&cache_key, &expression, &model);

    // ------------------------------------------------ 10. Emit success telemetry
    emit_llm_ok_event(
        &task_run_id,
        tokens_in,
        tokens_out,
        &model,
        provider_label,
        cache_creation_tokens,
        cache_read_tokens,
    );

    Ok(EmitResult {
        expression,
        model_id: model,
        tokens_in,
        tokens_out,
        source: "llm".to_string(),
        cache_tier: None,
        provider: provider_label.to_string(),
        cache_creation_tokens,
        cache_read_tokens,
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

/// Dispatch a single emit call to the provider selected by `mode`, returning
/// both the provider response and the telemetry label that should end up in
/// `EmitResult.provider` and in the `scripted_output.llm_ok` event.
///
/// `Auto` implements a two-step cascade:
///   1. Claude warm (API key → OAuth fallback internal to `claude_api_warm`).
///   2. On credentials-missing, fall through to Gemma local if the configured
///      endpoint is reachable.
/// If both lanes surface their respective "missing" markers the caller sees
/// a final response carrying the Gemma server-unreachable marker; the main
/// emit path maps that to `EmitError::Disabled` for `Auto`.
///
/// `ClaudeApiWarm` / `ClaudeApi` / `GemmaLocalWarm` are forced lanes — no
/// fallback. The caller surfaces the underlying error as `LlmError` when a
/// forced lane fails.
fn call_provider(
    mode: ScriptedOutputProviderMode,
    system_prefix: &str,
    user_message: &str,
    claude_api_settings: &crate::settings::ClaudeApiSettings,
    gemma_settings: &ScriptedOutputSettings,
    model: &str,
    schema: &serde_json::Value,
) -> (crate::ai_provider::AiResponse, &'static str) {
    const EXTRACTION_SCHEMA_NAME: &str = "scripted_output_extraction";
    const MAX_EMITTER_TOKENS: u32 = 512;

    let claude_warm = || {
        crate::ai_provider::claude_api_warm::run_claude_api_warm_with_structured_output(
            system_prefix,
            user_message,
            claude_api_settings,
            Some(model),
            None,      // no Doctor handle — short synthetic call
            Some(0.0), // deterministic
            Some(MAX_EMITTER_TOKENS),
            schema,
            EXTRACTION_SCHEMA_NAME,
        )
    };

    let gemma_local = || {
        crate::ai_provider::gemma_local_warm::run_gemma_local_warm_with_structured_output(
            system_prefix,
            user_message,
            &gemma_settings.gemma_local_endpoint,
            &gemma_settings.gemma_local_model_alias,
            None,
            Some(0.0),
            Some(MAX_EMITTER_TOKENS),
            schema,
            EXTRACTION_SCHEMA_NAME,
        )
    };

    match mode {
        ScriptedOutputProviderMode::Auto => {
            let warm = claude_warm();
            if crate::ai_provider::claude_api_warm::is_no_credential_error(&warm) {
                info!(
                    "scripted-output emitter: Auto — Claude warm credentials missing, trying Gemma local"
                );
                let local = gemma_local();
                (local, "gemma_local_warm")
            } else {
                (warm, "claude_api_warm")
            }
        }
        ScriptedOutputProviderMode::ClaudeApiWarm => (claude_warm(), "claude_api_warm"),
        ScriptedOutputProviderMode::ClaudeApi => {
            let concatenated = format!("{}\n\n---\n\n{}", system_prefix, user_message);
            let resp = run_prompt_with_structured_output(
                &concatenated,
                schema,
                EXTRACTION_SCHEMA_NAME,
                None,
                Some(model),
                Some("claude_api"),
                Some(0.0),
                Some(MAX_EMITTER_TOKENS),
            );
            (resp, "claude_api")
        }
        ScriptedOutputProviderMode::GemmaLocalWarm => (gemma_local(), "gemma_local_warm"),
    }
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
fn build_cache_key(goal: &str, schema_hint: &serde_json::Value, output_preview: &str) -> String {
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

fn resolve_provider_mode() -> ScriptedOutputProviderMode {
    let cfg: ScriptedOutputSettings = crate::config_facade::get_setting();
    cfg.provider
}

fn settings_flag_on() -> bool {
    let cfg: ScriptedOutputSettings = crate::config_facade::get_setting();
    cfg.enabled
}

fn env_kill_switch_on() -> bool {
    matches!(std::env::var(ENV_KILL_SWITCH).ok().as_deref(), Some("1"))
}

/// Which provider serves scripted-output emit calls.
///
/// The user's chat-flow provider is deliberately disjoint from this — see
/// `plans/warm-emitter-provider-2026-04-21.md` decision 3. A `claude_cli` /
/// `gemini_cli` value is intentionally not offered: subprocess-per-call
/// providers can't satisfy the emitter's latency contract, and re-enabling
/// them here would reintroduce the 100% timeout failure mode on dev runners.
///
/// `GemmaLocalWarm` is *not* a CLI-shaped provider — it's a long-lived HTTP
/// server (llama.cpp serving Gemma 4 26B-A4B). The 2026-04-22 spike measured
/// 1.1–1.4 s wall-time against the emitter's canonical test shape, ahead of
/// the Claude Haiku 4.5 warm baseline, with zero dollars and zero quota.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScriptedOutputProviderMode {
    /// Cascade: Claude warm (API key first, OAuth second) → Gemma local
    /// (if an endpoint is reachable) → `Disabled`. Default.
    #[default]
    Auto,
    /// Force the warm provider. If no credentials are available the
    /// emitter surfaces a loud `LlmError` rather than falling back silently.
    ClaudeApiWarm,
    /// Force the legacy keychain-API-key path (no prompt caching, no
    /// OAuth fallback). Useful for production runners with a dedicated
    /// Anthropic API key where billing boundaries matter.
    ClaudeApi,
    /// Force the local llama.cpp / Gemma path. If the endpoint is
    /// unreachable the emitter surfaces a loud `LlmError` rather than
    /// falling back silently. Useful for dev machines that want to
    /// deliberately avoid any Anthropic-side traffic.
    GemmaLocalWarm,
}

/// Settings block for the scripted-output subsystem. Intentionally kept
/// small: a bool, an optional model override, a provider mode, and the
/// local-Gemma endpoint knobs. Lives in the persistent `settings.json` so
/// ops can flip the kill switch without a restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptedOutputSettings {
    #[serde(default = "default_scripted_output_enabled")]
    pub enabled: bool,
    /// Optional per-runner model override (e.g. `"claude-sonnet-4-5"` for
    /// extraction-heavy deployments). `None` → [`DEFAULT_MODEL`]. Only
    /// consulted by the Claude lanes; the Gemma local lane always uses
    /// the model the server was started with.
    #[serde(default)]
    pub model: Option<String>,
    /// Which provider serves emit calls. See [`ScriptedOutputProviderMode`].
    #[serde(default)]
    pub provider: ScriptedOutputProviderMode,
    /// HTTP base URL of the local llama.cpp / Gemma server. Only consulted
    /// by the `GemmaLocalWarm` lane (and by `Auto` when it cascades into
    /// that lane). Default targets the sibling docker-compose in
    /// `qontinui/docker/gemma-server/`.
    #[serde(default = "default_gemma_local_endpoint")]
    pub gemma_local_endpoint: String,
    /// Alias the llama.cpp server registers for the loaded GGUF. Used in
    /// log lines only; llama.cpp's `/completion` endpoint doesn't route on
    /// model id.
    #[serde(default = "default_gemma_local_model_alias")]
    pub gemma_local_model_alias: String,
}

fn default_scripted_output_enabled() -> bool {
    true
}

fn default_gemma_local_endpoint() -> String {
    crate::ai_provider::gemma_local_warm::DEFAULT_LOCAL_ENDPOINT.to_string()
}

fn default_gemma_local_model_alias() -> String {
    crate::ai_provider::gemma_local_warm::DEFAULT_MODEL_ALIAS.to_string()
}

impl Default for ScriptedOutputSettings {
    fn default() -> Self {
        Self {
            enabled: default_scripted_output_enabled(),
            model: None,
            provider: ScriptedOutputProviderMode::Auto,
            gemma_local_endpoint: default_gemma_local_endpoint(),
            gemma_local_model_alias: default_gemma_local_model_alias(),
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

/// Compose the prompt as a `(system_prefix, user_message)` pair.
///
/// The `system_prefix` is stable across emit calls and is deliberately
/// fattened with worked examples so it crosses Anthropic's Haiku caching
/// minimum. Haiku 3.x accepted ~1024 tokens (~4000 chars); Haiku 4.x
/// raised the floor to ~2048 tokens (~8000 chars). Below that threshold
/// `cache_control: ephemeral` is silently ignored and the warm provider
/// pays the uncached rate on every call — confirmed empirically against
/// `claude-haiku-4-5-20251001` on 2026-04-22 (cache_marker=true but
/// `cache_read_input_tokens=0` on repeat calls).
///
/// The `user_message` carries the per-call dynamic payload — goal,
/// schema hint, output preview — which changes every call and must not
/// be part of the cached prefix.
///
/// **Versioning.** The trailing `Emitter system v2` line makes intentional
/// preamble bumps invalidate the cache cleanly (distinct bytes → distinct
/// cache key) instead of waiting for the 5-minute TTL.
fn build_prompt_split(
    goal: &str,
    schema_hint: &serde_json::Value,
    output_preview: &str,
) -> (String, String) {
    // The stable prefix. Fattened with 13 worked examples to cross the
    // ~2048-token (~8000 char) Haiku 4.x caching minimum with margin.
    // Counted once at write time and asserted in tests — don't shrink
    // without rechecking the bound against current Haiku cache telemetry.
    let system_prefix = concat!(
        "You synthesize a single-line JavaScript expression that, given a ",
        "variable `output` (a string containing the raw stdout of a shell/step ",
        "command), returns the smallest value the caller asked for, ",
        "conforming to the provided schema hint.\n\n",
        "## Rules\n",
        "- Output JSON only, matching {\"expression\": \"<js>\"}.\n",
        "- The expression MUST be a single JavaScript expression — no statements, ",
        "no semicolons, no `return`, no `let`/`const`/`var`.\n",
        "- Reference the variable by its literal name `output` (the TS sandbox ",
        "binds the raw stdout to that exact identifier).\n",
        "- Prefer concise extractions: split lines, slice, regex match, map, ",
        "filter. Do not invent fields that aren't in the preview.\n",
        "- If you cannot extract usefully, return the trivial expression ",
        "`output.slice(0, 4000)` — never an empty string.\n",
        "- Treat `output` as a string. You can call any String.prototype method ",
        "and any Array.prototype method on split results. No external modules, ",
        "no network, no filesystem — the sandbox only exposes `output`.\n",
        "- Prefer defensive extractions: guard against missing matches with ",
        "optional chaining (`?.`) and `||` fallbacks so the expression never ",
        "throws. A throw aborts the step; a harmless empty result does not.\n",
        "- Keep output sizes bounded. If the caller asks for a collection, cap ",
        "it at 50 entries with `.slice(0, 50)` so a runaway grep doesn't blow ",
        "the summariser's budget.\n\n",
        "## Worked examples\n\n",
        "### Example 1 — git log commit hashes\n",
        "goal: extract the first 10 commit hashes\n",
        "schemaHint: {\"keys\": {\"hashes\": \"string[]\"}}\n",
        "outputPreview: \"commit 8a3fe12cd9ab4...\\nAuthor: Jane Doe <jane@x.com>\\nDate: Tue Apr 21 09:12:33 2026\\n\\n    add script emitter\\n\\ncommit 2b17d9e5cf01a...\\nAuthor: J Q <jq@x.com>\\nDate: Mon Apr 20 16:04:11 2026\\n\\n    cleanup\\n\"\n",
        "expression: ({hashes: (output.match(/^commit\\s+([0-9a-f]{7,40})/gm) || []).slice(0, 10).map(l => l.split(/\\s+/)[1])})\n\n",
        "### Example 2 — filenames from ls -la\n",
        "goal: list regular filenames (no directories or dotfiles)\n",
        "schemaHint: {\"keys\": {\"files\": \"string[]\"}}\n",
        "outputPreview: \"total 24\\ndrwxr-xr-x 2 jq jq 4096 Apr 21 09:00 .\\ndrwxr-xr-x 3 jq jq 4096 Apr 21 08:59 ..\\n-rw-r--r-- 1 jq jq  120 Apr 21 09:00 README.md\\n-rw-r--r-- 1 jq jq 2048 Apr 21 09:00 script.js\\ndrwxr-xr-x 2 jq jq 4096 Apr 21 09:00 build\\n\"\n",
        "expression: ({files: output.split(/\\r?\\n/).filter(l => l.startsWith('-')).map(l => l.trim().split(/\\s+/).slice(8).join(' ')).filter(n => n && !n.startsWith('.')).slice(0, 50)})\n\n",
        "### Example 3 — numeric line count from wc -l\n",
        "goal: extract the line count as a number\n",
        "schemaHint: {\"keys\": {\"count\": \"number\"}}\n",
        "outputPreview: \"   1427 /tmp/input.log\\n\"\n",
        "expression: ({count: Number((output.trim().match(/^\\d+/) || [0])[0])})\n\n",
        "### Example 4 — JSON field via jq\n",
        "goal: extract the `id` and `status` fields of the returned JSON object\n",
        "schemaHint: {\"keys\": {\"id\": \"string\", \"status\": \"string\"}}\n",
        "outputPreview: \"{\\\"id\\\":\\\"task-8412\\\",\\\"status\\\":\\\"running\\\",\\\"started_at\\\":1713702933,\\\"worker\\\":\\\"w-3\\\"}\\n\"\n",
        "expression: (((j) => ({id: j?.id || '', status: j?.status || ''}))(((s) => { try { return JSON.parse(s); } catch { return {}; } })(output.trim())))\n\n",
        "### Example 5 — last matching line from grep\n",
        "goal: return the last line mentioning an ERROR, or an empty string\n",
        "schemaHint: {\"keys\": {\"last_error\": \"string\"}}\n",
        "outputPreview: \"2026-04-21T09:02:10 INFO  startup complete\\n2026-04-21T09:02:18 ERROR pg connection failed\\n2026-04-21T09:02:19 INFO  retrying\\n2026-04-21T09:02:22 ERROR pg connection failed again\\n\"\n",
        "expression: ({last_error: (output.split(/\\r?\\n/).filter(l => /ERROR/.test(l)).pop() || '')})\n\n",
        "### Example 6 — PID from ps row\n",
        "goal: extract the PID of the node process\n",
        "schemaHint: {\"keys\": {\"pid\": \"number\"}}\n",
        "outputPreview: \"  PID TTY          TIME CMD\\n 1234 pts/0    00:00:00 bash\\n 1517 pts/0    00:00:03 node\\n 1520 pts/0    00:00:00 ps\\n\"\n",
        "expression: ({pid: Number(((output.split(/\\r?\\n/).find(l => /\\bnode\\b/.test(l)) || '').trim().split(/\\s+/)[0]) || 0)})\n\n",
        "### Example 7 — disk usage summary from df -h\n",
        "goal: extract the mount point and use% of each filesystem, skipping tmpfs and devfs\n",
        "schemaHint: {\"keys\": {\"mounts\": \"Array<{mount: string, usePercent: number}>\"}}\n",
        "outputPreview: \"Filesystem      Size  Used Avail Use% Mounted on\\n/dev/sda1       100G   42G   58G  43% /\\ntmpfs           3.9G  8.0K  3.9G   1% /run\\n/dev/sdb1       500G  312G  188G  63% /data\\ndevfs           200K  200K     0 100% /dev\\n\"\n",
        "expression: ({mounts: output.split(/\\r?\\n/).slice(1).map(l => l.trim().split(/\\s+/)).filter(p => p.length >= 6 && !/^(tmpfs|devfs)$/.test(p[0])).map(p => ({mount: p[5], usePercent: Number((p[4] || '').replace('%','')) || 0})).slice(0, 50)})\n\n",
        "### Example 8 — HTTP status codes from curl -v\n",
        "goal: extract the final HTTP status code and the requested URL\n",
        "schemaHint: {\"keys\": {\"status\": \"number\", \"url\": \"string\"}}\n",
        "outputPreview: \"*   Trying 93.184.216.34:443...\\n* Connected to example.com (93.184.216.34) port 443\\n> GET /about HTTP/1.1\\n> Host: example.com\\n> User-Agent: curl/8.4.0\\n>\\n< HTTP/1.1 200 OK\\n< Content-Type: text/html; charset=UTF-8\\n< Content-Length: 1256\\n<\\n\"\n",
        "expression: ({status: Number(((output.match(/^< HTTP\\/[0-9.]+\\s+(\\d{3})/m) || [])[1]) || 0), url: (((output.match(/^> GET\\s+(\\S+)\\s/m) || [])[1]) || '')})\n\n",
        "### Example 9 — running containers from docker ps --format json\n",
        "goal: list running container names and images\n",
        "schemaHint: {\"keys\": {\"containers\": \"Array<{name: string, image: string}>\"}}\n",
        "outputPreview: \"{\\\"Names\\\":\\\"api-1\\\",\\\"Image\\\":\\\"registry.local/api:v2.3.1\\\",\\\"Status\\\":\\\"Up 2 hours\\\"}\\n{\\\"Names\\\":\\\"db-1\\\",\\\"Image\\\":\\\"postgres:16.2\\\",\\\"Status\\\":\\\"Up 2 hours (healthy)\\\"}\\n{\\\"Names\\\":\\\"cache\\\",\\\"Image\\\":\\\"redis:7-alpine\\\",\\\"Status\\\":\\\"Up 5 minutes\\\"}\\n\"\n",
        "expression: ({containers: output.split(/\\r?\\n/).filter(l => l.trim().startsWith('{')).map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean).map(c => ({name: c?.Names || '', image: c?.Image || ''})).slice(0, 50)})\n\n",
        "### Example 10 — env var map from `env | sort`\n",
        "goal: build a map of uppercase-named environment variables (skip lowercase and comments)\n",
        "schemaHint: {\"keys\": {\"env\": \"Record<string, string>\"}}\n",
        "outputPreview: \"# local overrides\\nHOME=/home/jq\\nPATH=/usr/local/bin:/usr/bin:/bin\\nLANG=en_US.UTF-8\\nSHELL=/bin/bash\\nrecent_files=/tmp/a,/tmp/b\\nTERM=xterm-256color\\n\"\n",
        "expression: ({env: output.split(/\\r?\\n/).filter(l => /^[A-Z_][A-Z0-9_]*=/.test(l)).reduce((acc, l) => { const i = l.indexOf('='); if (i > 0) acc[l.slice(0, i)] = l.slice(i + 1); return acc; }, {})})\n\n",
        "### Example 11 — semver from `tool --version`\n",
        "goal: extract the version string (major.minor.patch) from a --version banner\n",
        "schemaHint: {\"keys\": {\"version\": \"string\"}}\n",
        "outputPreview: \"my-tool version 4.12.0 (build 2026-03-14, rev abc1234)\\nCopyright (c) 2026 Example Corp.\\n\"\n",
        "expression: ({version: ((output.match(/\\b(\\d+\\.\\d+\\.\\d+(?:-[0-9A-Za-z.-]+)?)\\b/) || [])[1]) || ''})\n\n",
        "### Example 12 — pytest summary counts\n",
        "goal: extract pass/fail/skip counts from a pytest summary line\n",
        "schemaHint: {\"keys\": {\"passed\": \"number\", \"failed\": \"number\", \"skipped\": \"number\"}}\n",
        "outputPreview: \"collected 47 items\\n\\ntests/test_api.py ..........F.....s....          [ 45%]\\ntests/test_db.py ............s.s..............          [100%]\\n\\n======== 42 passed, 1 failed, 4 skipped in 12.34s ========\\n\"\n",
        "expression: ({passed: Number(((output.match(/(\\d+)\\s+passed/) || [])[1]) || 0), failed: Number(((output.match(/(\\d+)\\s+failed/) || [])[1]) || 0), skipped: Number(((output.match(/(\\d+)\\s+skipped/) || [])[1]) || 0)})\n\n",
        "### Example 13 — unique IPs from access log\n",
        "goal: count distinct client IPs that produced any 5xx response\n",
        "schemaHint: {\"keys\": {\"error_ips\": \"string[]\", \"error_ip_count\": \"number\"}}\n",
        "outputPreview: \"10.0.0.12 - - [21/Apr/2026:09:02:10] \\\"GET /api/a HTTP/1.1\\\" 200 1244\\n10.0.0.55 - - [21/Apr/2026:09:02:11] \\\"GET /api/b HTTP/1.1\\\" 502 41\\n10.0.0.12 - - [21/Apr/2026:09:02:13] \\\"GET /api/c HTTP/1.1\\\" 503 44\\n10.0.0.91 - - [21/Apr/2026:09:02:14] \\\"GET /api/d HTTP/1.1\\\" 200 1200\\n10.0.0.55 - - [21/Apr/2026:09:02:15] \\\"GET /api/b HTTP/1.1\\\" 500 47\\n\"\n",
        "expression: (((ips) => ({error_ips: ips, error_ip_count: ips.length}))(Array.from(new Set(output.split(/\\r?\\n/).filter(l => /\"\\s+5\\d{2}\\s/.test(l)).map(l => (l.match(/^(\\d+\\.\\d+\\.\\d+\\.\\d+)/) || [])[1]).filter(Boolean))).slice(0, 50)))\n\n",
        "## Response format\n\n",
        "Respond ONLY with a single JSON object matching {\"expression\": \"<js>\"} via the supplied tool. Do not wrap in prose. Do not emit multiple tool calls. Keep the expression on a single line.\n\n",
        "## Anti-patterns to avoid\n\n",
        "- Do not use `JSON.parse` outside a try/catch-equivalent IIFE — malformed JSON must not throw.\n",
        "- Do not use `eval`, `Function`, or dynamic code construction — the sandbox disallows them.\n",
        "- Do not access globals like `process`, `require`, `fetch`, `window`, `document` — they are not in scope.\n",
        "- Do not assume the preview is the full `output` at runtime — `output` may be longer.\n",
        "- Do not rely on regex flags other than `g`, `i`, `m`, `s`, `u` — other flags are not guaranteed.\n\n",
        "Emitter system v2"
    );

    let user_message = format!(
        "goal:\n{}\n\nschemaHint (keys + types):\n{}\n\noutputPreview (first ~2KB):\n{}",
        goal,
        serde_json::to_string_pretty(schema_hint).unwrap_or_else(|_| "{}".to_string()),
        output_preview
    );

    (system_prefix.to_string(), user_message)
}

/// Parse `{"expression": "..."}` out of the model's output string.
/// The structured-output path returns the schema-compliant JSON directly
/// as the response content.
fn extract_expression(raw: &str) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(raw.trim()).map_err(|e| format!("response is not JSON: {}", e))?;
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
    provider: &str,
    cache_creation_tokens: u32,
    cache_read_tokens: u32,
) {
    spawn_activity_event(
        task_run_id.clone(),
        "scripted_output.llm_ok".to_string(),
        serde_json::json!({
            "tokens_in": tokens_in,
            "tokens_out": tokens_out,
            "model": model,
            "provider": provider,
            "cache_creation_tokens": cache_creation_tokens,
            "cache_read_tokens": cache_read_tokens,
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
        debug!(
            "scripted-output telemetry: PgDb not initialised yet; skipping '{}'",
            event_name
        );
        return;
    };
    // activity_timeline.task_run_id has a FK to task_runs(id). An "unassigned"
    // sentinel or a pre-task-run ad-hoc id would fail the FK check and the
    // insert would be silently dropped. Fold the original value into
    // metadata_json for debuggability; spawn a task that validates the FK by
    // pre-check, and falls back to NULL on miss.
    let mut metadata_with_tr = metadata;
    if let (serde_json::Value::Object(map), Some(s)) = (&mut metadata_with_tr, task_run_id.as_ref())
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
        assert_eq!(
            k1, k2,
            "cache key should be stable under schema key reorder"
        );
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
        assert_eq!(
            k1, k2,
            "bytes past the cache-key cutoff must not change the key"
        );
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
    fn build_prompt_split_meets_cache_threshold() {
        let hint = serde_json::json!({"keys": {"a": "string"}});
        let (system_prefix, _user) = build_prompt_split("some goal", &hint, "some preview");
        // Plan decision 4: the system_prefix must cross Anthropic's
        // Haiku caching minimum or `cache_control: ephemeral` is a no-op.
        // Haiku 3.x was ~1024 tokens (~4000 chars); Haiku 4.x raised the
        // floor to ~2048 tokens (~8000 chars) — confirmed against
        // `claude-haiku-4-5-20251001` on 2026-04-22 where a 5943-char
        // prefix produced `cache_read_input_tokens=0` on repeat calls.
        // If a future Haiku revision raises the minimum again and real
        // telemetry shows a zero cache_read rate, bump examples further
        // and update this bound.
        assert!(
            system_prefix.len() >= 8000,
            "system_prefix length {} is under the 8000-char Haiku 4.x cache threshold",
            system_prefix.len()
        );
    }

    #[test]
    fn build_prompt_split_contains_version_suffix() {
        let (system_prefix, _user) = build_prompt_split("g", &serde_json::json!({}), "p");
        // The trailing `Emitter system v2` marker lets intentional
        // preamble bumps invalidate the cache cleanly instead of
        // waiting for the 5-minute TTL.
        assert!(
            system_prefix.trim_end().ends_with("Emitter system v2"),
            "system_prefix should end with the version suffix; got tail: {:?}",
            &system_prefix[system_prefix.len().saturating_sub(64)..]
        );
        assert!(system_prefix.contains("Emitter system v2"));
    }

    #[test]
    fn build_prompt_split_user_message_has_goal_schema_preview() {
        let goal = "extract-the-thing";
        let hint = serde_json::json!({"keys": {"q": "number"}});
        let preview = "some-unique-preview-token-abc123";
        let (_system, user) = build_prompt_split(goal, &hint, preview);
        assert!(
            user.contains(goal),
            "user_message should contain goal literal"
        );
        assert!(
            user.contains("\"q\""),
            "user_message should contain serialised schema_hint keys"
        );
        assert!(
            user.contains(preview),
            "user_message should contain the output preview"
        );
    }

    #[test]
    fn scripted_output_settings_defaults_provider_to_auto() {
        let s = ScriptedOutputSettings::default();
        assert_eq!(s.provider, ScriptedOutputProviderMode::Auto);
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
