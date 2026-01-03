"""
Gemini Prompt Executor Module

Handles execution of AI prompts via Gemini CLI or Gemini API.
Supports both OAuth and API key authentication for CLI.
"""

import logging
import os
import subprocess
import time
from collections.abc import Callable
from typing import Any

import requests

logger = logging.getLogger(__name__)


class GeminiExecutor:
    """
    Executes AI prompts via Gemini CLI or Gemini API.

    Responsibilities:
    - Execute prompts via Gemini CLI (gemini command)
    - Execute prompts via Gemini API (direct HTTP calls)
    - Handle timeout and error conditions
    - Support different authentication methods
    """

    # Available Gemini models
    MODELS = [
        "gemini-3-flash",
        "gemini-3-pro",
        "gemini-2.5-flash",
        "gemini-2.5-pro",
        "gemini-2.0-flash",
    ]

    def __init__(
        self,
        emit_log_fn: Callable[[str, str], None] | None = None,
        api_key: str | None = None,
    ):
        """
        Initialize Gemini executor.

        Args:
            emit_log_fn: Function to emit log messages
            api_key: Optional API key for Gemini API (can also be set via env var)
        """
        self.emit_log = emit_log_fn or (
            lambda level, msg: logger.log(getattr(logging, level.upper(), logging.INFO), msg)
        )
        self.api_key = api_key or os.environ.get("GEMINI_API_KEY")

    def execute_prompt_cli(
        self,
        prompt: str,
        model: str = "gemini-3-flash",
        working_directory: str | None = None,
        timeout: int | None = 600000,
    ) -> dict[str, Any]:
        """
        Execute a prompt via Gemini CLI.

        Args:
            prompt: The prompt to send to Gemini
            model: Model to use (e.g., "gemini-3-flash")
            working_directory: Working directory for execution
            timeout: Timeout in milliseconds

        Returns:
            Dictionary with:
                - success: bool
                - output: str (Gemini's response)
                - error: str | None
                - exit_code: int
                - duration_seconds: float
        """
        try:
            self.emit_log("info", f"Executing Gemini CLI prompt: {prompt[:100]}...")

            # Build command with appropriate flags
            # -p: Non-interactive prompt mode
            # -m: Model selection
            # --output-format: Output format (text for easier parsing)
            cmd = [
                "gemini",
                "-m",
                model,
                "--output-format",
                "text",
                "-p",
                prompt,
            ]

            # Convert timeout from ms to seconds
            timeout_seconds = (timeout / 1000.0) if timeout else 600.0

            # Set up subprocess options
            kwargs: dict[str, Any] = {
                "capture_output": True,
                "text": True,
                "timeout": timeout_seconds,
            }

            if working_directory:
                kwargs["cwd"] = working_directory
                self.emit_log("debug", f"Working directory: {working_directory}")

            self.emit_log("debug", f"Using model: {model}")

            # Execute command
            start_time = time.time()
            result = subprocess.run(cmd, **kwargs)
            duration = time.time() - start_time

            self.emit_log(
                "info",
                f"Gemini CLI completed in {duration:.2f}s, exit code: {result.returncode}",
            )

            # Process result
            success = result.returncode == 0
            output = result.stdout.strip() if result.stdout else ""
            error = result.stderr.strip() if result.stderr and not success else None

            if not success:
                self.emit_log("error", f"Gemini CLI failed: {error or 'Unknown error'}")
            else:
                self.emit_log("debug", f"Gemini output length: {len(output)} characters")

            return {
                "success": success,
                "output": output,
                "error": error,
                "exit_code": result.returncode,
                "duration_seconds": duration,
                "provider": "gemini_cli",
                "model": model,
            }

        except subprocess.TimeoutExpired:
            self.emit_log("error", f"Gemini CLI timed out after {timeout_seconds}s")
            return {
                "success": False,
                "output": "",
                "error": f"Execution timed out after {timeout_seconds} seconds",
                "exit_code": -1,
                "duration_seconds": timeout_seconds,
                "provider": "gemini_cli",
                "model": model,
            }

        except FileNotFoundError:
            self.emit_log("error", "Gemini CLI not found. Is Gemini CLI installed?")
            return {
                "success": False,
                "output": "",
                "error": "Gemini CLI executable not found. Install with: npm install -g @google/gemini-cli",
                "exit_code": -1,
                "duration_seconds": 0.0,
                "provider": "gemini_cli",
                "model": model,
            }

        except Exception as e:
            self.emit_log("error", f"Gemini CLI execution failed: {e}")
            return {
                "success": False,
                "output": "",
                "error": str(e),
                "exit_code": -1,
                "duration_seconds": 0.0,
                "provider": "gemini_cli",
                "model": model,
            }

    def execute_prompt_api(
        self,
        prompt: str,
        model: str = "gemini-3-flash",
        max_output_tokens: int = 8192,
        temperature: float = 0.7,
        timeout: int | None = 600000,
        api_key: str | None = None,
    ) -> dict[str, Any]:
        """
        Execute a prompt via Gemini API (direct HTTP).

        Args:
            prompt: The prompt to send to Gemini
            model: Model to use (e.g., "gemini-3-flash")
            max_output_tokens: Maximum output tokens
            temperature: Sampling temperature
            timeout: Timeout in milliseconds
            api_key: Optional API key (uses instance key if not provided)

        Returns:
            Dictionary with:
                - success: bool
                - output: str (Gemini's response)
                - error: str | None
                - duration_seconds: float
        """
        try:
            self.emit_log("info", f"Executing Gemini API prompt: {prompt[:100]}...")

            # Use provided key or instance key
            key = api_key or self.api_key
            if not key:
                return {
                    "success": False,
                    "output": "",
                    "error": "No Gemini API key configured. Set GEMINI_API_KEY or provide key.",
                    "duration_seconds": 0.0,
                    "provider": "gemini_api",
                    "model": model,
                }

            # Build API URL
            url = f"https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={key}"

            # Build request payload
            payload = {
                "contents": [{"parts": [{"text": prompt}]}],
                "generationConfig": {
                    "maxOutputTokens": max_output_tokens,
                    "temperature": temperature,
                },
            }

            # Convert timeout from ms to seconds
            timeout_seconds = (timeout / 1000.0) if timeout else 600.0

            self.emit_log("debug", f"Using model: {model}, max_tokens: {max_output_tokens}")

            # Make API request
            start_time = time.time()
            response = requests.post(
                url,
                json=payload,
                headers={"Content-Type": "application/json"},
                timeout=timeout_seconds,
            )
            duration = time.time() - start_time

            self.emit_log(
                "info",
                f"Gemini API completed in {duration:.2f}s, status: {response.status_code}",
            )

            if response.status_code == 200:
                # Parse successful response
                result = response.json()
                try:
                    # Extract text from response
                    candidates = result.get("candidates", [])
                    if candidates:
                        content = candidates[0].get("content", {})
                        parts = content.get("parts", [])
                        if parts:
                            output = parts[0].get("text", "")
                            self.emit_log(
                                "debug", f"Gemini output length: {len(output)} characters"
                            )
                            return {
                                "success": True,
                                "output": output,
                                "error": None,
                                "duration_seconds": duration,
                                "provider": "gemini_api",
                                "model": model,
                            }

                    # No content in response
                    return {
                        "success": False,
                        "output": "",
                        "error": "No content in Gemini response",
                        "duration_seconds": duration,
                        "provider": "gemini_api",
                        "model": model,
                    }
                except (KeyError, IndexError) as e:
                    self.emit_log("error", f"Failed to parse Gemini response: {e}")
                    return {
                        "success": False,
                        "output": "",
                        "error": f"Failed to parse response: {e}",
                        "duration_seconds": duration,
                        "provider": "gemini_api",
                        "model": model,
                    }
            else:
                # Handle error response
                error_body = response.text
                error_msg = f"API error ({response.status_code})"

                # Parse specific errors
                if response.status_code == 400:
                    if "API_KEY_INVALID" in error_body:
                        error_msg = "Invalid API key. Please check your Gemini API key."
                    else:
                        error_msg = f"Bad request: {error_body[:200]}"
                elif response.status_code == 403:
                    error_msg = "API key doesn't have permission. Check your API key permissions."
                elif response.status_code == 404:
                    error_msg = f"Model '{model}' not found. Check the model name."
                elif response.status_code == 429:
                    error_msg = "Rate limit exceeded. Try again later or upgrade your quota."
                else:
                    error_msg = f"API error ({response.status_code}): {error_body[:200]}"

                self.emit_log("error", error_msg)
                return {
                    "success": False,
                    "output": "",
                    "error": error_msg,
                    "duration_seconds": duration,
                    "provider": "gemini_api",
                    "model": model,
                }

        except requests.Timeout:
            self.emit_log("error", f"Gemini API timed out after {timeout_seconds}s")
            return {
                "success": False,
                "output": "",
                "error": f"Request timed out after {timeout_seconds} seconds",
                "duration_seconds": timeout_seconds,
                "provider": "gemini_api",
                "model": model,
            }

        except requests.RequestException as e:
            self.emit_log("error", f"Gemini API request failed: {e}")
            return {
                "success": False,
                "output": "",
                "error": f"Network error: {e}",
                "duration_seconds": 0.0,
                "provider": "gemini_api",
                "model": model,
            }

        except Exception as e:
            self.emit_log("error", f"Gemini API execution failed: {e}")
            return {
                "success": False,
                "output": "",
                "error": str(e),
                "duration_seconds": 0.0,
                "provider": "gemini_api",
                "model": model,
            }

    def execute_prompt(
        self,
        prompt: str,
        provider: str = "gemini_cli",
        model: str = "gemini-3-flash",
        working_directory: str | None = None,
        timeout: int | None = 600000,
        max_output_tokens: int = 8192,
        temperature: float = 0.7,
        api_key: str | None = None,
    ) -> dict[str, Any]:
        """
        Execute a prompt using the specified provider.

        Args:
            prompt: The prompt to send to Gemini
            provider: Provider to use ("gemini_cli" or "gemini_api")
            model: Model to use
            working_directory: Working directory (CLI only)
            timeout: Timeout in milliseconds
            max_output_tokens: Maximum output tokens (API only)
            temperature: Sampling temperature (API only)
            api_key: API key (API only, uses env var if not provided)

        Returns:
            Dictionary with execution results
        """
        if provider == "gemini_api":
            return self.execute_prompt_api(
                prompt=prompt,
                model=model,
                max_output_tokens=max_output_tokens,
                temperature=temperature,
                timeout=timeout,
                api_key=api_key,
            )
        else:
            # Default to CLI
            return self.execute_prompt_cli(
                prompt=prompt,
                model=model,
                working_directory=working_directory,
                timeout=timeout,
            )
