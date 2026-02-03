//! Parser for progress markers in AI output.
//!
//! Detects and parses progress patterns from Claude's output stream.
//!
//! ## Supported patterns:
//!
//! ### Explicit markers:
//! - `[PROGRESS: 50/100]` - Explicit progress marker
//! - `[PROGRESS: 75%]` - Percentage-based marker
//! - `[PROGRESS: file_progress: 50/100]` - Typed progress marker
//!
//! ### Natural language patterns:
//! - `Analyzing file 25 of 50...`
//! - `Processing 75%...`
//! - `Completed 10/20 tests`
//! - `Reviewed 5 of 12 files`
//!
//! ### Processing patterns:
//! - `Processing file 3 of 10...`
//! - `Analyzing 5/20...`
//! - `Completed 8 of 15`
//! - `[3/10] filename.rs`
//!
//! ### Task list patterns:
//! - `Task 2 of 5:`
//! - `Step 3/7:`
//! - `(4/10) description`
//!
//! ### Iteration patterns:
//! - `Iteration 3 of 5`
//! - `Round 2/4`
//! - `Pass 1 of 3`
//!
//! ### Batch patterns:
//! - `Batch 2 of 5`
//! - `Chunk 3/8`
//! - `Processing batch 4...`
//!
//! ### Test patterns:
//! - `Running test 5 of 20`
//! - `Test 8/15 passed`
//! - `[checkmark] 12/20 tests`
//!
//! ## Example usage:
//! ```ignore
//! let mut parser = ProgressParser::new();
//! for line in ai_output.lines() {
//!     if let Some(progress) = parser.parse_line(line) {
//!         // Store progress marker
//!     }
//! }
//! ```

use regex::Regex;
use std::sync::OnceLock;

/// Parsed progress information from AI output.
#[derive(Debug, Clone)]
pub struct ParsedProgress {
    /// Type of progress (e.g., "file_progress", "analysis_progress")
    pub marker_type: String,
    /// Current value
    pub current: u64,
    /// Total value (if known)
    pub total: Option<u64>,
    /// Human-readable description
    pub description: Option<String>,
    /// Sub-step ID for STEP_COMPLETE markers (None for other progress types)
    pub sub_step_id: Option<String>,
}

impl ParsedProgress {
    /// Calculate percentage (0-100), if total is known.
    pub fn percentage(&self) -> Option<f64> {
        self.total.map(|t| {
            if t == 0 {
                0.0
            } else {
                (self.current as f64 / t as f64) * 100.0
            }
        })
    }
}

// Regex patterns for progress detection
static STEP_COMPLETE_PATTERN: OnceLock<Regex> = OnceLock::new();
static EXPLICIT_PROGRESS_PATTERN: OnceLock<Regex> = OnceLock::new();
static EXPLICIT_TYPED_PROGRESS_PATTERN: OnceLock<Regex> = OnceLock::new();
static PERCENTAGE_PROGRESS_PATTERN: OnceLock<Regex> = OnceLock::new();
static NATURAL_X_OF_Y_PATTERN: OnceLock<Regex> = OnceLock::new();
static NATURAL_COMPLETED_PATTERN: OnceLock<Regex> = OnceLock::new();

// New patterns for enhanced detection
static PROCESSING_ACTION_PATTERN: OnceLock<Regex> = OnceLock::new();
static BRACKET_PROGRESS_PATTERN: OnceLock<Regex> = OnceLock::new();
static TASK_STEP_PATTERN: OnceLock<Regex> = OnceLock::new();
static PAREN_PROGRESS_PATTERN: OnceLock<Regex> = OnceLock::new();
static ITERATION_PATTERN: OnceLock<Regex> = OnceLock::new();
static BATCH_PATTERN: OnceLock<Regex> = OnceLock::new();
static TEST_STATUS_PATTERN: OnceLock<Regex> = OnceLock::new();
static CHECKMARK_TEST_PATTERN: OnceLock<Regex> = OnceLock::new();

