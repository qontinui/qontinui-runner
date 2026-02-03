//! Intra-Step Progress Markers
//!
//! Provides fine-grained progress tracking within individual steps.
//! This is useful for long-running AI operations where you want to track
//! progress (e.g., "analyzed 50/100 files") and enable resume from the
//! last known position.

use serde::{Deserialize, Serialize};

/// A progress marker for tracking progress within a step.
///
/// Progress markers are used to track progress within long-running operations,
/// such as AI analysis sessions that process many files. When a step is resumed,
/// the latest progress marker can be used to continue from where it left off.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepProgressMarker {
    /// Database ID for this marker.
    pub id: i64,
    /// Reference to the parent step checkpoint.
    pub checkpoint_id: String,
    /// Type of progress (e.g., "file_progress", "analysis_progress", "test_progress").
    pub marker_type: String,
    /// Current progress value.
    pub current_value: u64,
    /// Total value (if known). Used for percentage calculation.
    pub total_value: Option<u64>,
    /// Human-readable description of the current progress.
    pub description: Option<String>,
    /// Additional structured data as JSON.
    pub data_json: Option<String>,
    /// When this marker was created.
    pub created_at: String,
}

impl StepProgressMarker {
    /// Get the progress percentage (0.0 - 1.0), if total_value is known.
    pub fn percentage(&self) -> Option<f64> {
        self.total_value.map(|total| {
            if total == 0 {
                0.0
            } else {
                self.current_value as f64 / total as f64
            }
        })
    }

    /// Get a human-readable progress string.
    pub fn progress_string(&self) -> String {
        match self.total_value {
            Some(total) => format!("{}/{}", self.current_value, total),
            None => format!("{}", self.current_value),
        }
    }

    /// Parse the data_json field into a typed structure.
    pub fn parse_data<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
        self.data_json
            .as_ref()
            .and_then(|json| serde_json::from_str(json).ok())
    }
}

/// Common marker types for progress tracking.
pub mod marker_types {
    /// Progress through a collection of files.
    pub const FILE_PROGRESS: &str = "file_progress";
    /// Progress through analysis steps.
    pub const ANALYSIS_PROGRESS: &str = "analysis_progress";
    /// Progress through test execution.
    pub const TEST_PROGRESS: &str = "test_progress";
    /// Progress through code review.
    pub const REVIEW_PROGRESS: &str = "review_progress";
    /// General iteration progress.
    pub const ITERATION_PROGRESS: &str = "iteration_progress";
    /// Completion marker for a sub-step within a consolidated prompt.
    pub const STEP_COMPLETE: &str = "step_complete";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_percentage() {
        let marker = StepProgressMarker {
            id: 1,
            checkpoint_id: "cp-1".to_string(),
            marker_type: marker_types::FILE_PROGRESS.to_string(),
            current_value: 50,
            total_value: Some(100),
            description: Some("Processing files".to_string()),
            data_json: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
        };

        assert_eq!(marker.percentage(), Some(0.5));
        assert_eq!(marker.progress_string(), "50/100");
    }

    #[test]
    fn test_progress_no_total() {
        let marker = StepProgressMarker {
            id: 1,
            checkpoint_id: "cp-1".to_string(),
            marker_type: marker_types::ANALYSIS_PROGRESS.to_string(),
            current_value: 25,
            total_value: None,
            description: None,
            data_json: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
        };

        assert_eq!(marker.percentage(), None);
        assert_eq!(marker.progress_string(), "25");
    }

    #[test]
    fn test_parse_data() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct FileData {
            filename: String,
            line_number: u32,
        }

        let marker = StepProgressMarker {
            id: 1,
            checkpoint_id: "cp-1".to_string(),
            marker_type: marker_types::FILE_PROGRESS.to_string(),
            current_value: 10,
            total_value: Some(50),
            description: None,
            data_json: Some(r#"{"filename":"test.rs","line_number":42}"#.to_string()),
            created_at: "2024-01-01T00:00:00Z".to_string(),
        };

        let data: Option<FileData> = marker.parse_data();
        assert_eq!(
            data,
            Some(FileData {
                filename: "test.rs".to_string(),
                line_number: 42
            })
        );
    }
}
