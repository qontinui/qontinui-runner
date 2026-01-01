//! Flakiness-aware execution options and caching.
//!
//! This module provides tools for adjusting execution behavior based on
//! historical flakiness data from the Tiered Information Model.

use crate::tiered_info::{get_flaky_templates, get_flaky_transitions, FlakyItem};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;

/// Execution options that can be adjusted based on flakiness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionOptions {
    /// Number of retry attempts for this operation.
    pub retry_count: u32,
    /// Multiplier for timeout duration.
    pub timeout_multiplier: f64,
    /// Confidence threshold for template matching.
    pub confidence_threshold: f64,
    /// Whether to use an alternative path if available.
    pub use_alternative_path: bool,
}

impl Default for ExecutionOptions {
    fn default() -> Self {
        Self {
            retry_count: 1,
            timeout_multiplier: 1.0,
            confidence_threshold: 0.8,
            use_alternative_path: false,
        }
    }
}

/// Severity level for flaky items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Success rate >= 70% but < 80%
    Low,
    /// Success rate >= 50% but < 70%
    Medium,
    /// Success rate < 50%
    High,
}

impl Severity {
    /// Calculate severity from success rate.
    pub fn from_success_rate(success_rate: f64) -> Self {
        if success_rate < 0.5 {
            Severity::High
        } else if success_rate < 0.7 {
            Severity::Medium
        } else {
            Severity::Low
        }
    }
}

/// Cache of flaky items for a configuration.
///
/// This cache is loaded once and can be reused for multiple operations
/// within a workflow execution. The cache has a 5-minute TTL.
pub struct FlakinessCache {
    config_id: String,
    flaky_transitions: HashMap<String, FlakyItem>,
    flaky_templates: HashMap<String, FlakyItem>,
    loaded_at: Instant,
}

impl FlakinessCache {
    /// Load flakiness data for a configuration.
    ///
    /// # Arguments
    /// * `conn` - Database connection
    /// * `config_id` - Configuration ID to load data for
    /// * `threshold` - Flakiness threshold (items with success rate between
    ///   threshold and (1-threshold) are considered flaky)
    ///
    /// # Returns
    /// A new `FlakinessCache` with the loaded data.
    pub fn load(conn: &Connection, config_id: &str, threshold: f64) -> Result<Self, String> {
        let transitions = get_flaky_transitions(conn, config_id, threshold)?;
        let templates = get_flaky_templates(conn, config_id, threshold)?;

        Ok(Self {
            config_id: config_id.to_string(),
            flaky_transitions: transitions
                .into_iter()
                .map(|f| (f.name.clone(), f))
                .collect(),
            flaky_templates: templates.into_iter().map(|f| (f.name.clone(), f)).collect(),
            loaded_at: Instant::now(),
        })
    }

    /// Check if the cache is stale (older than 5 minutes).
    pub fn is_stale(&self) -> bool {
        self.loaded_at.elapsed() > std::time::Duration::from_secs(300)
    }

    /// Get the configuration ID this cache was loaded for.
    pub fn config_id(&self) -> &str {
        &self.config_id
    }

    /// Get adjusted execution options for a transition.
    ///
    /// The transition ID should be in the format "FromState|ToState".
    ///
    /// # Arguments
    /// * `transition_id` - Transition identifier
    ///
    /// # Returns
    /// Execution options adjusted based on the transition's flakiness.
    pub fn get_transition_options(&self, transition_id: &str) -> ExecutionOptions {
        match self.flaky_transitions.get(transition_id) {
            Some(flaky) => {
                let severity = Severity::from_success_rate(flaky.success_rate);
                ExecutionOptions {
                    retry_count: match severity {
                        Severity::Low => 2,
                        Severity::Medium => 3,
                        Severity::High => 4,
                    },
                    timeout_multiplier: match severity {
                        Severity::Low => 1.2,
                        Severity::Medium => 1.5,
                        Severity::High => 2.0,
                    },
                    confidence_threshold: 0.7, // Lower threshold for flaky items
                    use_alternative_path: severity == Severity::High,
                }
            }
            None => ExecutionOptions::default(),
        }
    }

    /// Get adjusted options for a template match.
    ///
    /// # Arguments
    /// * `template_id` - Template/image identifier
    ///
    /// # Returns
    /// Execution options adjusted based on the template's flakiness.
    pub fn get_template_options(&self, template_id: &str) -> ExecutionOptions {
        match self.flaky_templates.get(template_id) {
            Some(flaky) => ExecutionOptions {
                retry_count: 3,
                timeout_multiplier: 1.5,
                // Accept slightly lower confidence for known flaky templates
                confidence_threshold: flaky.success_rate * 0.9,
                use_alternative_path: false,
            },
            None => ExecutionOptions::default(),
        }
    }

