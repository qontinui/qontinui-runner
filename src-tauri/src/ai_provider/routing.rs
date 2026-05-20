#![allow(dead_code)]

use super::cache_aware_builder::StructuredPrompt;
use super::circuit_breaker;
use super::claude_api::{
    run_claude_api, run_claude_api_cached, run_claude_api_multimodal,
    run_claude_api_with_overrides, run_claude_api_with_structured_output,
};
use super::claude_cli::run_claude_cli;
use super::gemini_api::{
    run_gemini_api, run_gemini_api_multimodal, run_gemini_api_with_overrides,
    run_gemini_api_with_structured_output,
};
use super::gemini_cli::run_gemini_cli;
use super::multimodal::MultimodalPrompt;
use super::retry::{
    retry_with_backoff, retry_with_backoff_tracked, retry_with_fallback,
    retry_with_fallback_tracked,
};
use super::types::AiResponse;
use crate::ai_router::{TaskContext, TaskRouter};
use crate::doctor::DoctorHandle;
use crate::settings::{self, AiProvider};
use tracing::{debug, info, warn};

/// Run an AI prompt synchronously and return the response.
///
/// This function selects the appropriate provider based on settings and
/// executes the prompt, waiting for the response. The process runs until
/// completion — health monitoring is handled by the Doctor service.
///
/// # Arguments
/// * `prompt` - The prompt to send to the AI
/// * `doctor_handle` - Optional Doctor health monitor handle for process registration
///
/// # Returns
/// `AiResponse` with success status, output, and any error message
pub fn run_prompt_sync(prompt: &str, doctor_handle: Option<&DoctorHandle>) -> AiResponse {
    let ai_settings = settings::get_ai_settings();
    let cb_key = circuit_breaker::provider_key(&ai_settings.provider);

    info!(
        "Running AI prompt via {:?} (prompt length: {} chars)",
        ai_settings.provider,
        prompt.len()
    );

    retry_with_backoff_tracked("AI prompt (sync)", Some(&cb_key), || {
        let ai_settings = settings::get_ai_settings();
        match ai_settings.provider {
            AiProvider::ClaudeCli => {
                run_claude_cli(prompt, &ai_settings.claude_cli, None, doctor_handle)
            }
            AiProvider::ClaudeApi => {
                run_claude_api(prompt, &ai_settings.claude_api, None, doctor_handle)
            }
            AiProvider::GeminiCli => {
                run_gemini_cli(prompt, &ai_settings.gemini_cli, None, doctor_handle)
            }
            AiProvider::GeminiApi => {
                run_gemini_api(prompt, &ai_settings.gemini_api, None, doctor_handle)
            }
            // TODO(tier-decoupling): wire Ollama/OpenAiCompatible into the agentic loop
            AiProvider::Ollama | AiProvider::OpenAiCompatible => AiResponse::error(
                "Local provider (Ollama / OpenAI-compatible) dispatch is not yet \
                 wired into the agentic loop. The provider is configured but full \
                 integration is deferred — use Claude or Gemini for now."
                    .to_string(),
            ),
        }
    })
}

