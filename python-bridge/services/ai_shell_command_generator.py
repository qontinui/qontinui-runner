"""AI Shell Command Generator Service.

Generates shell commands from natural language descriptions using AI.
Uses the runner's AI settings to connect to Claude CLI, Claude API, or Gemini.
"""

import json
import logging
from typing import Any

from .claude_cli_runner import run_claude_cli

logger = logging.getLogger(__name__)


def build_shell_command_prompt(
    user_prompt: str,
    target_os: str,
    category: str | None = None,
) -> str:
    """Build the full prompt for AI shell command generation.

    Args:
        user_prompt: User's natural language description of the command
        target_os: Target operating system (windows, linux, macos)
        category: Optional command category (git, npm, docker, etc.)

    Returns:
        Complete prompt string for the AI
    """
    lines: list[str] = []

    lines.append("## Task")
    lines.append("Generate a shell command based on the following requirements.")
    lines.append("")

    # OS-specific context
    lines.append("## Target Environment")
    if target_os == "windows":
        lines.append("- **Operating System:** Windows")
        lines.append("- **Shell:** PowerShell")
        lines.append("- Use PowerShell syntax and cmdlets")
        lines.append("- Use semicolons (;) or && to chain commands")
        lines.append("- Use $() for command substitution")
        lines.append("- Use Get-Date for date operations")
    elif target_os == "macos":
        lines.append("- **Operating System:** macOS")
        lines.append("- **Shell:** Zsh")
        lines.append("- Use standard Unix commands with macOS conventions")
        lines.append("- Use && to chain commands")
        lines.append("- Use $(command) for command substitution")
        lines.append("- date command uses BSD flags")
    else:  # linux
        lines.append("- **Operating System:** Linux")
        lines.append("- **Shell:** Bash")
        lines.append("- Use standard Unix commands")
        lines.append("- Use && to chain commands")
        lines.append("- Use $(command) for command substitution")
        lines.append("- date command uses GNU coreutils flags")
    lines.append("")

    if category:
        lines.append("## Command Category")
        if category == "git":
            lines.append("This should be a Git-related command.")
            lines.append("Common git operations: status, add, commit, push, pull, branch, checkout, merge, log")
        elif category == "npm":
            lines.append("This should be an npm/Node.js-related command.")
            lines.append("Common npm operations: install, run, build, test, update, outdated, audit")
        elif category == "docker":
            lines.append("This should be a Docker-related command.")
            lines.append("Common docker operations: build, run, ps, stop, rm, images, prune, compose")
        elif category == "build":
            lines.append("This should be a build/compilation-related command.")
            lines.append("Common build operations: make, cmake, cargo build, go build, npm run build")
        elif category == "poetry":
            lines.append("This should be a Poetry/Python-related command.")
            lines.append("Common poetry operations: install, add, update, run, build, publish")
        lines.append("")

    lines.append("## User Request")
    lines.append(user_prompt)
    lines.append("")

    lines.append("## Output Requirements")
    lines.append("Return a JSON object with exactly two fields:")
    lines.append("1. `command`: The shell command (single line or multi-line with proper escaping)")
    lines.append("2. `description`: A brief description of what the command does")
    lines.append("")
    lines.append("Example output format:")
    lines.append("```json")
    lines.append('{')
    lines.append('  "command": "git status && git add . && git commit -m \\"Update\\"",')
    lines.append('  "description": "Check status, stage all changes, and commit with message"')
    lines.append('}')
    lines.append("```")
    lines.append("")
    lines.append("Return ONLY the JSON object, no explanations or markdown code blocks.")

    return "\n".join(lines)


