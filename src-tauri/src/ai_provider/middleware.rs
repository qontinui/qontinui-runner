//! AI Middleware Module
//!
//! Provides a middleware chain for deterministic pre/post-processing of AI
//! prompts and responses. Inspired by open-swe's deterministic middleware hooks.
//!
//! Middleware runs before/after EACH AI model call, enabling:
//! - Pre-call: sanitize prompts, inject critical context, strip broken URLs
//! - Post-call: fix AI output (f-strings, jq commands, SDK URLs, regression steps)
//!
//! This replaces the ad-hoc sanitizer calls scattered across the hardener with
//! a composable, ordered middleware chain.

use super::types::AiResponse;
use tracing::{debug, info};

// ============================================================================
// Middleware trait
// ============================================================================

/// Context passed to middleware during pre/post processing.
#[derive(Debug, Clone, Default)]
pub struct MiddlewareContext {
    /// Current execution phase (e.g., "setup", "agentic", "verification", "hardener", "generation")
    pub phase: String,
    /// Task run ID (if available)
    pub task_run_id: Option<String>,
    /// Current iteration number (if applicable)
    pub iteration: Option<u32>,
}

impl MiddlewareContext {
    pub fn new(phase: &str) -> Self {
        Self {
            phase: phase.to_string(),
            ..Default::default()
        }
    }

    pub fn with_task_run_id(mut self, id: &str) -> Self {
        self.task_run_id = Some(id.to_string());
        self
    }

    pub fn with_iteration(mut self, iteration: u32) -> Self {
        self.iteration = Some(iteration);
        self
    }
}

/// Trait for deterministic middleware that processes AI prompts and responses.
///
/// Middleware is applied in chain order: pre_call runs first-to-last,
/// post_call runs last-to-first (like nested middleware in web frameworks).
pub trait AiMiddleware: Send + Sync {
    /// Human-readable name for logging.
    fn name(&self) -> &'static str;

    /// Transform the prompt before it's sent to the AI.
    /// Return `Some(modified_prompt)` to transform, or `None` to pass through unchanged.
    fn pre_call(&self, _prompt: &str, _ctx: &MiddlewareContext) -> Option<String> {
        None
    }

    /// Transform the AI response after receipt.
    /// Modify the response in-place (output text, success flag, etc.).
    ///
    /// Note: `post_call` is only invoked when the AI response is successful.
    /// Failed responses skip post-processing to preserve error diagnostics.
    /// If your middleware needs to handle errors, check `response.success`
    /// in a wrapper or handle it at the call site.
    fn post_call(&self, _response: &mut AiResponse, _ctx: &MiddlewareContext) {}
}

// ============================================================================
// Middleware chain
// ============================================================================

/// Ordered chain of middleware that wraps AI calls.
pub struct AiMiddlewareChain {
    middlewares: Vec<Box<dyn AiMiddleware>>,
}

impl AiMiddlewareChain {
    /// Create an empty middleware chain.
    pub fn new() -> Self {
        Self {
            middlewares: Vec::new(),
        }
    }

    /// Add a middleware to the end of the chain.
    pub fn add<M: AiMiddleware + 'static>(mut self, middleware: M) -> Self {
        self.middlewares.push(Box::new(middleware));
        self
    }

    /// Returns true if the chain has no middleware.
    pub fn is_empty(&self) -> bool {
        self.middlewares.is_empty()
    }

    /// Run all pre-call middleware in order, returning the (possibly transformed) prompt.
    pub fn run_pre_call(&self, prompt: &str, ctx: &MiddlewareContext) -> String {
        let mut current = prompt.to_string();

        for mw in &self.middlewares {
            if let Some(transformed) = mw.pre_call(&current, ctx) {
                debug!(
                    "Middleware '{}' transformed prompt ({} -> {} chars)",
                    mw.name(),
                    current.len(),
                    transformed.len()
                );
                current = transformed;
            }
        }

        current
    }

    /// Run all post-call middleware in reverse order (innermost first).
    pub fn run_post_call(&self, response: &mut AiResponse, ctx: &MiddlewareContext) {
        for mw in self.middlewares.iter().rev() {
            let before_len = response.output.len();
            mw.post_call(response, ctx);
            let after_len = response.output.len();

            if before_len != after_len {
                debug!(
                    "Middleware '{}' modified response ({} -> {} chars)",
                    mw.name(),
                    before_len,
                    after_len
                );
            }
        }
    }

    /// Log the middleware chain contents for debugging.
    pub fn log_chain(&self) {
        if !self.middlewares.is_empty() {
            let names: Vec<&str> = self.middlewares.iter().map(|m| m.name()).collect();
            info!("AI middleware chain: [{}]", names.join(" → "));
        }
    }
}

