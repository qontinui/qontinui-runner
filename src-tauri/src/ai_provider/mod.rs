//! AI Provider Module
//!
//! Provides a unified interface for running AI prompts across different providers:
//! - Claude CLI (subscription-based, recommended)
//! - Claude API (per-token billing)
//! - Gemini CLI (OAuth or API key auth)
//! - Gemini API (direct HTTP calls)
//!
//! This module should be the ONLY place that spawns AI processes/calls.
//! Other modules should use this module to interact with AI providers.

pub mod account_usage;
pub(crate) mod anthropic_auth;
pub mod cache_aware_builder;
pub mod circuit_breaker;
mod claude_api;
pub(crate) mod claude_api_warm;
mod claude_cli;
pub(crate) use claude_cli::score_options_via_cli;
pub mod compaction_middleware;
mod config;
mod gemini_api;
mod gemini_cli;
pub(crate) mod gemma_local_warm;
pub mod middleware;
pub mod multimodal;
pub(crate) mod oauth_refresh;
pub mod oneshot;
mod process;
pub mod retry;
pub(crate) mod routing;
mod types;

// Re-export public API
pub use account_usage::{pick_best_account, pick_migration_target};
pub use cache_aware_builder::StructuredPrompt;
pub use config::{
    account_known_exhausted, get_account_statuses, get_effective_config_dir,
    get_resolved_config_dir, mark_account_rate_limited_with_duration, record_account_usage,
    rotate_account_on_rate_limit, set_resolved_config_dir, switch_to_account,
};
pub use multimodal::{ContentBlock, ImageSource, MultimodalPrompt};
pub use oneshot::{
    oneshot_for_settings, snapshot_oneshot_stats, OneshotClaudeApi, OneshotClaudeApiWarm,
    OneshotDisabled, OneshotError, OneshotGeminiApi, OneshotLlm, OneshotLlmExt, OneshotLocal,
    OneshotStats,
};
pub use routing::{
    run_prompt_multimodal, run_prompt_sync, run_prompt_with_middleware,
    run_prompt_with_model_override, run_prompt_with_model_override_multimodal,
    run_prompt_with_routing, run_prompt_with_routing_multimodal, run_prompt_with_structured_output,
    run_structured_prompt, run_structured_prompt_with_middleware,
};
pub use types::AiResponse;