fn get_step_complete_pattern() -> &'static Regex {
    STEP_COMPLETE_PATTERN.get_or_init(|| {
        // Matches: [STEP_COMPLETE: step_id] or [STEP_COMPLETE:step_id]
        // Step ID can contain alphanumeric characters, underscores, and hyphens
        Regex::new(r"(?i)\[STEP_COMPLETE:\s*([a-zA-Z0-9_-]+)\]").unwrap()
    })
}

fn get_explicit_pattern() -> &'static Regex {
    EXPLICIT_PROGRESS_PATTERN.get_or_init(|| {
        // Matches: [PROGRESS: 50/100] or [PROGRESS:50/100]
        Regex::new(r"(?i)\[PROGRESS:\s*(\d+)/(\d+)\]").unwrap()
    })
}

fn get_explicit_typed_pattern() -> &'static Regex {
    EXPLICIT_TYPED_PROGRESS_PATTERN.get_or_init(|| {
        // Matches: [PROGRESS: file_progress: 50/100]
        Regex::new(r"(?i)\[PROGRESS:\s*([a-z_]+):\s*(\d+)/(\d+)\]").unwrap()
    })
}

fn get_percentage_pattern() -> &'static Regex {
    PERCENTAGE_PROGRESS_PATTERN.get_or_init(|| {
        // Matches: [PROGRESS: 75%] or Processing 75%
        Regex::new(r"(?i)(?:\[PROGRESS:\s*)?(\d+)%\]?").unwrap()
    })
}

fn get_natural_x_of_y_pattern() -> &'static Regex {
    NATURAL_X_OF_Y_PATTERN.get_or_init(|| {
        // Matches: "file 25 of 50", "item 10 of 100", etc.
        Regex::new(r"(?i)(file|item|test|step|task|element|component|module|function|class)s?\s+(\d+)\s+of\s+(\d+)").unwrap()
    })
}

fn get_natural_completed_pattern() -> &'static Regex {
    NATURAL_COMPLETED_PATTERN.get_or_init(|| {
        // Matches: "Completed 10/20", "Processed 5/10", "Analyzed 3/8"
        Regex::new(r"(?i)(completed|processed|analyzed|reviewed|checked|tested|scanned|examined)\s+(\d+)/(\d+)").unwrap()
    })
}

// --- New pattern getters ---

fn get_processing_action_pattern() -> &'static Regex {
    PROCESSING_ACTION_PATTERN.get_or_init(|| {
        // Matches: "Processing file 3 of 10...", "Analyzing 5/20...", "Completed 8 of 15"
        // Uses possessive-like matching with atomic groups simulated by specific patterns
        Regex::new(r"(?i)^(Processing|Analyzing|Completed)\s+(?:file\s+)?(\d+)\s*(?:of|/)\s*(\d+)")
            .unwrap()
    })
}

fn get_bracket_progress_pattern() -> &'static Regex {
    BRACKET_PROGRESS_PATTERN.get_or_init(|| {
        // Matches: "[3/10] filename.rs", "[15/100] Processing..."
        // Must be at start of line to avoid false positives
        Regex::new(r"^\[(\d+)/(\d+)\]\s+\S").unwrap()
    })
}

fn get_task_step_pattern() -> &'static Regex {
    TASK_STEP_PATTERN.get_or_init(|| {
        // Matches: "Task 2 of 5:", "Step 3/7:", "Task 4 of 10 -"
        Regex::new(r"(?i)^(Task|Step)\s+(\d+)\s*(?:of|/)\s*(\d+)\s*[:\-]").unwrap()
    })
}

fn get_paren_progress_pattern() -> &'static Regex {
    PAREN_PROGRESS_PATTERN.get_or_init(|| {
        // Matches: "(4/10) description" at start, or "description (4/10)" at end
        Regex::new(r"(?:^\((\d+)/(\d+)\)\s|\s\((\d+)/(\d+)\)$)").unwrap()
    })
}

fn get_iteration_pattern() -> &'static Regex {
    ITERATION_PATTERN.get_or_init(|| {
        // Matches: "Iteration 3 of 5", "Round 2/4", "Pass 1 of 3"
        Regex::new(r"(?i)(Iteration|Round|Pass)\s+(\d+)\s*(?:of|/)\s*(\d+)").unwrap()
    })
}

