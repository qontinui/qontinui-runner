//! Shell Step Handler
//!
//! This is an alias for shell_command. Both step types execute shell commands
//! with the same implementation.

use async_trait::async_trait;

use super::shell_command::ShellCommandHandler;
use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for "shell" step type (alias for shell_command).
pub struct ShellHandler;

#[async_trait]
impl StepHandler for ShellHandler {
    fn step_type(&self) -> &'static str {
        "shell"
    }

    fn display_name(&self) -> &'static str {
        "Shell"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // Delegate to ShellCommandHandler since they're functionally identical
        ShellCommandHandler.execute(step, context).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_handler_step_type() {
        let handler = ShellHandler;
        assert_eq!(handler.step_type(), "shell");
        assert_eq!(handler.display_name(), "Shell");
    }
}
