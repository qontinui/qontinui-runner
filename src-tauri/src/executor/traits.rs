//! Executor Traits Module
//!
//! Defines the common traits for all executors in the system.
//! These traits provide a unified interface for executing various types of
//! operations (setup, verification, agentic, completion, etc.).

use async_trait::async_trait;
use std::fmt::Debug;

use super::context::ExecutorContext;

// =============================================================================
// Executor Error
// =============================================================================

/// Error type for executor operations.
#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    /// A required dependency is missing.
    #[error("Missing dependency: {0}")]
    MissingDependency(&'static str),

    /// Execution failed with an error message.
    #[error("Execution failed: {0}")]
    ExecutionFailed(String),

    /// Configuration error.
    #[error("Configuration error: {0}")]
    ConfigError(String),

    /// Lifecycle error (start/stop operations).
    #[error("Lifecycle error: {0}")]
    LifecycleError(String),

    /// Timeout during execution.
    #[error("Timeout: {0}")]
    Timeout(String),

    /// The executor is not in the right state for this operation.
    #[error("Invalid state: {0}")]
    InvalidState(String),

    /// Wrapped AppError for compatibility.
    #[error("{0}")]
    App(#[from] crate::error::AppError),
}

impl ExecutorError {
    /// Create a missing dependency error.
    pub fn missing(dep: &'static str) -> Self {
        Self::MissingDependency(dep)
    }

    /// Create an execution failed error.
    pub fn failed(msg: impl Into<String>) -> Self {
        Self::ExecutionFailed(msg.into())
    }

    /// Create a config error.
    pub fn config(msg: impl Into<String>) -> Self {
        Self::ConfigError(msg.into())
    }

    /// Create a lifecycle error.
    pub fn lifecycle(msg: impl Into<String>) -> Self {
        Self::LifecycleError(msg.into())
    }

    /// Create a timeout error.
    pub fn timeout(msg: impl Into<String>) -> Self {
        Self::Timeout(msg.into())
    }

    /// Create an invalid state error.
    pub fn invalid_state(msg: impl Into<String>) -> Self {
        Self::InvalidState(msg.into())
    }
}

// =============================================================================
// Executor Trait
// =============================================================================

/// Core trait for all executors.
///
/// An executor is a component that can perform a specific type of operation
/// with a given configuration, producing an output result.
///
/// # Type Parameters
///
/// - `Config`: The configuration type for this executor. Must be `Send + Sync`
///   to support async execution across threads.
/// - `Output`: The result type produced by execution. Must be `Send` for
///   async compatibility.
///
/// # Example
///
/// ```ignore
/// struct MyExecutor { /* ... */ }
///
/// #[async_trait]
/// impl Executor for MyExecutor {
///     type Config = MyConfig;
///     type Output = MyResult;
///
///     async fn execute(&self, config: Self::Config) -> Result<Self::Output, ExecutorError> {
///         // Perform execution
///         Ok(MyResult { success: true })
///     }
///
///     fn name(&self) -> &'static str {
///         "my-executor"
///     }
/// }
/// ```
#[async_trait]
pub trait Executor: Send + Sync {
    /// Configuration type for this executor.
    type Config: Send + Sync;

    /// Output type produced by execution.
    type Output: Send;

    /// Execute with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - The configuration for this execution
    ///
    /// # Returns
    ///
    /// Returns `Ok(Output)` on success, or `Err(ExecutorError)` on failure.
    async fn execute(&self, config: Self::Config) -> Result<Self::Output, ExecutorError>;

    /// Get the name of this executor.
    ///
    /// Used for logging and debugging purposes.
    fn name(&self) -> &'static str;
}

// =============================================================================
// Lifecycle Executor Trait
// =============================================================================

/// Extended trait for executors with lifecycle management.
///
/// Some executors (like ExtractionExecutor) need to be started and stopped
/// independently of individual executions. This trait extends Executor with
/// lifecycle methods.
///
/// # Example
///
/// ```ignore
/// struct ExtractionExecutor { running: bool }
///
/// #[async_trait]
/// impl LifecycleExecutor for ExtractionExecutor {
///     async fn start(&mut self) -> Result<(), ExecutorError> {
///         self.running = true;
///         Ok(())
///     }
///
///     async fn stop(&mut self) -> Result<(), ExecutorError> {
///         self.running = false;
///         Ok(())
///     }
///
///     fn is_running(&self) -> bool {
///         self.running
///     }
/// }
/// ```
#[async_trait]
pub trait LifecycleExecutor: Executor {
    /// Start the executor.
    ///
    /// Called before the first execution. May initialize resources,
    /// start background processes, etc.
    async fn start(&mut self) -> Result<(), ExecutorError>;

    /// Stop the executor.
    ///
    /// Called when the executor is no longer needed. Should clean up
    /// resources and stop any background processes.
    async fn stop(&mut self) -> Result<(), ExecutorError>;

    /// Check if the executor is currently running.
    fn is_running(&self) -> bool;
}

// =============================================================================
// Contextual Executor Trait
// =============================================================================

/// Trait for executors that can be constructed from an ExecutorContext.
///
/// This provides a standard way to create executors with all their dependencies.
pub trait FromContext: Sized {
    /// Create an executor from the given context.
    ///
    /// # Arguments
    ///
    /// * `context` - The executor context containing dependencies
    ///
    /// # Returns
    ///
    /// Returns `Ok(Self)` on success, or `Err(ExecutorError)` if required
    /// dependencies are missing.
    fn from_context(context: ExecutorContext) -> Result<Self, ExecutorError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_executor_error_constructors() {
        let err = ExecutorError::missing("config_storage");
        assert!(matches!(err, ExecutorError::MissingDependency("config_storage")));

        let err = ExecutorError::failed("test failure");
        assert!(matches!(err, ExecutorError::ExecutionFailed(_)));

        let err = ExecutorError::config("bad config");
        assert!(matches!(err, ExecutorError::ConfigError(_)));

        let err = ExecutorError::lifecycle("start failed");
        assert!(matches!(err, ExecutorError::LifecycleError(_)));

        let err = ExecutorError::timeout("30s");
        assert!(matches!(err, ExecutorError::Timeout(_)));

        let err = ExecutorError::invalid_state("not started");
        assert!(matches!(err, ExecutorError::InvalidState(_)));
    }
}