fn get_batch_pattern() -> &'static Regex {
    BATCH_PATTERN.get_or_init(|| {
        // Matches: "Batch 2 of 5", "Chunk 3/8", "Processing batch 4 of 10"
        Regex::new(r"(?i)(?:Processing\s+)?(Batch|Chunk)\s+(\d+)\s*(?:of|/)\s*(\d+)").unwrap()
    })
}

fn get_test_status_pattern() -> &'static Regex {
    TEST_STATUS_PATTERN.get_or_init(|| {
        // Matches: "Running test 5 of 20", "Test 8/15 passed", "Test 3/10 failed"
        Regex::new(r"(?i)(?:Running\s+)?[Tt]est\s+(\d+)\s*(?:of|/)\s*(\d+)(?:\s+(?:passed|failed|skipped))?").unwrap()
    })
}

fn get_checkmark_test_pattern() -> &'static Regex {
    CHECKMARK_TEST_PATTERN.get_or_init(|| {
        // Matches: "✓ 12/20 tests", "✔ 5/10 tests", "✗ 3/15 tests"
        // Also matches ASCII variants: "[x] 12/20 tests", "[v] 5/10 tests"
        Regex::new(r"(?:[\u2713\u2714\u2717\u2718]|\[[xvX]\])\s*(\d+)/(\d+)\s+tests?").unwrap()
    })
}

/// Parser for progress markers in AI output.
#[derive(Debug, Default)]
pub struct ProgressParser {
    /// Last parsed progress (for deduplication)
    last_progress: Option<(u64, Option<u64>)>,
}

