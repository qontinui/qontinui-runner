//! Output interceptor pipeline for terminal output processing.
//!
//! Provides a hook-based pipeline through which all PTY output passes.
//! Currently a pure pass-through — hooks will be added later for:
//! - Recording raw bytes to SQLite for session replay
//! - Detecting Claude Code output patterns for structured extraction
//! - Input recording for audit trails

use std::sync::Mutex;

/// Trait for processing terminal output as it flows through the pipeline.
///
/// Each hook receives the raw bytes and returns (potentially modified) bytes.
/// Hooks are chained in order — output of one feeds into the next.
pub trait OutputHook: Send + Sync {
    /// Process a chunk of terminal output.
    ///
    /// Returns the (potentially modified) data to pass to the next hook.
    /// The default implementation is a pass-through.
    fn process(&self, terminal_id: &str, data: &[u8]) -> Vec<u8>;
}

/// Pipeline that chains multiple `OutputHook` implementations.
///
/// All PTY output passes through this interceptor before being emitted
/// to the frontend. With zero hooks, it's a pure pass-through with
/// negligible overhead.
pub struct OutputInterceptor {
    hooks: Mutex<Vec<Box<dyn OutputHook>>>,
}

impl OutputInterceptor {
    /// Create a new interceptor with no hooks (pure pass-through).
    pub fn new() -> Self {
        Self {
            hooks: Mutex::new(Vec::new()),
        }
    }

    /// Add a hook to the end of the pipeline.
    pub fn add_hook(&self, hook: Box<dyn OutputHook>) {
        if let Ok(mut hooks) = self.hooks.lock() {
            hooks.push(hook);
        }
    }

    /// Process data through all hooks in order.
    ///
    /// With zero hooks, returns the input data unchanged (cloned to Vec).
    pub fn process(&self, terminal_id: &str, data: &[u8]) -> Vec<u8> {
        let hooks = match self.hooks.lock() {
            Ok(h) => h,
            Err(_) => return data.to_vec(),
        };

        if hooks.is_empty() {
            return data.to_vec();
        }

        let mut current = data.to_vec();
        for hook in hooks.iter() {
            current = hook.process(terminal_id, &current);
        }
        current
    }
}

impl Default for OutputInterceptor {
    fn default() -> Self {
        Self::new()
    }
}
