//! Filtering processor for ignore patterns and severity thresholds.

use crate::error_monitor::pipeline::traits::Processor;
use crate::error_monitor::pipeline::types::LogRecord;
use crate::error_monitor::types::ErrorSeverity;
use async_trait::async_trait;
use regex::Regex;

/// Processor that filters records by ignore patterns and minimum severity.
pub struct FilterProcessor {
    ignore_patterns: Vec<Regex>,
    min_severity: Option<ErrorSeverity>,
}

impl FilterProcessor {
    pub fn new(ignore_patterns: Vec<String>, min_severity: Option<ErrorSeverity>) -> Self {
        let compiled = ignore_patterns
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();

        Self {
            ignore_patterns: compiled,
            min_severity,
        }
    }
}

#[async_trait]
impl Processor for FilterProcessor {
    fn name(&self) -> &str {
        "filter"
    }

    async fn process(&self, records: Vec<LogRecord>) -> Vec<LogRecord> {
        records
            .into_iter()
            .filter(|record| {
                // Check ignore patterns against raw content
                for pattern in &self.ignore_patterns {
                    if pattern.is_match(&record.raw) {
                        return false;
                    }
                }

                // Check minimum severity if set
                if let Some(ref min) = self.min_severity {
                    if let Some(ref severity) = record.severity {
                        return severity >= min;
                    }
                }

                true
            })
            .collect()
    }
}