impl ProgressParser {
    /// Create a new progress parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse a line of AI output for progress markers.
    ///
    /// Returns Some(ParsedProgress) if a progress pattern is detected.
    /// Returns None if no progress pattern is found or if it's a duplicate.
    pub fn parse_line(&mut self, line: &str) -> Option<ParsedProgress> {
        // Try patterns in order of specificity (most specific first)

        // 0. STEP_COMPLETE markers: [STEP_COMPLETE: step_id]
        // This is checked first because it's a special completion marker for sub-steps
        // within consolidated prompts, not a progress indicator.
        if let Some(caps) = get_step_complete_pattern().captures(line) {
            let sub_step_id = caps.get(1)?.as_str().to_string();
            return Some(ParsedProgress {
                marker_type: "step_complete".to_string(),
                current: 0,
                total: None,
                description: Some(line.trim().to_string()),
                sub_step_id: Some(sub_step_id),
            });
        }

        // 1. Explicit typed progress: [PROGRESS: file_progress: 50/100]
        if let Some(caps) = get_explicit_typed_pattern().captures(line) {
            let marker_type = caps.get(1)?.as_str().to_lowercase();
            let current: u64 = caps.get(2)?.as_str().parse().ok()?;
            let total: u64 = caps.get(3)?.as_str().parse().ok()?;

            if self.is_duplicate(current, Some(total)) {
                return None;
            }

            return Some(ParsedProgress {
                marker_type,
                current,
                total: Some(total),
                description: Some(line.trim().to_string()),
                sub_step_id: None,
            });
        }

        // 2. Explicit progress: [PROGRESS: 50/100]
        if let Some(caps) = get_explicit_pattern().captures(line) {
            let current: u64 = caps.get(1)?.as_str().parse().ok()?;
            let total: u64 = caps.get(2)?.as_str().parse().ok()?;

            if self.is_duplicate(current, Some(total)) {
                return None;
            }

            return Some(ParsedProgress {
                marker_type: "iteration_progress".to_string(),
                current,
                total: Some(total),
                description: Some(line.trim().to_string()),
                sub_step_id: None,
            });
        }

        // 3. Bracket progress: "[3/10] filename.rs" (before natural patterns)
        if let Some(caps) = get_bracket_progress_pattern().captures(line) {
            let current: u64 = caps.get(1)?.as_str().parse().ok()?;
            let total: u64 = caps.get(2)?.as_str().parse().ok()?;

            if self.is_duplicate(current, Some(total)) {
                return None;
            }

            return Some(ParsedProgress {
                marker_type: "build_progress".to_string(),
                current,
                total: Some(total),
                description: Some(line.trim().to_string()),
                sub_step_id: None,
            });
        }

        // 4. Task/Step patterns: "Task 2 of 5:", "Step 3/7:" (before generic natural patterns)
        if let Some(caps) = get_task_step_pattern().captures(line) {
            let item_type = caps.get(1)?.as_str().to_lowercase();
            let current: u64 = caps.get(2)?.as_str().parse().ok()?;
            let total: u64 = caps.get(3)?.as_str().parse().ok()?;

            if self.is_duplicate(current, Some(total)) {
                return None;
            }

            let marker_type = match item_type.as_str() {
                "task" => "task_progress",
                _ => "step_progress",
            };

            return Some(ParsedProgress {
                marker_type: marker_type.to_string(),
                current,
                total: Some(total),
                description: Some(line.trim().to_string()),
                sub_step_id: None,
            });
        }

        // 5. Processing action patterns: "Processing file 3 of 10...", "Analyzing 5/20..."
        // (before generic natural patterns - has specific action verbs at start)
        if let Some(caps) = get_processing_action_pattern().captures(line) {
            let action = caps.get(1)?.as_str().to_lowercase();
            let current: u64 = caps.get(2)?.as_str().parse().ok()?;
            let total: u64 = caps.get(3)?.as_str().parse().ok()?;

            if self.is_duplicate(current, Some(total)) {
                return None;
            }

            let marker_type = match action.as_str() {
                "analyzing" => "analysis_progress",
                "completed" => "iteration_progress",
                _ => "processing_progress",
            };

            return Some(ParsedProgress {
                marker_type: marker_type.to_string(),
                current,
                total: Some(total),
                description: Some(line.trim().to_string()),
                sub_step_id: None,
            });
        }

        // 6. Iteration patterns: "Iteration 3 of 5", "Round 2/4", "Pass 1 of 3"
        // (before generic natural patterns - specific keywords)
        if let Some(caps) = get_iteration_pattern().captures(line) {
            let item_type = caps.get(1)?.as_str().to_lowercase();
            let current: u64 = caps.get(2)?.as_str().parse().ok()?;
            let total: u64 = caps.get(3)?.as_str().parse().ok()?;

            if self.is_duplicate(current, Some(total)) {
                return None;
            }

            let marker_type = match item_type.as_str() {
                "pass" => "pass_progress",
                "round" => "round_progress",
                _ => "iteration_progress",
            };

            return Some(ParsedProgress {
                marker_type: marker_type.to_string(),
                current,
                total: Some(total),
                description: Some(line.trim().to_string()),
                sub_step_id: None,
            });
        }

        // 7. Batch patterns: "Batch 2 of 5", "Chunk 3/8"
        // (before generic natural patterns - specific keywords)
        if let Some(caps) = get_batch_pattern().captures(line) {
            let item_type = caps.get(1)?.as_str().to_lowercase();
            let current: u64 = caps.get(2)?.as_str().parse().ok()?;
            let total: u64 = caps.get(3)?.as_str().parse().ok()?;

            if self.is_duplicate(current, Some(total)) {
                return None;
            }

            let marker_type = match item_type.as_str() {
                "chunk" => "chunk_progress",
                _ => "batch_progress",
            };

            return Some(ParsedProgress {
                marker_type: marker_type.to_string(),
                current,
                total: Some(total),
                description: Some(line.trim().to_string()),
                sub_step_id: None,
            });
        }

        // 8. Test status patterns: "Running test 5 of 20", "Test 8/15 passed"
        // (before generic natural patterns - specific test format)
        if let Some(caps) = get_test_status_pattern().captures(line) {
            let current: u64 = caps.get(1)?.as_str().parse().ok()?;
            let total: u64 = caps.get(2)?.as_str().parse().ok()?;

            if self.is_duplicate(current, Some(total)) {
                return None;
            }

            return Some(ParsedProgress {
                marker_type: "test_progress".to_string(),
                current,
                total: Some(total),
                description: Some(line.trim().to_string()),
                sub_step_id: None,
            });
        }

        // 9. Checkmark test patterns: "✓ 12/20 tests"
        if let Some(caps) = get_checkmark_test_pattern().captures(line) {
            let current: u64 = caps.get(1)?.as_str().parse().ok()?;
            let total: u64 = caps.get(2)?.as_str().parse().ok()?;

            if self.is_duplicate(current, Some(total)) {
                return None;
            }

            return Some(ParsedProgress {
                marker_type: "test_progress".to_string(),
                current,
                total: Some(total),
                description: Some(line.trim().to_string()),
                sub_step_id: None,
            });
        }

        // 10. Natural language: "file 25 of 50" (generic, lower priority)
        if let Some(caps) = get_natural_x_of_y_pattern().captures(line) {
            let item_type = caps.get(1)?.as_str().to_lowercase();
            let current: u64 = caps.get(2)?.as_str().parse().ok()?;
            let total: u64 = caps.get(3)?.as_str().parse().ok()?;

            if self.is_duplicate(current, Some(total)) {
                return None;
            }

            let marker_type = match item_type.as_str() {
                "file" => "file_progress",
                "test" => "test_progress",
                "function" | "class" | "module" | "component" => "analysis_progress",
                _ => "iteration_progress",
            };

            return Some(ParsedProgress {
                marker_type: marker_type.to_string(),
                current,
                total: Some(total),
                description: Some(line.trim().to_string()),
                sub_step_id: None,
            });
        }

        // 11. Natural language: "Completed 10/20" (generic, lower priority)
        if let Some(caps) = get_natural_completed_pattern().captures(line) {
            let action = caps.get(1)?.as_str().to_lowercase();
            let current: u64 = caps.get(2)?.as_str().parse().ok()?;
            let total: u64 = caps.get(3)?.as_str().parse().ok()?;

            if self.is_duplicate(current, Some(total)) {
                return None;
            }

            let marker_type = match action.as_str() {
                "tested" => "test_progress",
                "reviewed" | "analyzed" | "examined" => "review_progress",
                "scanned" | "checked" => "analysis_progress",
                _ => "iteration_progress",
            };

            return Some(ParsedProgress {
                marker_type: marker_type.to_string(),
                current,
                total: Some(total),
                description: Some(line.trim().to_string()),
                sub_step_id: None,
            });
        }

        // 12. Parenthesis progress: "(4/10) description" or "description (4/10)"
        if let Some(caps) = get_paren_progress_pattern().captures(line) {
            // Pattern has two capture group pairs - check which one matched
            let (current, total) = if caps.get(1).is_some() {
                // Start pattern matched: (X/Y) at beginning
                let current: u64 = caps.get(1)?.as_str().parse().ok()?;
                let total: u64 = caps.get(2)?.as_str().parse().ok()?;
                (current, total)
            } else {
                // End pattern matched: (X/Y) at end
                let current: u64 = caps.get(3)?.as_str().parse().ok()?;
                let total: u64 = caps.get(4)?.as_str().parse().ok()?;
                (current, total)
            };

            if self.is_duplicate(current, Some(total)) {
                return None;
            }

            return Some(ParsedProgress {
                marker_type: "iteration_progress".to_string(),
                current,
                total: Some(total),
                description: Some(line.trim().to_string()),
                sub_step_id: None,
            });
        }

        // 13. Percentage pattern (lower priority - might have false positives)
        // Only match if it looks like a progress update (has PROGRESS marker or specific keywords)
        if line.to_lowercase().contains("progress")
            || line.to_lowercase().contains("processing")
            || line.to_lowercase().contains("analyzing")
        {
            if let Some(caps) = get_percentage_pattern().captures(line) {
                let percentage: u64 = caps.get(1)?.as_str().parse().ok()?;
                if percentage <= 100 {
                    // Convert percentage to current/total
                    let current = percentage;
                    let total = 100u64;

                    if self.is_duplicate(current, Some(total)) {
                        return None;
                    }

                    return Some(ParsedProgress {
                        marker_type: "iteration_progress".to_string(),
                        current,
                        total: Some(total),
                        description: Some(line.trim().to_string()),
                        sub_step_id: None,
                    });
                }
            }
        }

        None
    }

