//! Explicit assertion system for State Explorer
//!
//! This module provides optional explicit assertions that can be defined per state
//! in the workflow configuration. Assertions go beyond element checks to validate
//! specific conditions with custom failure messages.
//!
//! # Usage
//!
//! Assertions can be defined in workflow configs:
//! ```yaml
//! states:
//!   login_page:
//!     assertions:
//!       - type: element_enabled
//!         target: submit_button
//!         critical: true
//!         message: "Submit button should be enabled when form is valid"
//! ```

use serde::{Deserialize, Serialize};

/// Types of assertions that can be checked
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssertionType {
    /// Element should be visible on screen
    ElementVisible { element_id: String },
    /// Element should NOT be visible
    ElementNotVisible { element_id: String },
    /// Element should be enabled (clickable)
    ElementEnabled { element_id: String },
    /// Element should be disabled
    ElementDisabled { element_id: String },
    /// Element text should contain specified string
    TextContains {
        element_id: String,
        expected_text: String,
    },
    /// Element text should exactly match
    TextEquals {
        element_id: String,
        expected_text: String,
    },
    /// Count of elements matching pattern should equal expected
    ElementCount {
        pattern: String,
        expected_count: usize,
    },
    /// Custom assertion with expression (for future AI evaluation)
    Custom { expression: String },
}

impl AssertionType {
    /// Get a human-readable description of the assertion type
    pub fn description(&self) -> String {
        match self {
            Self::ElementVisible { element_id } => {
                format!("Element '{}' should be visible", element_id)
            }
            Self::ElementNotVisible { element_id } => {
                format!("Element '{}' should NOT be visible", element_id)
            }
            Self::ElementEnabled { element_id } => {
                format!("Element '{}' should be enabled", element_id)
            }
            Self::ElementDisabled { element_id } => {
                format!("Element '{}' should be disabled", element_id)
            }
            Self::TextContains {
                element_id,
                expected_text,
            } => format!(
                "Element '{}' should contain text '{}'",
                element_id, expected_text
            ),
            Self::TextEquals {
                element_id,
                expected_text,
            } => format!(
                "Element '{}' text should equal '{}'",
                element_id, expected_text
            ),
            Self::ElementCount {
                pattern,
                expected_count,
            } => format!(
                "Count of elements matching '{}' should be {}",
                pattern, expected_count
            ),
            Self::Custom { expression } => format!("Custom assertion: {}", expression),
        }
    }
}

/// An assertion to check for a specific state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateAssertion {
    /// The type of assertion to perform
    pub assertion_type: AssertionType,
    /// Whether this assertion is critical (failure stops exploration)
    #[serde(default)]
    pub is_critical: bool,
    /// Custom failure message (if not provided, uses default from assertion type)
    #[serde(default)]
    pub failure_message: Option<String>,
    /// Optional timeout for this specific assertion (ms)
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl StateAssertion {
    /// Create a new assertion
    pub fn new(assertion_type: AssertionType) -> Self {
        Self {
            assertion_type,
            is_critical: false,
            failure_message: None,
            timeout_ms: None,
        }
    }

    /// Mark assertion as critical
    pub fn critical(mut self) -> Self {
        self.is_critical = true;
        self
    }

    /// Set custom failure message
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.failure_message = Some(message.into());
        self
    }

    /// Set timeout
    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    /// Get the failure message (custom or default)
    pub fn get_failure_message(&self) -> String {
        self.failure_message
            .clone()
            .unwrap_or_else(|| self.assertion_type.description())
    }
}

/// Result of evaluating an assertion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionResult {
    /// The assertion that was evaluated
    pub assertion: StateAssertion,
    /// Whether the assertion passed
    pub passed: bool,
    /// Actual value found (if applicable)
    pub actual_value: Option<String>,
    /// Error message if failed
    pub error: Option<String>,
    /// Evaluation duration in milliseconds
    pub duration_ms: u64,
}

impl AssertionResult {
    /// Create a passing result
    pub fn pass(assertion: StateAssertion, duration_ms: u64) -> Self {
        Self {
            assertion,
            passed: true,
            actual_value: None,
            error: None,
            duration_ms,
        }
    }

    /// Create a failing result
    pub fn fail(assertion: StateAssertion, error: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            assertion,
            passed: false,
            actual_value: None,
            error: Some(error.into()),
            duration_ms,
        }
    }

    /// Create a failing result with actual value
    pub fn fail_with_actual(
        assertion: StateAssertion,
        actual: impl Into<String>,
        error: impl Into<String>,
        duration_ms: u64,
    ) -> Self {
        Self {
            assertion,
            passed: false,
            actual_value: Some(actual.into()),
            error: Some(error.into()),
            duration_ms,
        }
    }
}

/// Convenience builders for common assertions
impl StateAssertion {
    /// Create assertion that element should be visible
    pub fn element_visible(element_id: impl Into<String>) -> Self {
        Self::new(AssertionType::ElementVisible {
            element_id: element_id.into(),
        })
    }

    /// Create assertion that element should NOT be visible
    pub fn element_not_visible(element_id: impl Into<String>) -> Self {
        Self::new(AssertionType::ElementNotVisible {
            element_id: element_id.into(),
        })
    }

    /// Create assertion that element should be enabled
    pub fn element_enabled(element_id: impl Into<String>) -> Self {
        Self::new(AssertionType::ElementEnabled {
            element_id: element_id.into(),
        })
    }

    /// Create assertion that element should be disabled
    pub fn element_disabled(element_id: impl Into<String>) -> Self {
        Self::new(AssertionType::ElementDisabled {
            element_id: element_id.into(),
        })
    }

    /// Create assertion that element text contains string
    pub fn text_contains(element_id: impl Into<String>, expected: impl Into<String>) -> Self {
        Self::new(AssertionType::TextContains {
            element_id: element_id.into(),
            expected_text: expected.into(),
        })
    }

    /// Create assertion that element text equals string
    pub fn text_equals(element_id: impl Into<String>, expected: impl Into<String>) -> Self {
        Self::new(AssertionType::TextEquals {
            element_id: element_id.into(),
            expected_text: expected.into(),
        })
    }

    /// Create assertion for element count
    pub fn element_count(pattern: impl Into<String>, expected_count: usize) -> Self {
        Self::new(AssertionType::ElementCount {
            pattern: pattern.into(),
            expected_count,
        })
    }

    /// Create custom assertion
    pub fn custom(expression: impl Into<String>) -> Self {
        Self::new(AssertionType::Custom {
            expression: expression.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assertion_creation() {
        let assertion = StateAssertion::element_visible("submit_button")
            .critical()
            .with_message("Submit button must be visible");

        assert!(assertion.is_critical);
        assert_eq!(
            assertion.get_failure_message(),
            "Submit button must be visible"
        );
    }

    #[test]
    fn test_assertion_result() {
        let assertion = StateAssertion::element_enabled("login_btn");
        let result = AssertionResult::pass(assertion.clone(), 100);

        assert!(result.passed);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_assertion_type_description() {
        let assertion_type = AssertionType::TextContains {
            element_id: "header".to_string(),
            expected_text: "Welcome".to_string(),
        };

        assert!(assertion_type.description().contains("should contain text"));
    }

    #[test]
    fn test_serialization() {
        let assertion = StateAssertion::element_visible("test_element").critical();

        let json = serde_json::to_string(&assertion).unwrap();
        let parsed: StateAssertion = serde_json::from_str(&json).unwrap();

        assert!(parsed.is_critical);
        assert!(matches!(
            parsed.assertion_type,
            AssertionType::ElementVisible { .. }
        ));
    }
}