/// Run an AI prompt with intelligent routing based on task complexity.
///
/// This function assesses the complexity of the task based on the provided context
/// and routes it to an appropriate model (simple tasks -> cheaper models, complex -> powerful models).
/// The process runs until completion — health monitoring is handled by the Doctor service.
///
/// # Arguments
/// * `prompt` - The prompt to send to the AI
/// * `context` - Task context for complexity assessment (files, verification criteria, etc.)
/// * `doctor_handle` - Optional Doctor health monitor handle for process registration
///
/// # Returns
/// `AiResponse` with success status, output, and any error message
pub fn run_prompt_with_routing(
    prompt: &str,
    context: &TaskContext,
    doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    let ai_settings = settings::get_ai_settings();
    let cb_key = circuit_breaker::provider_key(&ai_settings.provider);
    let routing_config = &ai_settings.routing;

    // Determine the model to use based on routing (bandit + heuristic fallback).
    // This is computed once — only the provider call retries on transient errors.
    let model_override = if routing_config.enabled {
        let router = TaskRouter::new(routing_config.clone());

        let model = if let Some(bandit_mtx) = crate::online_learning::model_router() {
            if let Ok(mut bandit) = bandit_mtx.lock() {
                let (model, decision) = router.route_task_with_bandit(context, &mut bandit);
                info!(
                    "Task routing: model={} source={:?} context={} explore={}",
                    model, decision.source, decision.context_key, decision.exploration
                );
                model
            } else {
                router.route_task(context)
            }
        } else {
            let assessment = router.assess_and_route(context);
            info!(
                "Task routing (heuristic): complexity={:?}, confidence={:.2}, model={}",
                assessment.complexity, assessment.confidence, assessment.selected_model
            );
            router.route_task(context)
        };

        Some(model)
    } else {
        debug!("Task routing disabled, using default model");
        None
    };

    info!(
        "Running AI prompt via {:?} (prompt length: {} chars, routed: {})",
        ai_settings.provider,
        prompt.len(),
        model_override.is_some()
    );

    // Wrap provider call in retry with account rotation on rate-limit.
    retry_with_backoff_tracked("AI prompt (routed)", Some(&cb_key), || {
        let ai_settings = settings::get_ai_settings();
        match ai_settings.provider {
            AiProvider::ClaudeCli => run_claude_cli(
                prompt,
                &ai_settings.claude_cli,
                model_override.as_deref(),
                doctor_handle,
            ),
            AiProvider::ClaudeApi => run_claude_api(
                prompt,
                &ai_settings.claude_api,
                model_override.as_deref(),
                doctor_handle,
            ),
            AiProvider::GeminiCli => {
                if let Some(ref model) = model_override {
                    if model.starts_with("claude") {
                        warn!("Cannot route to Claude model when using Gemini provider");
                        run_gemini_cli(prompt, &ai_settings.gemini_cli, None, doctor_handle)
                    } else {
                        run_gemini_cli(
                            prompt,
                            &ai_settings.gemini_cli,
                            model_override.as_deref(),
                            doctor_handle,
                        )
                    }
                } else {
                    run_gemini_cli(prompt, &ai_settings.gemini_cli, None, doctor_handle)
                }
            }
            AiProvider::GeminiApi => {
                if let Some(ref model) = model_override {
                    if model.starts_with("claude") {
                        warn!("Cannot route to Claude model when using Gemini provider");
                        run_gemini_api(prompt, &ai_settings.gemini_api, None, doctor_handle)
                    } else {
                        run_gemini_api(
                            prompt,
                            &ai_settings.gemini_api,
                            model_override.as_deref(),
                            doctor_handle,
                        )
                    }
                } else {
                    run_gemini_api(prompt, &ai_settings.gemini_api, None, doctor_handle)
                }
            }
            // TODO(tier-decoupling): wire Ollama/OpenAiCompatible into the agentic loop
            AiProvider::Ollama | AiProvider::OpenAiCompatible => AiResponse::error(
                "Local provider (Ollama / OpenAI-compatible) routed dispatch is not \
                 yet wired into the agentic loop. The provider is configured but \
                 full integration is deferred — use Claude or Gemini for now."
                    .to_string(),
            ),
        }
    })
}