impl Default for AiMiddlewareChain {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    struct UppercaseMiddleware;
    impl AiMiddleware for UppercaseMiddleware {
        fn name(&self) -> &'static str {
            "uppercase"
        }
        fn pre_call(&self, prompt: &str, _ctx: &MiddlewareContext) -> Option<String> {
            Some(prompt.to_uppercase())
        }
    }

    struct AppendMiddleware;
    impl AiMiddleware for AppendMiddleware {
        fn name(&self) -> &'static str {
            "append"
        }
        fn post_call(&self, response: &mut AiResponse, _ctx: &MiddlewareContext) {
            response.output.push_str(" [processed]");
        }
    }

    struct NoopMiddleware;
    impl AiMiddleware for NoopMiddleware {
        fn name(&self) -> &'static str {
            "noop"
        }
    }

    #[test]
    fn test_empty_chain_passes_through() {
        let chain = AiMiddlewareChain::new();
        let ctx = MiddlewareContext::new("test");

        let result = chain.run_pre_call("hello", &ctx);
        assert_eq!(result, "hello");

        let mut response = AiResponse::success("world".to_string());
        chain.run_post_call(&mut response, &ctx);
        assert_eq!(response.output, "world");
    }

    #[test]
    fn test_pre_call_transforms_prompt() {
        let chain = AiMiddlewareChain::new().add(UppercaseMiddleware);
        let ctx = MiddlewareContext::new("test");

        let result = chain.run_pre_call("hello world", &ctx);
        assert_eq!(result, "HELLO WORLD");
    }

    #[test]
    fn test_post_call_modifies_response() {
        let chain = AiMiddlewareChain::new().add(AppendMiddleware);
        let ctx = MiddlewareContext::new("test");

        let mut response = AiResponse::success("output".to_string());
        chain.run_post_call(&mut response, &ctx);
        assert_eq!(response.output, "output [processed]");
    }

    #[test]
    fn test_noop_middleware_passes_through() {
        let chain = AiMiddlewareChain::new().add(NoopMiddleware);
        let ctx = MiddlewareContext::new("test");

        let result = chain.run_pre_call("hello", &ctx);
        assert_eq!(result, "hello");

        let mut response = AiResponse::success("world".to_string());
        chain.run_post_call(&mut response, &ctx);
        assert_eq!(response.output, "world");
    }

    #[test]
    fn test_chain_ordering() {
        // Pre-call runs first-to-last, post-call runs last-to-first
        let chain = AiMiddlewareChain::new()
            .add(UppercaseMiddleware)
            .add(NoopMiddleware)
            .add(AppendMiddleware);
        let ctx = MiddlewareContext::new("test");

        let result = chain.run_pre_call("hello", &ctx);
        assert_eq!(result, "HELLO"); // UppercaseMiddleware applied

        let mut response = AiResponse::success("output".to_string());
        chain.run_post_call(&mut response, &ctx);
        assert_eq!(response.output, "output [processed]"); // AppendMiddleware runs first (reverse)
    }

    #[test]
    fn test_middleware_context_builder() {
        let ctx = MiddlewareContext::new("hardener")
            .with_task_run_id("run-123")
            .with_iteration(3);

        assert_eq!(ctx.phase, "hardener");
        assert_eq!(ctx.task_run_id, Some("run-123".to_string()));
        assert_eq!(ctx.iteration, Some(3));
    }

    #[test]
    fn test_is_empty() {
        assert!(AiMiddlewareChain::new().is_empty());
        assert!(!AiMiddlewareChain::new().add(NoopMiddleware).is_empty());
    }
}
