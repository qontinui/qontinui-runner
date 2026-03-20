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

mod claude_api;
mod claude_cli;
mod config;
mod gemini_api;
mod gemini_cli;
mod process;
mod retry;
mod routing;
mod types;

// Re-export public API
pub use config::{get_effective_config_dir, set_resolved_config_dir};
pub use routing::{run_prompt_sync, run_prompt_with_model_override, run_prompt_with_routing};
pub use types::AiResponse;