/// Run an AI prompt with an explicit model/provider override.
///
/// Like `run_prompt_with_routing`, but allows an explicit model override that takes
/// precedence over the task router. This is used for per-phase model selection where
/// the workflow specifies different models for different phases.
///
/// Fallback chain:
/// 1. `model_override` parameter (per-phase setting)
/// 2. Task router (if routing is enabled and no explicit override)
/// 3. Global AI settings default model
///
/// Optional `temperature_override` and `max_tokens_override` are applied to API
/// providers (Claude API, Gemini API). CLI providers ignore these with a debug warning.
///
/// Optional `fallback_model`/`fallback_provider` define a secondary model+provider to
/// try when the primary fails with a retryable error.
pub fn run_prompt_with_model_override(
    prompt: &str,
    context: &TaskContext,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
    fallback_model: Option<&str>,
    fallback_provider: Option<&str>,
) -> AiResponse {
    // Resolve provider keys for circuit breaker tracking
    let ai_settings = settings::get_ai_settings();
    let primary_key = resolve_provider_key(provider_override, &ai_settings.provider);
    let fallback_key =
        fallback_provider.map(|p| resolve_provider_key(Some(p), &ai_settings.provider));

    // Build the primary call closure
    let primary = || {
        run_prompt_with_overrides_inner(
            prompt,
            context,
            doctor_handle,
            model_override,
            provider_override,
            temperature_override,
            max_tokens_override,
        )
    };

    // If fallback is configured, build fallback closure and use retry_with_fallback
    if fallback_model.is_some() || fallback_provider.is_some() {
        let fallback = || {
            run_prompt_with_overrides_inner(
                prompt,
                context,
                doctor_handle,
                fallback_model.or(model_override),
                fallback_provider.or(provider_override),
                temperature_override,
                max_tokens_override,
            )
        };
        return retry_with_fallback_tracked(
            "AI prompt",
            Some(&primary_key),
            fallback_key.as_deref(),
            primary,
            Some(fallback),
        );
    }

    // No fallback configured — still retry on transient errors with circuit breaker tracking.
    retry_with_backoff_tracked("AI prompt", Some(&primary_key), primary)
}

/// Resolve a provider override string to a circuit breaker key.
fn resolve_provider_key(provider_str: Option<&str>, default: &AiProvider) -> String {
    if let Some(p) = provider_str {
        match p {
            "claude_cli" => "claude_cli".to_string(),
            "claude_api" => "claude_api".to_string(),
            "gemini_cli" => "gemini_cli".to_string(),
            "gemini_api" => "gemini_api".to_string(),
            _ => circuit_breaker::provider_key(default),
        }
    } else {
        circuit_breaker::provider_key(default)
    }
}

/// Run an AI prompt with model override AND a middleware chain.
///
/// This wraps `run_prompt_with_model_override` with pre/post middleware processing:
/// - Pre-call: middleware transforms the prompt before it reaches the AI
/// - Post-call: middleware transforms the response before it's returned to the caller
///
/// Use this when deterministic sanitization is needed around AI calls (e.g., hardener).
pub fn run_prompt_with_middleware(
    prompt: &str,
    context: &TaskContext,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
    fallback_model: Option<&str>,
    fallback_provider: Option<&str>,
    middleware: &super::middleware::AiMiddlewareChain,
    middleware_ctx: &super::middleware::MiddlewareContext,
) -> AiResponse {
    middleware.log_chain();

    // Pre-call: transform prompt
    let transformed_prompt = middleware.run_pre_call(prompt, middleware_ctx);

    // Execute the AI call with the transformed prompt
    let mut response = run_prompt_with_model_override(
        &transformed_prompt,
        context,
        doctor_handle,
        model_override,
        provider_override,
        temperature_override,
        max_tokens_override,
        fallback_model,
        fallback_provider,
    );

    // Post-call: transform response
    if response.success {
        middleware.run_post_call(&mut response, middleware_ctx);
    }

    response
}

