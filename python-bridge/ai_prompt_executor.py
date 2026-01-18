"""
AI Prompt Executor Module

Handles execution of AI_PROMPT and RUN_PROMPT_SEQUENCE actions.
Uses the shared Claude CLI runner for consistent execution across the runner.
"""

import logging
import time
from collections.abc import Callable
from typing import Any

from services.claude_cli_runner import run_claude_cli

logger = logging.getLogger(__name__)


class AIPromptExecutor:
    """
    Executes AI prompts via Claude Code CLI.

    Responsibilities:
    - Spawn Claude Code CLI as subprocess
    - Support fresh context and continuous context modes
    - Handle timeout and error conditions
    - Execute prompt sequences with context isolation
    """

    def __init__(self, emit_log_fn: Callable[[str, str], None]):
        """
        Initialize AI prompt executor.

        Args:
            emit_log_fn: Function to emit log messages
        """
        self.emit_log = emit_log_fn

    def execute_prompt(
        self,
        prompt: str,
        fresh_context: bool = True,
        working_directory: str | None = None,
        timeout: int | None = 600000,
        output_variable: str | None = None,
        execution_mode: str = "auto",
    ) -> dict[str, Any]:
        """
        Execute a single AI prompt.

        Args:
            prompt: The prompt to send to Claude
            fresh_context: Whether to use fresh context (--print) or continue (-c)
            working_directory: Working directory for execution
            timeout: Timeout in milliseconds
            output_variable: Variable name to store output (for future use)
            execution_mode: How to run Claude (auto, native, wsl)

        Returns:
            Dictionary with:
                - success: bool
                - output: str (Claude's response)
                - error: str | None
                - exit_code: int
                - duration_seconds: float
        """
        self.emit_log("info", f"Executing AI prompt: {prompt[:100]}...")

        context_mode = "fresh context" if fresh_context else "continuous context"
        self.emit_log("debug", f"Using {context_mode} with bypassPermissions")

        if working_directory:
            self.emit_log("debug", f"Working directory: {working_directory}")

        # Convert timeout from ms to seconds
        timeout_seconds = int((timeout / 1000.0) if timeout else 600.0)

        # Use the shared Claude CLI runner
        result = run_claude_cli(
            prompt=prompt,
            timeout_seconds=timeout_seconds,
            execution_mode=execution_mode,
            working_directory=working_directory,
            permission_mode="bypassPermissions",
            fresh_context=fresh_context,
            max_turns=None,  # No limit for AI prompts
            output_format="text",
        )

        self.emit_log(
            "info",
            f"AI prompt completed in {result['duration_seconds']:.2f}s, exit code: {result['exit_code']}",
        )

        if not result["success"]:
            self.emit_log("error", f"AI prompt failed: {result['error'] or 'Unknown error'}")
        else:
            self.emit_log("debug", f"AI output length: {len(result['output'])} characters")

        return result

    def execute_prompt_sequence(
        self,
        sequence_config: dict[str, Any],
        parameter_overrides: dict[str, Any] | None = None,
        working_directory: str | None = None,
        results_directory: str | None = None,
    ) -> dict[str, Any]:
        """
        Execute a sequence of AI prompts with context isolation.

        Each step runs in a fresh context to prevent context overflow.

        Args:
            sequence_config: Sequence configuration (id, name, steps, onFailure, etc.)
            parameter_overrides: Parameter values to apply to all steps
            working_directory: Working directory for execution
            results_directory: Directory to store step results

        Returns:
            Dictionary with:
                - success: bool (true if all steps succeeded or onFailure allows)
                - steps: list of step results
                - failed_step_id: str | None (if a step failed)
                - total_duration_seconds: float
        """
        try:
            sequence_id = sequence_config.get("id", "unnamed-sequence")
            sequence_name = sequence_config.get("name", sequence_id)
            steps = sequence_config.get("steps", [])
            on_failure = sequence_config.get("onFailure", "stop")
            max_retries = sequence_config.get("maxRetries", 0)
            default_timeout = sequence_config.get("defaultTimeout", 600000)

            self.emit_log(
                "info",
                f"Executing prompt sequence: {sequence_name} ({len(steps)} steps)",
            )

            start_time = time.time()
            step_results = []
            overall_success = True
            failed_step_id = None

            for step_idx, step in enumerate(steps):
                step_id = step.get("id", f"step-{step_idx}")
                template_id = step.get("templateId")
                inline_prompt = step.get("inlinePrompt")
                parameter_values = step.get("parameterValues", {})
                step_timeout = step.get("timeout", default_timeout)
                step_working_dir = step.get("workingDirectory", working_directory)
                continue_on_failure = step.get("continueOnFailure", False)
                step_max_retries = step.get("maxRetries", max_retries)

                self.emit_log("info", f"Executing step {step_idx + 1}/{len(steps)}: {step_id}")

                # Determine the prompt to execute
                if inline_prompt:
                    prompt = inline_prompt
                    self.emit_log("debug", f"Using inline prompt for {step_id}")
                elif template_id:
                    # For now, template_id would need to be resolved from a template library
                    # This is a placeholder - in production, you'd look up the template
                    self.emit_log(
                        "warning",
                        f"Template lookup not yet implemented, using template_id as prompt: {template_id}",
                    )
                    prompt = f"Execute template: {template_id}"
                else:
                    self.emit_log("error", f"Step {step_id} has no prompt or templateId")
                    step_results.append(
                        {
                            "step_id": step_id,
                            "success": False,
                            "error": "No prompt or templateId specified",
                        }
                    )
                    if not continue_on_failure and on_failure == "stop":
                        overall_success = False
                        failed_step_id = step_id
                        break
                    continue

                # Execute the prompt with retries
                retry_count = 0
                step_success = False
                step_result: dict[str, Any] = {"success": False}

                while retry_count <= step_max_retries and not step_success:
                    if retry_count > 0:
                        self.emit_log(
                            "info", f"Retry {retry_count}/{step_max_retries} for {step_id}"
                        )

                    step_result = self.execute_prompt(
                        prompt=prompt,
                        fresh_context=True,  # Always fresh for sequence steps
                        working_directory=step_working_dir,
                        timeout=step_timeout,
                    )

                    step_success = step_result["success"]
                    retry_count += 1

                # Add step result
                step_results.append(
                    {
                        "step_id": step_id,
                        "success": step_success,
                        "output": step_result.get("output", ""),
                        "error": step_result.get("error"),
                        "retry_count": retry_count - 1,
                        "duration_seconds": step_result.get("duration_seconds", 0.0),
                    }
                )

                # Handle failure
                if not step_success:
                    self.emit_log("error", f"Step {step_id} failed: {step_result.get('error')}")

                    if continue_on_failure:
                        self.emit_log("info", "Continuing after failure (continueOnFailure=true)")
                        continue

                    if on_failure == "stop":
                        self.emit_log("info", "Stopping sequence (onFailure=stop)")
                        overall_success = False
                        failed_step_id = step_id
                        break
                    elif on_failure == "continue":
                        self.emit_log("info", "Continuing sequence (onFailure=continue)")
                        continue
                    # on_failure="retry" is handled by step-level retry logic above

            total_duration = time.time() - start_time
            self.emit_log(
                "info",
                f"Sequence completed in {total_duration:.2f}s, overall success: {overall_success}",
            )

            return {
                "success": overall_success,
                "steps": step_results,
                "failed_step_id": failed_step_id,
                "total_duration_seconds": total_duration,
            }

        except Exception as e:
            self.emit_log("error", f"Prompt sequence execution failed: {e}")
            return {
                "success": False,
                "steps": [],
                "error": str(e),
                "total_duration_seconds": (
                    time.time() - start_time if "start_time" in locals() else 0.0
                ),
            }
