//! Output interceptor pipeline for terminal output processing.
//!
//! Provides a hook-based pipeline through which all PTY output passes.
//! Currently a pure pass-through — hooks will be added later for:
//! - Recording raw bytes to SQLite for session replay
//! - Detecting Claude Code output patterns for structured extraction
//! - Input recording for audit trails

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;

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
///
/// ## Concurrency (perf: per-chunk hot path)
///
/// ONE `Arc<OutputInterceptor>` is built by [`super::manager::TerminalManager`]
/// and handed to EVERY session, and [`Self::process`] runs on each session's
/// PTY reader thread for every chunk. The hook list was previously a
/// `Mutex<Vec<…>>` locked *before* the empty-check, so all N reader threads
/// serialized on one exclusive lock on the hottest path in the process.
///
/// The list is written once at startup (`main.rs`, before any terminal exists)
/// and read on every chunk thereafter, so it is now read-mostly state:
/// - `hook_count` is a lock-free early-out — the zero-hook case takes no lock
///   at all;
/// - `hooks` is an `RwLock`, so concurrent reader threads share the read guard
///   instead of queueing behind each other.
///
/// This mirrors the existing in-repo precedent for read-mostly hot-path state,
/// `auto_response::COMPILED_RULES` (`RwLock<Vec<CompiledRule>>` + a cheap
/// read-lock early-out). No new dependency is introduced.
pub struct OutputInterceptor {
    hooks: RwLock<Vec<Box<dyn OutputHook>>>,
    /// Lock-free mirror of `hooks.len()` for the zero-hook early-out. Only
    /// ever mutated under the `hooks` write lock, so it can never observe a
    /// count for a hook that is not yet visible to readers.
    hook_count: AtomicUsize,
}

impl OutputInterceptor {
    /// Create a new interceptor with no hooks (pure pass-through).
    pub fn new() -> Self {
        Self {
            hooks: RwLock::new(Vec::new()),
            hook_count: AtomicUsize::new(0),
        }
    }

    /// Add a hook to the end of the pipeline.
    pub fn add_hook(&self, hook: Box<dyn OutputHook>) {
        if let Ok(mut hooks) = self.hooks.write() {
            hooks.push(hook);
            // Published AFTER the push, while the write lock is still held:
            // a reader that observes the new count is guaranteed to see the
            // new hook once it takes the read lock.
            self.hook_count.store(hooks.len(), Ordering::Release);
        }
    }

    /// Process data through all hooks in order.
    ///
    /// With zero hooks, returns the input data unchanged (cloned to Vec)
    /// without taking any lock.
    pub fn process(&self, terminal_id: &str, data: &[u8]) -> Vec<u8> {
        if self.hook_count.load(Ordering::Acquire) == 0 {
            return data.to_vec();
        }

        let hooks = match self.hooks.read() {
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