/// Inner implementation of model-override prompt execution.
fn run_prompt_with_overrides_inner(
    prompt: &str,
    context: &TaskContext,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
) -> AiResponse {
    // If we have an explicit model override, bypass the router entirely
    if let Some(model) = model_override {
        let ai_settings = settings::get_ai_settings();

        // Determine which provider to use
        let effective_provider = if let Some(prov) = provider_override {
            // Parse provider string to AiProvider enum
            match prov {
                "claude_cli" => AiProvider::ClaudeCli,
                "claude_api" => AiProvider::ClaudeApi,
                "gemini_cli" => AiProvider::GeminiCli,
                "gemini_api" => AiProvider::GeminiApi,
                _ => ai_settings.provider,
            }
        } else {
            ai_settings.provider
        };

        info!(
            "Running AI prompt with explicit override: provider={:?}, model={}, prompt_len={}",
            effective_provider,
            model,
            prompt.len()
        );

        return match effective_provider {
            AiProvider::ClaudeCli => {
                if temperature_override.is_some() || max_tokens_override.is_some() {
                    debug!(
                        "Claude CLI does not support temperature/max_tokens overrides; ignoring"
                    );
                }
                run_claude_cli(prompt, &ai_settings.claude_cli, Some(model), doctor_handle)
            }
            AiProvider::ClaudeApi => run_claude_api_with_overrides(
                prompt,
                &ai_settings.claude_api,
                Some(model),
                doctor_handle,
                temperature_override,
                max_tokens_override,
            ),
            AiProvider::GeminiCli => {
                if temperature_override.is_some() || max_tokens_override.is_some() {
                    debug!(
                        "Gemini CLI does not support temperature/max_tokens overrides; ignoring"
                    );
                }
                run_gemini_cli(prompt, &ai_settings.gemini_cli, Some(model), doctor_handle)
            }
            AiProvider::GeminiApi => run_gemini_api_with_overrides(
                prompt,
                &ai_settings.gemini_api,
                Some(model),
                doctor_handle,
                temperature_override,
                max_tokens_override,
            ),
            // TODO(tier-decoupling): wire Ollama/OpenAiCompatible into the agentic loop
            AiProvider::Ollama | AiProvider::OpenAiCompatible => AiResponse::error(
                "Local provider (Ollama / OpenAI-compatible) overridden dispatch is \
                 not yet wired into the agentic loop. The provider is configured \
                 but full integration is deferred — use Claude or Gemini for now."
                    .to_string(),
            ),
        };
    }

    // No explicit override — delegate to routing logic
    run_prompt_with_routing(prompt, context, doctor_handle)
}

// =============================================================================
// Structured Prompt Functions (with prompt caching support)
// =============================================================================