def generate_command_via_claude_cli(
    prompt: str,
    timeout_seconds: int = 120,
    execution_mode: str = "auto",
    custom_path: str | None = None,
) -> dict[str, Any]:
    """Generate shell command using Claude CLI.

    Args:
        prompt: The full prompt for command generation
        timeout_seconds: Maximum time to wait
        execution_mode: How to run Claude (auto, wsl, native)
        custom_path: Optional custom path to Claude executable

    Returns:
        Dict with success, command, description, and error fields
    """
    import tempfile

    result = {"success": False, "command": "", "description": "", "error": ""}

    # Run from temp directory to avoid picking up project context (CLAUDE.md)
    # This ensures Claude only responds to our prompt, not project instructions
    temp_dir = tempfile.gettempdir()

    cli_result = run_claude_cli(
        prompt=prompt,
        timeout_seconds=timeout_seconds,
        execution_mode=execution_mode,
        custom_path=custom_path,
        working_directory=temp_dir,
    )

    if cli_result["success"]:
        parsed = _parse_command_output(cli_result["output"])
        if parsed["success"]:
            result["success"] = True
            result["command"] = parsed["command"]
            result["description"] = parsed["description"]
        else:
            result["error"] = parsed["error"]
    else:
        result["error"] = cli_result["error"]

    return result


def generate_command_via_claude_api(
    prompt: str,
    api_key: str,
    model: str = "claude-sonnet-4-20250514",
    max_tokens: int = 1024,
) -> dict[str, Any]:
    """Generate shell command using Claude API.

    Args:
        prompt: The full prompt for command generation
        api_key: Anthropic API key
        model: Model name to use
        max_tokens: Maximum tokens in response

    Returns:
        Dict with success, command, description, and error fields
    """
    result = {"success": False, "command": "", "description": "", "error": ""}

    try:
        import httpx

        response = httpx.post(
            "https://api.anthropic.com/v1/messages",
            headers={
                "x-api-key": api_key,
                "anthropic-version": "2023-06-01",
                "content-type": "application/json",
            },
            json={
                "model": model,
                "max_tokens": max_tokens,
                "messages": [{"role": "user", "content": prompt}],
            },
            timeout=120.0,
        )

        if response.status_code == 200:
            data = response.json()
            if data.get("content") and len(data["content"]) > 0:
                output = data["content"][0].get("text", "")
                parsed = _parse_command_output(output)
                if parsed["success"]:
                    result["success"] = True
                    result["command"] = parsed["command"]
                    result["description"] = parsed["description"]
                else:
                    result["error"] = parsed["error"]
            else:
                result["error"] = "Empty response from Claude API"
        else:
            result["error"] = f"Claude API error {response.status_code}: {response.text}"

    except ImportError:
        result["error"] = "httpx not installed. Run: pip install httpx"
    except Exception as e:
        result["error"] = f"Claude API request failed: {e}"

    return result


def _parse_command_output(output: str) -> dict[str, Any]:
    """Parse the AI output to extract command and description.

    Args:
        output: Raw AI output

    Returns:
        Dict with success, command, description, and error fields
    """
    output = output.strip()

    # Remove markdown code blocks if present
    if output.startswith("```"):
        lines = output.split("\n")
        lines = lines[1:]  # Remove first line (```json or similar)
        if lines and lines[-1].strip() == "```":
            lines = lines[:-1]
        output = "\n".join(lines).strip()

    try:
        data = json.loads(output)
        if "command" in data:
            return {
                "success": True,
                "command": data["command"],
                "description": data.get("description", ""),
                "error": "",
            }
        else:
            return {
                "success": False,
                "command": "",
                "description": "",
                "error": "AI response missing 'command' field",
            }
    except json.JSONDecodeError as e:
        # Try to extract command from non-JSON output
        if output and not output.startswith("{"):
            # Assume the entire output is the command
            return {
                "success": True,
                "command": output,
                "description": "AI-generated command",
                "error": "",
            }
        return {
            "success": False,
            "command": "",
            "description": "",
            "error": f"Failed to parse AI response as JSON: {e}",
        }


