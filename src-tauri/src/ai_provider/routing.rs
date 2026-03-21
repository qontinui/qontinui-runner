#![allow(dead_code)]

use super::claude_api::{run_claude_api, run_claude_api_with_overrides};
use super::claude_cli::run_claude_cli;
use super::gemini_api::{run_gemini_api, run_gemini_api_with_overrides};
use super::gemini_cli::run_gemini_cli;
use super::retry::{retry_with_backoff, retry_with_fallback};
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

    info!(
        "Running AI prompt via {:?} (prompt length: {} chars)",
        ai_settings.provider,
        prompt.len()
    );

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
    }
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
    let routing_config = &ai_settings.routing;

    // Determine the model to use based on routing
    let model_override = if routing_config.enabled {
        let router = TaskRouter::new(routing_config.clone());
        let assessment = router.assess_and_route(context);
        let model = router.route_task(context);

        info!(
            "Task routing: complexity={:?}, confidence={:.2}, model={}",
            assessment.complexity, assessment.confidence, model
        );
        for factor in &assessment.factors {
            debug!("Routing factor: {}", factor);
        }

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
            // Gemini routing only works within Gemini models, warn if trying to route to Claude model
            if let Some(ref model) = model_override {
                if model.starts_with("claude") {
                    warn!("Cannot route to Claude model when using Gemini provider, using default Gemini model");
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
                    warn!("Cannot route to Claude model when using Gemini provider, using default Gemini model");
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
    }
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
        return retry_with_fallback("AI prompt", primary, Some(fallback));
    }

    // No fallback configured — still retry on transient errors.
    // Without this, a single transient failure (e.g., a 2-second timeout from
    // the specification agent) kills the entire workflow generation pipeline.
    retry_with_backoff("AI prompt", primary)
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
        };
    }

    // No explicit override — delegate to routing logic
    run_prompt_with_routing(prompt, context, doctor_handle)
}