/// Run a structured prompt with cache-aware dispatch.
///
/// When the provider is `ClaudeApi` and the prompt has cached blocks,
/// this uses `run_claude_api_cached()` which sends `cache_control` breakpoints
/// on stable system blocks. For all other providers, the prompt is flattened
/// to a string and dispatched through the normal path.
pub fn run_structured_prompt(
    prompt: &StructuredPrompt,
    context: &TaskContext,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
    fallback_model: Option<&str>,
    fallback_provider: Option<&str>,
) -> AiResponse {
    let ai_settings = settings::get_ai_settings();

    let effective_provider = if let Some(prov) = provider_override {
        match prov {
            "claude_cli" => AiProvider::ClaudeCli,
            "claude_api" => AiProvider::ClaudeApi,
            "gemini_cli" => AiProvider::GeminiCli,
            "gemini_api" => AiProvider::GeminiApi,
            _ => ai_settings.provider,
        }
    } else {
        ai_settings.provider
    };

    let effective_model = if let Some(model) = model_override {
        Some(model.to_string())
    } else if ai_settings.routing.enabled {
        let router = TaskRouter::new(ai_settings.routing.clone());
        let model = if let Some(bandit_mtx) = crate::online_learning::model_router() {
            if let Ok(mut bandit) = bandit_mtx.lock() {
                let (model, decision) = router.route_task_with_bandit(context, &mut bandit);
                info!(
                    "Structured prompt routing: model={} source={:?} explore={}",
                    model, decision.source, decision.exploration
                );
                model
            } else {
                router.route_task(context)
            }
        } else {
            router.route_task(context)
        };
        Some(model)
    } else {
        None
    };

    // Use cache-aware path for Claude API when we have cached blocks
    if matches!(effective_provider, AiProvider::ClaudeApi) && prompt.has_cached_blocks() {
        info!(
            "Using cache-aware Claude API path ({} cached blocks, {} dynamic blocks)",
            prompt.cached_system_blocks.len(),
            prompt.dynamic_system_blocks.len(),
        );

        let primary = || {
            run_claude_api_cached(
                prompt,
                &ai_settings.claude_api,
                effective_model.as_deref(),
                doctor_handle,
                temperature_override,
                max_tokens_override,
            )
        };

        if fallback_model.is_some() || fallback_provider.is_some() {
            let fallback_flat = prompt.to_flat_string();
            let fallback = || {
                run_prompt_with_overrides_inner(
                    &fallback_flat,
                    context,
                    doctor_handle,
                    fallback_model.or(model_override),
                    fallback_provider.or(provider_override),
                    temperature_override,
                    max_tokens_override,
                )
            };
            return retry_with_fallback("Claude API (cached)", primary, Some(fallback));
        }

        return retry_with_backoff("Claude API (cached)", primary);
    }

    // Non-Claude or no cached blocks: flatten and use standard path
    let flat_prompt = prompt.to_flat_string();
    run_prompt_with_model_override(
        &flat_prompt,
        context,
        doctor_handle,
        model_override,
        provider_override,
        temperature_override,
        max_tokens_override,
        fallback_model,
        fallback_provider,
    )
}

/// Run a structured prompt with middleware chain support.
pub fn run_structured_prompt_with_middleware(
    prompt: &StructuredPrompt,
    context: &TaskContext,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
    fallback_model: Option<&str>,
    fallback_provider: Option<&str>,
    middleware: &super::middleware::AiMiddlewareChain,
    middleware_ctx: &super::middleware::MiddlewareContext,
) -> AiResponse {
    middleware.log_chain();

    // Middleware only transforms the user message — cached system blocks
    // must remain stable to benefit from caching.
    let transformed_user_message = middleware.run_pre_call(&prompt.user_message, middleware_ctx);

    let transformed_prompt = StructuredPrompt {
        cached_system_blocks: prompt.cached_system_blocks.clone(),
        dynamic_system_blocks: prompt.dynamic_system_blocks.clone(),
        user_message: transformed_user_message,
    };

    let mut response = run_structured_prompt(
        &transformed_prompt,
        context,
        doctor_handle,
        model_override,
        provider_override,
        temperature_override,
        max_tokens_override,
        fallback_model,
        fallback_provider,
    );

    if response.success {
        middleware.run_post_call(&mut response, middleware_ctx);
    }

    response
}

// =============================================================================
// Multimodal Prompt Functions
// =============================================================================