class AiShellCommandGeneratorService:
    """Service for generating shell commands using AI."""

    def __init__(self, event_manager=None):
        """Initialize the service.

        Args:
            event_manager: Optional event manager for logging
        """
        self.event_manager = event_manager

    def _log(self, level: str, message: str):
        """Log a message."""
        if self.event_manager:
            self.event_manager.emit_log(level, message)
        else:
            logger.log(getattr(logging, level.upper(), logging.INFO), message)

    def generate_command(
        self,
        user_prompt: str,
        target_os: str = "windows",
        category: str | None = None,
        ai_provider: str = "claude_cli",
        ai_settings: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Generate a shell command using AI.

        Args:
            user_prompt: User's description of the command
            target_os: Target operating system (windows, linux, macos)
            category: Optional command category (git, npm, docker, build, poetry, general)
            ai_provider: AI provider to use (claude_cli, claude_api, gemini_cli, gemini_api)
            ai_settings: Provider-specific settings from runner configuration

        Returns:
            Dict with success, command, description, and error fields
        """
        import datetime
        import os
        import sys

        # Debug log file (same as executor)
        debug_log = os.path.join(os.environ.get("USERPROFILE", "C:\\Users\\Joshua"), ".qontinui", "ai-shell-debug.log")

        def debug(msg):
            ts = datetime.datetime.now().isoformat()
            line = f"[{ts}] [SERVICE] {msg}\n"
            with open(debug_log, "a", encoding="utf-8") as f:
                f.write(line)
            print(f"[DEBUG SERVICE] {msg}", file=sys.stderr, flush=True)

        debug("generate_command() CALLED")
        debug(f"ai_provider={ai_provider}, target_os={target_os}, category={category}")

        self._log("info", f"Generating {target_os} shell command using {ai_provider}")

        # Build the prompt
        debug("Building prompt...")
        full_prompt = build_shell_command_prompt(user_prompt, target_os, category)
        debug(f"Prompt built, length={len(full_prompt)}")
        self._log("debug", f"Built prompt with {len(full_prompt)} characters")

        # Generate based on provider
        ai_settings = ai_settings or {}
        debug(f"ai_settings={ai_settings}")

        if ai_provider == "claude_cli":
            execution_mode = ai_settings.get("execution_mode", "auto")
            timeout = ai_settings.get("timeout_seconds", 120)
            custom_path = ai_settings.get("custom_path")
            debug(f"Using claude_cli: execution_mode={execution_mode}, timeout={timeout}, custom_path={custom_path}")
            debug("Calling generate_command_via_claude_cli()...")
            result = generate_command_via_claude_cli(
                full_prompt, timeout, execution_mode, custom_path
            )
            debug(f"generate_command_via_claude_cli() returned: success={result.get('success')}, error={result.get('error')!r}")

        elif ai_provider == "claude_api":
            api_key = ai_settings.get("api_key", "")
            if not api_key:
                debug("ERROR: Claude API key not configured")
                return {
                    "success": False,
                    "command": "",
                    "description": "",
                    "error": "Claude API key not configured",
                }
            model = ai_settings.get("model", "claude-sonnet-4-20250514")
            max_tokens = ai_settings.get("max_tokens", 1024)
            debug(f"Using claude_api: model={model}, max_tokens={max_tokens}")
            result = generate_command_via_claude_api(full_prompt, api_key, model, max_tokens)

        elif ai_provider in ("gemini_cli", "gemini_api"):
            debug(f"ERROR: {ai_provider} not implemented")
            return {
                "success": False,
                "command": "",
                "description": "",
                "error": f"{ai_provider} not yet implemented for shell command generation",
            }

        else:
            debug(f"ERROR: Unknown AI provider: {ai_provider}")
            return {
                "success": False,
                "command": "",
                "description": "",
                "error": f"Unknown AI provider: {ai_provider}",
            }

        if result["success"]:
            self._log("info", f"Successfully generated command: {result['command'][:100]}...")
            debug(f"SUCCESS: command={result['command'][:200]!r}")
        else:
            self._log("error", f"Command generation failed: {result['error']}")
            debug(f"FAILURE: error={result['error']!r}")

        return result