    /// Check if a transition is known to be flaky.
    pub fn is_transition_flaky(&self, transition_id: &str) -> bool {
        self.flaky_transitions.contains_key(transition_id)
    }

    /// Check if a template is known to be flaky.
    pub fn is_template_flaky(&self, template_id: &str) -> bool {
        self.flaky_templates.contains_key(template_id)
    }

    /// Get all flaky transition IDs.
    pub fn get_flaky_transition_ids(&self) -> Vec<&str> {
        self.flaky_transitions.keys().map(|s| s.as_str()).collect()
    }

    /// Get all flaky template IDs.
    pub fn get_flaky_template_ids(&self) -> Vec<&str> {
        self.flaky_templates.keys().map(|s| s.as_str()).collect()
    }

    /// Get details for a flaky transition.
    pub fn get_transition_details(&self, transition_id: &str) -> Option<&FlakyItem> {
        self.flaky_transitions.get(transition_id)
    }

    /// Get details for a flaky template.
    pub fn get_template_details(&self, template_id: &str) -> Option<&FlakyItem> {
        self.flaky_templates.get(template_id)
    }

    /// Get count of flaky transitions.
    pub fn flaky_transition_count(&self) -> usize {
        self.flaky_transitions.len()
    }

    /// Get count of flaky templates.
    pub fn flaky_template_count(&self) -> usize {
        self.flaky_templates.len()
    }
}

/// Helper to execute an async operation with flakiness-aware retry.
///
/// This function applies exponential backoff between retries.
///
/// # Arguments
/// * `options` - Execution options controlling retry behavior
/// * `operation` - The async operation to execute
///
/// # Returns
/// The result of the operation, or an error after all retries are exhausted.
pub async fn execute_with_retry<T, F, Fut>(
    options: &ExecutionOptions,
    mut operation: F,
) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let mut last_error = String::new();

    for attempt in 0..options.retry_count {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_error = e;
                if attempt < options.retry_count - 1 {
                    // Wait before retry with exponential backoff
                    let delay = std::time::Duration::from_millis(100 * (2_u64.pow(attempt)));
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    Err(format!(
        "Failed after {} retries: {}",
        options.retry_count, last_error
    ))
}

/// Synchronous version of execute_with_retry for non-async contexts.
pub fn execute_with_retry_sync<T, F>(
    options: &ExecutionOptions,
    mut operation: F,
) -> Result<T, String>
where
    F: FnMut() -> Result<T, String>,
{
    let mut last_error = String::new();

    for attempt in 0..options.retry_count {
        match operation() {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_error = e;
                if attempt < options.retry_count - 1 {
                    // Wait before retry with exponential backoff
                    let delay = std::time::Duration::from_millis(100 * (2_u64.pow(attempt)));
                    std::thread::sleep(delay);
                }
            }
        }
    }

    Err(format!(
        "Failed after {} retries: {}",
        options.retry_count, last_error
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_severity_from_success_rate() {
        assert_eq!(Severity::from_success_rate(0.9), Severity::Low);
        assert_eq!(Severity::from_success_rate(0.75), Severity::Low);
        assert_eq!(Severity::from_success_rate(0.7), Severity::Low);
        assert_eq!(Severity::from_success_rate(0.69), Severity::Medium);
        assert_eq!(Severity::from_success_rate(0.5), Severity::Medium);
        assert_eq!(Severity::from_success_rate(0.49), Severity::High);
        assert_eq!(Severity::from_success_rate(0.1), Severity::High);
    }

    #[test]
    fn test_default_execution_options() {
        let options = ExecutionOptions::default();
        assert_eq!(options.retry_count, 1);
        assert!((options.timeout_multiplier - 1.0).abs() < f64::EPSILON);
        assert!((options.confidence_threshold - 0.8).abs() < f64::EPSILON);
        assert!(!options.use_alternative_path);
    }

    #[test]
    fn test_execute_with_retry_sync_success() {
        let options = ExecutionOptions {
            retry_count: 3,
            ..Default::default()
        };

        let mut attempt = 0;
        let result = execute_with_retry_sync(&options, || {
            attempt += 1;
            if attempt < 2 {
                Err("temporary error".to_string())
            } else {
                Ok("success")
            }
        });

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "success");
        assert_eq!(attempt, 2);
    }

    #[test]
    fn test_execute_with_retry_sync_exhausted() {
        let options = ExecutionOptions {
            retry_count: 2,
            ..Default::default()
        };

        let mut attempt = 0;
        let result: Result<(), String> = execute_with_retry_sync(&options, || {
            attempt += 1;
            Err("persistent error".to_string())
        });

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Failed after 2 retries"));
        assert_eq!(attempt, 2);
    }
}