/// Run a multimodal AI prompt (text + images) synchronously.
///
/// For API providers (Claude API, Gemini API), this sends proper content block
/// arrays with image data. For CLI providers, falls back to text-only with a
/// warning since CLIs don't support image content blocks.
pub fn run_prompt_multimodal(
    prompt: &MultimodalPrompt,
    doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    let ai_settings = settings::get_ai_settings();

    info!(
        "Running multimodal AI prompt via {:?} (blocks: {}, has_images: {})",
        ai_settings.provider,
        prompt.content.len(),
        prompt.has_images()
    );

    match ai_settings.provider {
        AiProvider::ClaudeCli => {
            if prompt.has_images() {
                warn!(
                    "Claude CLI does not support image content blocks; falling back to text-only"
                );
            }
            run_claude_cli(
                &prompt.text_content(),
                &ai_settings.claude_cli,
                None,
                doctor_handle,
            )
        }
        AiProvider::ClaudeApi => run_claude_api_multimodal(
            prompt,
            &ai_settings.claude_api,
            None,
            doctor_handle,
            None,
            None,
        ),
        AiProvider::GeminiCli => {
            if prompt.has_images() {
                warn!(
                    "Gemini CLI does not support image content blocks; falling back to text-only"
                );
            }
            run_gemini_cli(
                &prompt.text_content(),
                &ai_settings.gemini_cli,
                None,
                doctor_handle,
            )
        }
        AiProvider::GeminiApi => run_gemini_api_multimodal(
            prompt,
            &ai_settings.gemini_api,
            None,
            doctor_handle,
            None,
            None,
        ),
        // TODO(tier-decoupling): wire Ollama/OpenAiCompatible into the agentic loop
        AiProvider::Ollama | AiProvider::OpenAiCompatible => AiResponse::error(
            "Local provider (Ollama / OpenAI-compatible) multimodal dispatch is \
             not yet wired into the agentic loop. The provider is configured but \
             full integration is deferred — use Claude or Gemini for now."
                .to_string(),
        ),
    }
}

/// Run a multimodal AI prompt with intelligent routing based on task complexity.
///
/// Like `run_prompt_with_routing` but supports image content blocks.
/// For CLI providers, falls back to text-only.
pub fn run_prompt_with_routing_multimodal(
    prompt: &MultimodalPrompt,
    context: &TaskContext,
    doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    let ai_settings = settings::get_ai_settings();
    let routing_config = &ai_settings.routing;

    let model_override = if routing_config.enabled {
        let router = TaskRouter::new(routing_config.clone());
        let model = if let Some(bandit_mtx) = crate::online_learning::model_router() {
            if let Ok(mut bandit) = bandit_mtx.lock() {
                let (model, decision) = router.route_task_with_bandit(context, &mut bandit);
                info!(
                    "Multimodal routing: model={} source={:?} explore={}",
                    model, decision.source, decision.exploration
                );
                model
            } else {
                router.route_task(context)
            }
        } else {
            router.route_task(context)
        };
        Some(model)
    } else {
        None
    };

    match ai_settings.provider {
        AiProvider::ClaudeCli => {
            if prompt.has_images() {
                warn!(
                    "Claude CLI does not support image content blocks; falling back to text-only"
                );
            }
            run_claude_cli(
                &prompt.text_content(),
                &ai_settings.claude_cli,
                model_override.as_deref(),
                doctor_handle,
            )
        }
        AiProvider::ClaudeApi => run_claude_api_multimodal(
            prompt,
            &ai_settings.claude_api,
            model_override.as_deref(),
            doctor_handle,
            None,
            None,
        ),
        AiProvider::GeminiCli => {
            if prompt.has_images() {
                warn!(
                    "Gemini CLI does not support image content blocks; falling back to text-only"
                );
            }
            let effective_model = model_override.as_deref().and_then(|m| {
                if m.starts_with("claude") {
                    warn!("Cannot route to Claude model when using Gemini provider");
                    None
                } else {
                    Some(m)
                }
            });
            run_gemini_cli(
                &prompt.text_content(),
                &ai_settings.gemini_cli,
                effective_model,
                doctor_handle,
            )
        }
        AiProvider::GeminiApi => {
            let effective_model = model_override.as_deref().and_then(|m| {
                if m.starts_with("claude") {
                    warn!("Cannot route to Claude model when using Gemini provider");
                    None
                } else {
                    Some(m)
                }
            });
            run_gemini_api_multimodal(
                prompt,
                &ai_settings.gemini_api,
                effective_model,
                doctor_handle,
                None,
                None,
            )
        }
        // TODO(tier-decoupling): wire Ollama/OpenAiCompatible into the agentic loop
        AiProvider::Ollama | AiProvider::OpenAiCompatible => AiResponse::error(
            "Local provider (Ollama / OpenAI-compatible) multimodal-routed \
             dispatch is not yet wired into the agentic loop. The provider is \
             configured but full integration is deferred — use Claude or Gemini \
             for now."
                .to_string(),
        ),
    }
}

