use serde::{Deserialize, Serialize, Serializer};
use std::fmt;
use thiserror::Error;

// Re-export the canonical ErrorSeverity from error_monitor module
pub use crate::error_monitor::types::ErrorSeverity;

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

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Timeout error: {0}")]
    TimeoutError(String),

    #[error("Health check error: {0}")]
    HealthCheckError(String),

    #[error("Lock poisoned: {0}")]
    LockError(String),

    // =============================================================================
    // New error variants for unified error handling
    // =============================================================================
    #[error("Database error: {0}")]
    DatabaseError(String),

    #[error("Channel error: {0}")]
    ChannelError(String),

    #[error("Pool error: {0}")]
    PoolError(String),

    #[error("Regex error: {0}")]
    RegexError(String),

    #[error("Encoding error: {0}")]
    EncodingError(String),

    #[error("Tauri error: {0}")]
    TauriError(String),

    #[error("Network error: {0}")]
    NetworkError(String),
}

// =============================================================================
// Serialize implementation for Tauri commands
// =============================================================================

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("AppError", 3)?;

        // Serialize variant name
        let variant = match self {
            AppError::ConfigError(_) => "ConfigError",
            AppError::ExecutorError(_) => "ExecutorError",
            AppError::IoError(_) => "IoError",
            AppError::JsonError(_) => "JsonError",
            AppError::ProcessError(_) => "ProcessError",
            AppError::CommunicationError(_) => "CommunicationError",
            AppError::StateError(_) => "StateError",
            AppError::ValidationError(_) => "ValidationError",
            AppError::UnexpectedError(_) => "UnexpectedError",
            AppError::ParseError(_) => "ParseError",
            AppError::TimeoutError(_) => "TimeoutError",
            AppError::HealthCheckError(_) => "HealthCheckError",
            AppError::LockError(_) => "LockError",
            AppError::DatabaseError(_) => "DatabaseError",
            AppError::ChannelError(_) => "ChannelError",
            AppError::PoolError(_) => "PoolError",
            AppError::RegexError(_) => "RegexError",
            AppError::EncodingError(_) => "EncodingError",
            AppError::TauriError(_) => "TauriError",
            AppError::NetworkError(_) => "NetworkError",
        };

        state.serialize_field("variant", variant)?;
        state.serialize_field("message", &self.to_string())?;

        // Serialize error code from UserFacingError
        let user_facing = self.to_user_facing();
        state.serialize_field("error_code", &user_facing.error_code)?;

        state.end()
    }
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

