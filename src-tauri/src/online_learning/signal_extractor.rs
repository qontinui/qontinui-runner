//! Zero-LLM signal extraction for model routing enrichment.
//!
//! Extracts structural and semantic signals from prompts without making
//! any LLM calls. These signals enrich the `ModelRoutingContext` used
//! by the UCB1 bandit router for better model selection.

use once_cell::sync::Lazy;
use regex::Regex;

/// Signals extracted from a prompt for model routing decisions.
#[derive(Debug, Clone, Default)]
pub struct RoutingSignals {
    /// Total word count in the prompt.
    pub word_count: usize,
    /// Number of code blocks (triple-backtick pairs) in the prompt.
    pub code_block_count: usize,
    /// Number of distinct file paths mentioned in the prompt.
    pub cross_file_dep_count: usize,
    /// Whether the prompt contains error traces or stack traces.
    pub has_error_context: bool,
    /// Current loop iteration number (later iterations = harder problems).
    pub iteration_number: u32,
    /// Whether the previous agentic fix attempt failed.
    pub previous_fix_failed: bool,
}

// Known source file extensions for file path detection
static FILE_PATH_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?:^|[\s\(`'"])([a-zA-Z0-9_./\\-]+\.(?:rs|ts|tsx|js|jsx|py|go|java|cpp|c|h|hpp|css|scss|html|json|toml|yaml|yml|sql|md|sh|bat))\b"#,
    )
    .unwrap()
});

// Error/stack trace patterns
static ERROR_TRACE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?i)(?:error\[E\d+\]|panicked at|stack backtrace|Traceback \(most recent|TypeError:|SyntaxError:|ReferenceError:|at\s+\w+\s+\(|(?:thread|goroutine)\s+'?\w+'?\s+panicked|Exception in thread|FAILED|error:|Error:|FAIL)"#,
    )
    .unwrap()
});

/// The triple-backtick marker used for code fences.
const CODE_FENCE: &str = "```";

impl RoutingSignals {
    /// Extract routing signals from a prompt string without any LLM call.
    pub fn extract(prompt: &str, iteration: u32, previous_failed: bool) -> Self {
        let word_count = prompt.split_whitespace().count();

        // Count code blocks (triple-backtick pairs)
        let backtick_count = prompt.matches(CODE_FENCE).count();
        let code_block_count = backtick_count / 2;

        // Count distinct file paths
        let mut file_paths = std::collections::HashSet::new();
        for cap in FILE_PATH_RE.captures_iter(prompt) {
            if let Some(m) = cap.get(1) {
                file_paths.insert(m.as_str().to_lowercase());
            }
        }
        let cross_file_dep_count = file_paths.len();

        // Detect error context
        let has_error_context = ERROR_TRACE_RE.is_match(prompt);

        Self {
            word_count,
            code_block_count,
            cross_file_dep_count,
            has_error_context,
            iteration_number: iteration,
            previous_fix_failed: previous_failed,
        }
    }

    /// Compute an effort bucket string for the routing encoder.
    /// Returns "simple", "moderate", or "complex".
    pub fn effort_bucket(&self) -> &'static str {
        match (self.code_block_count, self.cross_file_dep_count) {
            (0..=1, 0..=1) => "simple",
            (_, 0..=3) => "moderate",
            _ => "complex",
        }
    }

    /// Compute a retry status string for the routing encoder.
    /// Returns "retry" if the previous attempt failed, "fresh" otherwise.
    pub fn retry_status(&self) -> &'static str {
        if self.previous_fix_failed {
            "retry"
        } else {
            "fresh"
        }
    }
}