/// Run a multimodal AI prompt with explicit model/provider overrides.
///
/// Like `run_prompt_with_model_override` but supports image content blocks.
/// For CLI providers, falls back to text-only.
pub fn run_prompt_with_model_override_multimodal(
    prompt: &MultimodalPrompt,
    context: &TaskContext,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
) -> AiResponse {
    let ai_settings = settings::get_ai_settings();

    // Determine the model and provider
    let effective_model = model_override;
    let effective_provider = if let Some(prov) = provider_override {
        match prov {
            "claude_cli" => AiProvider::ClaudeCli,
            "claude_api" => AiProvider::ClaudeApi,
            "gemini_cli" => AiProvider::GeminiCli,
            "gemini_api" => AiProvider::GeminiApi,
            _ => ai_settings.provider,
        }
    } else {
        // For multimodal with images, prefer API providers over CLI
        if prompt.has_images() {
            match ai_settings.provider {
                AiProvider::ClaudeCli => {
                    info!("Auto-switching from Claude CLI to Claude API for multimodal prompt");
                    AiProvider::ClaudeApi
                }
                AiProvider::GeminiCli => {
                    info!("Auto-switching from Gemini CLI to Gemini API for multimodal prompt");
                    AiProvider::GeminiApi
                }
                other => other,
            }
        } else {
            ai_settings.provider
        }
    };

    info!(
        "Running multimodal prompt with overrides: provider={:?}, model={:?}, has_images={}",
        effective_provider,
        effective_model,
        prompt.has_images()
    );

    match effective_provider {
        AiProvider::ClaudeCli => {
            if prompt.has_images() {
                warn!("Claude CLI does not support multimodal; using text-only fallback");
            }
            run_claude_cli(
                &prompt.text_content(),
                &ai_settings.claude_cli,
                effective_model,
                doctor_handle,
            )
        }
        AiProvider::ClaudeApi => run_claude_api_multimodal(
            prompt,
            &ai_settings.claude_api,
            effective_model,
            doctor_handle,
            temperature_override,
            max_tokens_override,
        ),
        AiProvider::GeminiCli => {
            if prompt.has_images() {
                warn!("Gemini CLI does not support multimodal; using text-only fallback");
            }
            run_gemini_cli(
                &prompt.text_content(),
                &ai_settings.gemini_cli,
                effective_model,
                doctor_handle,
            )
        }
        AiProvider::GeminiApi => run_gemini_api_multimodal(
            prompt,
            &ai_settings.gemini_api,
            effective_model,
            doctor_handle,
            temperature_override,
            max_tokens_override,
        ),
        // TODO(tier-decoupling): wire Ollama/OpenAiCompatible into the agentic loop
        AiProvider::Ollama | AiProvider::OpenAiCompatible => AiResponse::error(
            "Local provider (Ollama / OpenAI-compatible) multimodal-overridden \
             dispatch is not yet wired into the agentic loop. The provider is \
             configured but full integration is deferred — use Claude or Gemini \
             for now."
                .to_string(),
        ),
    }
}

// =============================================================================
// Structured Output Functions (server-side schema enforcement)
// =============================================================================