// ErrorSeverity is re-exported from crate::error_monitor::types above

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

            AppError::ParseError(msg) => UserFacingError {
                title: "Parse Error".to_string(),
                message: "Failed to parse data from executor.".to_string(),
                details: Some(msg.clone()),
                error_code: "PARSE_001".to_string(),
                severity: ErrorSeverity::Error,
                recoverable: true,
                suggested_action: Some("Check executor output and logs.".to_string()),
            },

            AppError::TimeoutError(msg) => UserFacingError {
                title: "Timeout Error".to_string(),
                message: "Operation timed out.".to_string(),
                details: Some(msg.clone()),
                error_code: "TIME_001".to_string(),
                severity: ErrorSeverity::Warning,
                recoverable: true,
                suggested_action: Some("Try the operation again.".to_string()),
            },

            AppError::HealthCheckError(msg) => UserFacingError {
                title: "Health Check Error".to_string(),
                message: "Executor health check failed.".to_string(),
                details: Some(msg.clone()),
                error_code: "HEALTH_001".to_string(),
                severity: ErrorSeverity::Error,
                recoverable: true,
                suggested_action: Some("Restart the executor.".to_string()),
            },

            AppError::LockError(msg) => UserFacingError {
                title: "Lock Error".to_string(),
                message: "A thread synchronization lock was poisoned.".to_string(),
                details: Some(msg.clone()),
                error_code: "LOCK_001".to_string(),
                severity: ErrorSeverity::Critical,
                recoverable: false,
                suggested_action: Some(
                    "This indicates a previous thread panic. Restart the application.".to_string(),
                ),
            },

            AppError::DatabaseError(msg) => UserFacingError {
                title: "Database Error".to_string(),
                message: "A database operation failed.".to_string(),
                details: Some(msg.clone()),
                error_code: "DB_001".to_string(),
                severity: ErrorSeverity::Error,
                recoverable: true,
                suggested_action: Some(
                    "Try the operation again. If the problem persists, check database integrity."
                        .to_string(),
                ),
            },

            AppError::ChannelError(msg) => UserFacingError {
                title: "Channel Error".to_string(),
                message: "Failed to communicate between components.".to_string(),
                details: Some(msg.clone()),
                error_code: "CHAN_001".to_string(),
                severity: ErrorSeverity::Error,
                recoverable: true,
                suggested_action: Some(
                    "The operation may have been interrupted. Try again.".to_string(),
                ),
            },

            AppError::PoolError(msg) => UserFacingError {
                title: "Connection Pool Error".to_string(),
                message: "Failed to acquire a connection from the pool.".to_string(),
                details: Some(msg.clone()),
                error_code: "POOL_001".to_string(),
                severity: ErrorSeverity::Error,
                recoverable: true,
                suggested_action: Some(
                    "The system may be under heavy load. Try again shortly.".to_string(),
                ),
            },

            AppError::RegexError(msg) => UserFacingError {
                title: "Pattern Error".to_string(),
                message: "Invalid regular expression pattern.".to_string(),
                details: Some(msg.clone()),
                error_code: "REGEX_001".to_string(),
                severity: ErrorSeverity::Warning,
                recoverable: true,
                suggested_action: Some("Check the pattern syntax and try again.".to_string()),
            },

            AppError::EncodingError(msg) => UserFacingError {
                title: "Encoding Error".to_string(),
                message: "Failed to encode or decode data.".to_string(),
                details: Some(msg.clone()),
                error_code: "ENC_001".to_string(),
                severity: ErrorSeverity::Error,
                recoverable: true,
                suggested_action: Some(
                    "The data may be corrupted. Try reloading from the source.".to_string(),
                ),
            },

            AppError::TauriError(msg) => UserFacingError {
                title: "Application Error".to_string(),
                message: "The application framework encountered an error.".to_string(),
                details: Some(msg.clone()),
                error_code: "TAURI_001".to_string(),
                severity: ErrorSeverity::Error,
                recoverable: true,
                suggested_action: Some(
                    "Restart the application. If the problem persists, reinstall.".to_string(),
                ),
            },

            AppError::NetworkError(msg) => UserFacingError {
                title: "Network Error".to_string(),
                message: "A network request failed.".to_string(),
                details: Some(msg.clone()),
                error_code: "NET_001".to_string(),
                severity: ErrorSeverity::Warning,
                recoverable: true,
                suggested_action: Some("Check your network connection and try again.".to_string()),
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

// =============================================================================
// Helper functions for wrapping errors with context
// =============================================================================

/// Wrap an IO error with additional context about what operation failed.
///
/// # Example
/// ```ignore
/// use crate::error::wrap_io_error;
///
/// std::fs::read_to_string("config.json")
///     .map_err(|e| wrap_io_error("reading configuration file", e))?;
/// ```
pub fn wrap_io_error(context: &str, err: std::io::Error) -> AppError {
    AppError::IoError(std::io::Error::new(
        err.kind(),
        format!("{}: {}", context, err),
    ))
}

/// Wrap a JSON parsing error with additional context about what was being parsed.
///
/// # Example
/// ```ignore
/// use crate::error::wrap_json_error;
///
/// serde_json::from_str::<Config>(json_str)
///     .map_err(|e| wrap_json_error("parsing workflow configuration", e))?;
/// ```
pub fn wrap_json_error(context: &str, err: serde_json::Error) -> AppError {
    AppError::ParseError(format!("{}: {}", context, err))
}

// =============================================================================
// From implementations for unified error conversion
// =============================================================================

// ActionError -> AppError conversion
impl From<crate::action_service::ActionError> for AppError {
    fn from(err: crate::action_service::ActionError) -> Self {
        use crate::action_service::ActionError;
        match err {
            ActionError::ExecutorNotInitialized => {
                AppError::ExecutorError("Executor not initialized".to_string())
            }
            ActionError::ExecutorNotRunning => {
                AppError::ExecutorError("Executor not running".to_string())
            }
            ActionError::ConfigNotLoaded => {
                AppError::ConfigError("Configuration not loaded".to_string())
            }
            ActionError::ConfigNotFound(msg) => {
                AppError::ConfigError(format!("Config not found: {}", msg))
            }
            ActionError::WorkflowNotFound(msg) => {
                AppError::ConfigError(format!("Workflow not found: {}", msg))
            }
            ActionError::StateNotFound(msg) => {
                AppError::StateError(format!("State not found: {}", msg))
            }
            ActionError::ActionFailed(msg) => AppError::ExecutorError(msg),
            ActionError::Timeout(secs) => {
                AppError::TimeoutError(format!("Operation timed out after {} seconds", secs))
            }
            ActionError::Internal(msg) => AppError::UnexpectedError(msg),
            ActionError::StorageError(storage_err) => AppError::from(storage_err),
        }
    }
}

// ConfigStorageError -> AppError conversion
impl From<crate::config_storage::ConfigStorageError> for AppError {
    fn from(err: crate::config_storage::ConfigStorageError) -> Self {
        use crate::config_storage::ConfigStorageError;
        match err {
            ConfigStorageError::IoError(io_err) => AppError::IoError(io_err),
            ConfigStorageError::SerializationError(json_err) => AppError::JsonError(json_err),
            ConfigStorageError::NotFound(id) => {
                AppError::ConfigError(format!("Configuration not found: {}", id))
            }
            ConfigStorageError::InvalidPath(msg) => AppError::ConfigError(msg),
            ConfigStorageError::ParseError(msg) => AppError::ParseError(msg),
        }
    }
}

// EmbeddingError -> AppError conversion
impl From<crate::rag::embeddings::EmbeddingError> for AppError {
    fn from(err: crate::rag::embeddings::EmbeddingError) -> Self {
        use crate::rag::embeddings::EmbeddingError;
        match err {
            EmbeddingError::IoError(io_err) => AppError::IoError(io_err),
            EmbeddingError::ConfigError(msg) => AppError::ConfigError(msg),
        }
    }
}

// FindError -> AppError conversion
impl From<crate::rag::FindError> for AppError {
    fn from(err: crate::rag::FindError) -> Self {
        use crate::rag::FindError;
        match err {
            FindError::IoError(io_err) => AppError::IoError(io_err),
            FindError::ConfigError(msg) => AppError::ConfigError(msg),
            FindError::JsonError(json_err) => AppError::JsonError(json_err),
        }
    }
}

// SearchError -> AppError conversion
impl From<crate::rag::SearchError> for AppError {
    fn from(err: crate::rag::SearchError) -> Self {
        use crate::rag::SearchError;
        match err {
            SearchError::IoError(io_err) => AppError::IoError(io_err),
            SearchError::PythonError(msg) => AppError::ExecutorError(msg),
            SearchError::ConfigError(msg) => AppError::ConfigError(msg),
            SearchError::JsonError(json_err) => AppError::JsonError(json_err),
            SearchError::EmbeddingsNotFound(msg) => {
                AppError::ConfigError(format!("Embeddings not found: {}", msg))
            }
        }
    }
}

// RAGStorageError -> AppError conversion
impl From<crate::rag::storage::RAGStorageError> for AppError {
    fn from(err: crate::rag::storage::RAGStorageError) -> Self {
        use crate::rag::storage::RAGStorageError;
        match err {
            RAGStorageError::IoError(io_err) => AppError::IoError(io_err),
            RAGStorageError::SerializationError(json_err) => AppError::JsonError(json_err),
            RAGStorageError::Base64Error(b64_err) => {
                AppError::ParseError(format!("Base64 decode error: {}", b64_err))
            }
            RAGStorageError::InvalidPath(msg) => AppError::ConfigError(msg),
            RAGStorageError::ProjectNotFound(id) => {
                AppError::ConfigError(format!("RAG project not found: {}", id))
            }
        }
    }
}

// =============================================================================
// From implementations for external error types
// =============================================================================

// tokio::sync::mpsc::error::SendError -> AppError
impl<T> From<tokio::sync::mpsc::error::SendError<T>> for AppError {
    fn from(err: tokio::sync::mpsc::error::SendError<T>) -> Self {
        AppError::ChannelError(format!("Failed to send message: {}", err))
    }
}

// tokio::sync::broadcast::error::RecvError -> AppError
impl From<tokio::sync::broadcast::error::RecvError> for AppError {
    fn from(err: tokio::sync::broadcast::error::RecvError) -> Self {
        AppError::ChannelError(format!("Failed to receive broadcast: {}", err))
    }
}

// tokio::sync::broadcast::error::SendError -> AppError
impl<T> From<tokio::sync::broadcast::error::SendError<T>> for AppError {
    fn from(err: tokio::sync::broadcast::error::SendError<T>) -> Self {
        AppError::ChannelError(format!("Failed to send broadcast: {}", err))
    }
}

// tokio::sync::oneshot::error::RecvError -> AppError
impl From<tokio::sync::oneshot::error::RecvError> for AppError {
    fn from(err: tokio::sync::oneshot::error::RecvError) -> Self {
        AppError::ChannelError(format!("Oneshot channel closed: {}", err))
    }
}

// std::sync::PoisonError -> AppError
impl<T> From<std::sync::PoisonError<T>> for AppError {
    fn from(err: std::sync::PoisonError<T>) -> Self {
        AppError::LockError(format!("Lock poisoned: {}", err))
    }
}

// regex::Error -> AppError
impl From<regex::Error> for AppError {
    fn from(err: regex::Error) -> Self {
        AppError::RegexError(err.to_string())
    }
}

// base64::DecodeError -> AppError
impl From<base64::DecodeError> for AppError {
    fn from(err: base64::DecodeError) -> Self {
        AppError::EncodingError(format!("Base64 decode error: {}", err))
    }
}

// reqwest::Error -> AppError
impl From<reqwest::Error> for AppError {
    fn from(err: reqwest::Error) -> Self {
        AppError::NetworkError(err.to_string())
    }
}

// tauri::Error -> AppError
impl From<tauri::Error> for AppError {
    fn from(err: tauri::Error) -> Self {
        AppError::TauriError(err.to_string())
    }
}
