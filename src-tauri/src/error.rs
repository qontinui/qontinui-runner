use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

#[derive(Error, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum AppError {
    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Python executor error: {0}")]
    ExecutorError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("JSON parsing error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("Process error: {0}")]
    ProcessError(String),

    #[error("Communication error: {0}")]
    CommunicationError(String),

    #[error("State error: {0}")]
    StateError(String),

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("Unexpected error: {0}")]
    UnexpectedError(String),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserFacingError {
    pub title: String,
    pub message: String,
    pub details: Option<String>,
    pub error_code: String,
    pub severity: ErrorSeverity,
    pub recoverable: bool,
    pub suggested_action: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum ErrorSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

impl AppError {
    pub fn to_user_facing(&self) -> UserFacingError {
        match self {
            AppError::ConfigError(msg) => UserFacingError {
                title: "Configuration Error".to_string(),
                message: "There was a problem with your configuration file.".to_string(),
                details: Some(msg.clone()),
                error_code: "CONFIG_001".to_string(),
                severity: ErrorSeverity::Error,
                recoverable: true,
                suggested_action: Some(
                    "Please check your configuration file and try again.".to_string(),
                ),
            },

            AppError::ExecutorError(msg) => UserFacingError {
                title: "Executor Error".to_string(),
                message: "The automation executor encountered a problem.".to_string(),
                details: Some(msg.clone()),
                error_code: "EXEC_001".to_string(),
                severity: ErrorSeverity::Warning,
                recoverable: true,
                suggested_action: Some(
                    "Try restarting the executor or check your Python installation.".to_string(),
                ),
            },

            AppError::IoError(err) => UserFacingError {
                title: "File System Error".to_string(),
                message: "Unable to access required files.".to_string(),
                details: Some(err.to_string()),
                error_code: "IO_001".to_string(),
                severity: ErrorSeverity::Error,
                recoverable: false,
                suggested_action: Some("Check file permissions and disk space.".to_string()),
            },

            AppError::JsonError(err) => UserFacingError {
                title: "Data Format Error".to_string(),
                message: "Unable to parse data format.".to_string(),
                details: Some(err.to_string()),
                error_code: "JSON_001".to_string(),
                severity: ErrorSeverity::Warning,
                recoverable: true,
                suggested_action: Some(
                    "The data format may be corrupted. Try reloading.".to_string(),
                ),
            },

            AppError::ProcessError(msg) => UserFacingError {
                title: "Process Error".to_string(),
                message: "A background process failed.".to_string(),
                details: Some(msg.clone()),
                error_code: "PROC_001".to_string(),
                severity: ErrorSeverity::Error,
                recoverable: true,
                suggested_action: Some(
                    "Restart the application or check system resources.".to_string(),
                ),
            },

            AppError::CommunicationError(msg) => UserFacingError {
                title: "Communication Error".to_string(),
                message: "Unable to communicate with the automation engine.".to_string(),
                details: Some(msg.clone()),
                error_code: "COMM_001".to_string(),
                severity: ErrorSeverity::Warning,
                recoverable: true,
                suggested_action: Some("Check your network connection and try again.".to_string()),
            },

            AppError::StateError(msg) => UserFacingError {
                title: "State Error".to_string(),
                message: "The application is in an invalid state.".to_string(),
                details: Some(msg.clone()),
                error_code: "STATE_001".to_string(),
                severity: ErrorSeverity::Warning,
                recoverable: true,
                suggested_action: Some("Try restarting the current operation.".to_string()),
            },

            AppError::ValidationError(msg) => UserFacingError {
                title: "Validation Error".to_string(),
                message: "The provided input is invalid.".to_string(),
                details: Some(msg.clone()),
                error_code: "VAL_001".to_string(),
                severity: ErrorSeverity::Info,
                recoverable: true,
                suggested_action: Some("Please check your input and try again.".to_string()),
            },

            AppError::UnexpectedError(msg) => UserFacingError {
                title: "Unexpected Error".to_string(),
                message: "An unexpected error occurred.".to_string(),
                details: Some(msg.clone()),
                error_code: "UNK_001".to_string(),
                severity: ErrorSeverity::Critical,
                recoverable: false,
                suggested_action: Some(
                    "Please restart the application. If the problem persists, contact support."
                        .to_string(),
                ),
            },
        }
    }
}

impl fmt::Display for UserFacingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}: {}", self.error_code, self.title, self.message)
    }
}

impl From<AppError> for String {
    fn from(error: AppError) -> Self {
        error.to_string()
    }
}

#[allow(dead_code)]
pub type AppResult<T> = Result<T, AppError>;

#[allow(dead_code)]
pub fn handle_error_with_context<T>(
    result: Result<T, AppError>,
    context: &str,
) -> Result<T, AppError> {
    result.map_err(|e| {
        tracing::error!("Error in {}: {:?}", context, e);
        e
    })
}