    /// Check if this progress is a duplicate of the last one.
    fn is_duplicate(&mut self, current: u64, total: Option<u64>) -> bool {
        let is_dup = self.last_progress == Some((current, total));
        if !is_dup {
            self.last_progress = Some((current, total));
        }
        is_dup
    }

    /// Reset the parser state.
    pub fn reset(&mut self) {
        self.last_progress = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_complete_pattern() {
        let mut parser = ProgressParser::new();

        // Basic STEP_COMPLETE marker
        let progress = parser.parse_line("[STEP_COMPLETE: setup-0]").unwrap();
        assert_eq!(progress.marker_type, "step_complete");
        assert_eq!(progress.sub_step_id, Some("setup-0".to_string()));
        assert_eq!(progress.current, 0);
        assert_eq!(progress.total, None);

        parser.reset();

        // Mixed case
        let progress = parser.parse_line("[step_complete: my-step-id]").unwrap();
        assert_eq!(progress.marker_type, "step_complete");
        assert_eq!(progress.sub_step_id, Some("my-step-id".to_string()));

        parser.reset();

        // Without space after colon
        let progress = parser.parse_line("[STEP_COMPLETE:verify-1]").unwrap();
        assert_eq!(progress.marker_type, "step_complete");
        assert_eq!(progress.sub_step_id, Some("verify-1".to_string()));

        parser.reset();

        // With underscores in ID
        let progress = parser.parse_line("[STEP_COMPLETE: setup_step_0]").unwrap();
        assert_eq!(progress.sub_step_id, Some("setup_step_0".to_string()));
    }

    #[test]
    fn test_explicit_progress() {
        let mut parser = ProgressParser::new();

        let progress = parser.parse_line("[PROGRESS: 50/100]").unwrap();
        assert_eq!(progress.current, 50);
        assert_eq!(progress.total, Some(100));
        assert_eq!(progress.marker_type, "iteration_progress");
        assert_eq!(progress.sub_step_id, None);
    }

    #[test]
    fn test_explicit_typed_progress() {
        let mut parser = ProgressParser::new();

        let progress = parser
            .parse_line("[PROGRESS: file_progress: 25/50]")
            .unwrap();
        assert_eq!(progress.current, 25);
        assert_eq!(progress.total, Some(50));
        assert_eq!(progress.marker_type, "file_progress");
    }

    #[test]
    fn test_natural_x_of_y() {
        let mut parser = ProgressParser::new();

        // "file 10 of 20" without action verb prefix - uses noun-based type
        let progress = parser.parse_line("Looking at file 10 of 20...").unwrap();
        assert_eq!(progress.current, 10);
        assert_eq!(progress.total, Some(20));
        assert_eq!(progress.marker_type, "file_progress");

        parser.reset();

        // "Running test 5 of 15" - matches test_status_pattern (more specific)
        let progress = parser.parse_line("Running test 5 of 15").unwrap();
        assert_eq!(progress.current, 5);
        assert_eq!(progress.total, Some(15));
        assert_eq!(progress.marker_type, "test_progress");
    }

    #[test]
    fn test_completed_pattern() {
        let mut parser = ProgressParser::new();

        let progress = parser.parse_line("Reviewed 8/12 files").unwrap();
        assert_eq!(progress.current, 8);
        assert_eq!(progress.total, Some(12));
        assert_eq!(progress.marker_type, "review_progress");
    }

    #[test]
    fn test_percentage() {
        let mut parser = ProgressParser::new();

        let progress = parser.parse_line("Processing 75%...").unwrap();
        assert_eq!(progress.current, 75);
        assert_eq!(progress.total, Some(100));
    }

    #[test]
    fn test_deduplication() {
        let mut parser = ProgressParser::new();

        // First occurrence should return progress
        let first = parser.parse_line("[PROGRESS: 50/100]");
        assert!(first.is_some());

        // Duplicate should return None
        let second = parser.parse_line("[PROGRESS: 50/100]");
        assert!(second.is_none());

        // Different value should return progress
        let third = parser.parse_line("[PROGRESS: 51/100]");
        assert!(third.is_some());
    }

    #[test]
    fn test_no_false_positives() {
        let mut parser = ProgressParser::new();

        // Regular text shouldn't match
        assert!(parser.parse_line("The ratio is 50/100").is_none());
        assert!(parser.parse_line("Version 2.0").is_none());
        assert!(parser.parse_line("100% cotton").is_none());
    }

    #[test]
    fn test_case_insensitive() {
        let mut parser = ProgressParser::new();

        let progress = parser.parse_line("[progress: 50/100]").unwrap();
        assert_eq!(progress.current, 50);

        parser.reset();

        let progress = parser.parse_line("Analyzing FILE 10 OF 20").unwrap();
        assert_eq!(progress.current, 10);
    }

    // --- New pattern tests ---

    #[test]
    fn test_processing_action_patterns() {
        let mut parser = ProgressParser::new();

        // "Processing file X of Y..."
        let progress = parser.parse_line("Processing file 3 of 10...").unwrap();
        assert_eq!(progress.current, 3);
        assert_eq!(progress.total, Some(10));
        assert_eq!(progress.marker_type, "processing_progress");

        parser.reset();

        // "Analyzing X/Y..."
        let progress = parser.parse_line("Analyzing 5/20...").unwrap();
        assert_eq!(progress.current, 5);
        assert_eq!(progress.total, Some(20));
        assert_eq!(progress.marker_type, "analysis_progress");

        parser.reset();

        // "Completed X of Y"
        let progress = parser.parse_line("Completed 8 of 15").unwrap();
        assert_eq!(progress.current, 8);
        assert_eq!(progress.total, Some(15));
        assert_eq!(progress.marker_type, "iteration_progress");
    }

    #[test]
    fn test_bracket_progress_pattern() {
        let mut parser = ProgressParser::new();

        // "[X/Y] filename"
        let progress = parser.parse_line("[3/10] processing main.rs").unwrap();
        assert_eq!(progress.current, 3);
        assert_eq!(progress.total, Some(10));
        assert_eq!(progress.marker_type, "build_progress");

        parser.reset();

        // "[X/Y] at start with content
        let progress = parser.parse_line("[15/100] Compiling...").unwrap();
        assert_eq!(progress.current, 15);
        assert_eq!(progress.total, Some(100));
    }

    #[test]
    fn test_task_step_patterns() {
        let mut parser = ProgressParser::new();

        // "Task X of Y:"
        let progress = parser
            .parse_line("Task 2 of 5: Update dependencies")
            .unwrap();
        assert_eq!(progress.current, 2);
        assert_eq!(progress.total, Some(5));
        assert_eq!(progress.marker_type, "task_progress");

        parser.reset();

        // "Step X/Y:"
        let progress = parser.parse_line("Step 3/7: Running tests").unwrap();
        assert_eq!(progress.current, 3);
        assert_eq!(progress.total, Some(7));
        assert_eq!(progress.marker_type, "step_progress");

        parser.reset();

        // "Task X of Y -"
        let progress = parser.parse_line("Task 4 of 10 - Build project").unwrap();
        assert_eq!(progress.current, 4);
        assert_eq!(progress.total, Some(10));
    }

    #[test]
    fn test_paren_progress_patterns() {
        let mut parser = ProgressParser::new();

        // "(X/Y) at start"
        let progress = parser.parse_line("(4/10) Running linter").unwrap();
        assert_eq!(progress.current, 4);
        assert_eq!(progress.total, Some(10));
        assert_eq!(progress.marker_type, "iteration_progress");

        parser.reset();

        // "(X/Y) at end"
        let progress = parser.parse_line("Running linter (5/10)").unwrap();
        assert_eq!(progress.current, 5);
        assert_eq!(progress.total, Some(10));
    }

    #[test]
    fn test_iteration_patterns() {
        let mut parser = ProgressParser::new();

        // "Iteration X of Y"
        let progress = parser.parse_line("Iteration 3 of 5").unwrap();
        assert_eq!(progress.current, 3);
        assert_eq!(progress.total, Some(5));
        assert_eq!(progress.marker_type, "iteration_progress");

        parser.reset();

        // "Round X/Y"
        let progress = parser.parse_line("Round 2/4 starting").unwrap();
        assert_eq!(progress.current, 2);
        assert_eq!(progress.total, Some(4));
        assert_eq!(progress.marker_type, "round_progress");

        parser.reset();

        // "Pass X of Y"
        let progress = parser.parse_line("Pass 1 of 3 complete").unwrap();
        assert_eq!(progress.current, 1);
        assert_eq!(progress.total, Some(3));
        assert_eq!(progress.marker_type, "pass_progress");
    }

    #[test]
    fn test_batch_patterns() {
        let mut parser = ProgressParser::new();

        // "Batch X of Y"
        let progress = parser.parse_line("Batch 2 of 5").unwrap();
        assert_eq!(progress.current, 2);
        assert_eq!(progress.total, Some(5));
        assert_eq!(progress.marker_type, "batch_progress");

        parser.reset();

        // "Chunk X/Y"
        let progress = parser.parse_line("Chunk 3/8 uploaded").unwrap();
        assert_eq!(progress.current, 3);
        assert_eq!(progress.total, Some(8));
        assert_eq!(progress.marker_type, "chunk_progress");

        parser.reset();

        // "Processing batch X of Y"
        let progress = parser.parse_line("Processing batch 4 of 10").unwrap();
        assert_eq!(progress.current, 4);
        assert_eq!(progress.total, Some(10));
        assert_eq!(progress.marker_type, "batch_progress");
    }

    #[test]
    fn test_test_status_patterns() {
        let mut parser = ProgressParser::new();

        // "Running test X of Y"
        let progress = parser.parse_line("Running test 5 of 20").unwrap();
        assert_eq!(progress.current, 5);
        assert_eq!(progress.total, Some(20));
        assert_eq!(progress.marker_type, "test_progress");

        parser.reset();

        // "Test X/Y passed"
        let progress = parser.parse_line("Test 8/15 passed").unwrap();
        assert_eq!(progress.current, 8);
        assert_eq!(progress.total, Some(15));
        assert_eq!(progress.marker_type, "test_progress");

        parser.reset();

        // "Test X/Y failed"
        let progress = parser.parse_line("Test 3/10 failed").unwrap();
        assert_eq!(progress.current, 3);
        assert_eq!(progress.total, Some(10));
        assert_eq!(progress.marker_type, "test_progress");
    }

    #[test]
    fn test_checkmark_test_patterns() {
        let mut parser = ProgressParser::new();

        // "✓ X/Y tests"
        let progress = parser.parse_line("✓ 12/20 tests").unwrap();
        assert_eq!(progress.current, 12);
        assert_eq!(progress.total, Some(20));
        assert_eq!(progress.marker_type, "test_progress");

        parser.reset();

        // "✔ X/Y tests"
        let progress = parser.parse_line("✔ 5/10 tests passed").unwrap();
        assert_eq!(progress.current, 5);
        assert_eq!(progress.total, Some(10));

        parser.reset();

        // "[x] X/Y tests" (ASCII variant)
        let progress = parser.parse_line("[x] 8/15 tests").unwrap();
        assert_eq!(progress.current, 8);
        assert_eq!(progress.total, Some(15));
    }

    #[test]
    fn test_no_false_positives_new_patterns() {
        let mut parser = ProgressParser::new();

        // These should NOT match
        assert!(parser.parse_line("The batch size is 32").is_none());
        assert!(parser.parse_line("Processing data...").is_none());
        assert!(parser.parse_line("Task completed successfully").is_none());
        assert!(parser.parse_line("(see documentation)").is_none());
        assert!(parser.parse_line("Round numbers are preferred").is_none());
        assert!(parser.parse_line("[INFO] Starting server").is_none());
        assert!(parser.parse_line("Test the application").is_none());
    }

    #[test]
    fn test_new_patterns_case_insensitive() {
        let mut parser = ProgressParser::new();

        // Mixed case should work
        let progress = parser.parse_line("PROCESSING FILE 3 OF 10").unwrap();
        assert_eq!(progress.current, 3);

        parser.reset();

        let progress = parser.parse_line("iteration 2 OF 5").unwrap();
        assert_eq!(progress.current, 2);

        parser.reset();

        let progress = parser.parse_line("BATCH 1/3").unwrap();
        assert_eq!(progress.current, 1);
    }
}