/// Run an AI prompt with server-side structured output enforcement.
///
/// When the current provider supports structured output (Claude API via tool_use,
/// Gemini API via responseSchema), the JSON Schema is sent to the provider for
/// server-side enforcement. For CLI providers or unsupported APIs, falls back to
/// the standard prompt path (the caller should rely on the middleware
/// retry-with-validation path for schema compliance).
///
/// # Arguments
/// * `prompt` - The prompt to send to the AI
/// * `schema` - JSON Schema to enforce on the response
/// * `schema_name` - Human-readable name for the schema (used for logging and tool naming)
/// * `model_override` - Optional model override
/// * `provider_override` - Optional provider override
/// * `temperature_override` - Optional temperature override
/// * `max_tokens_override` - Optional max tokens override
pub fn run_prompt_with_structured_output(
    prompt: &str,
    schema: &serde_json::Value,
    schema_name: &str,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
) -> AiResponse {
    let ai_settings = settings::get_ai_settings();

    // The warm provider is reachable only via the string override — it is not
    // a member of the AiProvider enum so the user chat flow can never select
    // it. This keeps the emitter subsystem isolated from the user's chat
    // provider choice.
    //
    // NOTE: the scripted-output emitter bypasses this routing entry point and
    // calls `claude_api_warm::run_claude_api_warm_with_structured_output`
    // directly so it can pass a split `(system_prefix, user_message)` pair
    // (required for meaningful prompt caching — see decision 4 in
    // plans/warm-emitter-provider-2026-04-21.md). This override path exists
    // only for external callers that want a quick warm structured call with a
    // single-string prompt; it sends the whole prompt as the `user_message`
    // with an empty `system_prefix`, so caching never fires through this
    // path. If a new caller needs caching, have it bypass routing too.
    if provider_override == Some("claude_api_warm") {
        info!(
            "Running structured output prompt via claude_api_warm (schema: {}, prompt_len: {}) — no system_prefix, caching disabled on this path",
            schema_name,
            prompt.len()
        );
        return crate::ai_provider::claude_api_warm::run_claude_api_warm_with_structured_output(
            "",     // no stable system prefix through the routing fast path
            prompt, // the whole prompt is treated as the user message
            &ai_settings.claude_api,
            model_override,
            doctor_handle,
            temperature_override,
            max_tokens_override,
            schema,
            schema_name,
        );
    }

    let effective_provider = if let Some(prov) = provider_override {
        match prov {
            "claude_cli" => AiProvider::ClaudeCli,
            "claude_api" => AiProvider::ClaudeApi,
            "gemini_cli" => AiProvider::GeminiCli,
            "gemini_api" => AiProvider::GeminiApi,
            _ => ai_settings.provider,
        }
    } else {
        ai_settings.provider
    };

    info!(
        "Running structured output prompt via {:?} (schema: {}, prompt_len: {})",
        effective_provider,
        schema_name,
        prompt.len()
    );

    match effective_provider {
        AiProvider::ClaudeApi => run_claude_api_with_structured_output(
            prompt,
            &ai_settings.claude_api,
            model_override,
            doctor_handle,
            temperature_override,
            max_tokens_override,
            schema,
            schema_name,
        ),
        AiProvider::GeminiApi => run_gemini_api_with_structured_output(
            prompt,
            &ai_settings.gemini_api,
            model_override,
            doctor_handle,
            temperature_override,
            max_tokens_override,
            schema,
            schema_name,
        ),
        // CLI providers don't support structured output — fall through to standard path.
        // The StructuredOutputMiddleware will handle validation and retries.
        AiProvider::ClaudeCli => {
            debug!("Claude CLI does not support structured output; using standard path");
            run_claude_cli(
                prompt,
                &ai_settings.claude_cli,
                model_override,
                doctor_handle,
            )
        }
        AiProvider::GeminiCli => {
            debug!("Gemini CLI does not support structured output; using standard path");
            run_gemini_cli(
                prompt,
                &ai_settings.gemini_cli,
                model_override,
                doctor_handle,
            )
        }
        // TODO(tier-decoupling): wire Ollama/OpenAiCompatible into the agentic loop
        AiProvider::Ollama | AiProvider::OpenAiCompatible => AiResponse::error(
            "Local provider (Ollama / OpenAI-compatible) structured-output \
             dispatch is not yet wired into the agentic loop. The provider is \
             configured but full integration is deferred — use Claude or Gemini \
             for now."
                .to_string(),
        ),
    }
}
