//! Unified Command Step Handler
//!
//! Dispatches to the appropriate execution logic based on config fields:
//! - `check_group_id` set -> check group logic
//! - `check_type` set -> check logic
//! - `test_id` or `test_type` set -> test logic
//! - Otherwise -> shell command logic
//!
//! This handler unifies shell_command, check, check_group, and test
//! handlers into a single `command` step type.

use async_trait::async_trait;

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;

// Re-use the existing handler implementations directly
use super::check::CheckHandler;
use super::check_group::CheckGroupHandler;
use super::shell_command::ShellCommandHandler;
use super::test::TestHandler;

/// Unified handler for command steps.
///
/// Dispatches to check_group, check, test, or shell_command logic based on
/// which config fields are populated:
///
/// 1. `check_group_id` is set -> execute as check group
/// 2. `check_type` is set -> execute as check
/// 3. `test_id` or `test_type` is set -> execute as test
/// 4. Otherwise -> execute as shell command
pub struct CommandHandler;

#[async_trait]
impl StepHandler for CommandHandler {
    fn step_type(&self) -> &'static str {
        "command"
    }

    fn display_name(&self) -> &'static str {
        "Command"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // Dispatch based on config fields
        if step.check_group_id.is_some() {
            // Check group mode
            CheckGroupHandler.execute(step, context).await
        } else if step.check_type.is_some() {
            // Check mode
            CheckHandler.execute(step, context).await
        } else if step.test_id.is_some() || step.test_type.is_some() {
            // Test mode
            TestHandler.execute(step, context).await
        } else {
            // Default: shell command mode
            ShellCommandHandler.execute(step, context).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_handler_step_type() {
        let handler = CommandHandler;
        assert_eq!(handler.step_type(), "command");
        assert_eq!(handler.display_name(), "Command");
    }
}
