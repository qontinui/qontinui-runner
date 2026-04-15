//! Unified Command Step Handler
//!
//! Dispatches to the appropriate execution logic based on the `mode` field:
//! - `mode: "check_group"` -> check group logic
//! - `mode: "check"` -> check logic
//! - `mode: "test"` -> test logic
//! - `mode: "shell"` -> shell command logic
//!
//! The `mode` field is required. Workflows without it should be fixed by
//! `fix_workflow()` before execution.

use async_trait::async_trait;

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::ExecutionStepConfig;
use crate::step_types::{CommandMode, CommandModeExt};

// Re-use the existing handler implementations directly
use super::check::CheckHandler;
use super::check_group::CheckGroupHandler;
use super::shell_command::ShellCommandHandler;
use super::test::TestHandler;

/// Unified handler for command steps.
///
/// Dispatches to check_group, check, test, or shell_command logic
/// based on the required `mode` field.
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
        let mode = step
            .command_mode
            .as_deref()
            .and_then(<CommandMode as CommandModeExt>::from_str_opt);

        match mode {
            Some(CommandMode::CheckGroup) => CheckGroupHandler.execute(step, context).await,
            Some(CommandMode::Check) => CheckHandler.execute(step, context).await,
            Some(CommandMode::Test) => TestHandler.execute(step, context).await,
            Some(CommandMode::Shell) => ShellCommandHandler.execute(step, context).await,
            None => {
                let raw = step.command_mode.as_deref().unwrap_or("<missing>");
                let name = step.name.as_deref().unwrap_or("unnamed");
                StepHandlerResult::failure(format!(
                    "Command step '{}' has invalid or missing mode: '{}'. \
                     Valid modes: shell, check, check_group, test",
                    name, raw
                ))
            }
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
