"""Claude CLI Runner Utility.

Shared utility for running Claude CLI commands consistently across all services.
Respects the runner's AI settings for execution mode.

This is the single source of truth for Claude CLI invocation in the runner.
All services should use these functions instead of calling subprocess directly.
"""

import contextlib
import os
import platform
import subprocess
import tempfile
from typing import Any


def _debug_log(msg: str) -> None:
    """Write debug message to log file and stderr."""
    import datetime
    import sys

    debug_log = os.path.join(
        os.environ.get("USERPROFILE", "C:\\Users\\Joshua"), ".qontinui", "ai-shell-debug.log"
    )
    ts = datetime.datetime.now().isoformat()
    line = f"[{ts}] [CLI_RUNNER] {msg}\n"
    try:
        with open(debug_log, "a", encoding="utf-8") as f:
            f.write(line)
    except Exception:
        pass
    print(f"[DEBUG CLI_RUNNER] {msg}", file=sys.stderr, flush=True)


def _sanitize_text_for_utf8(text: str) -> str:
    """Remove invalid UTF-8 surrogate characters from text.

    Some files may contain invalid Unicode surrogates (e.g., from incorrect
    encoding detection). This function removes them to ensure valid UTF-8.

    Surrogates are in the range U+D800 to U+DFFF and are invalid in UTF-8.
    """
    # Filter out surrogate characters directly - this is the most reliable method
    # Surrogates are code points in range 0xD800-0xDFFF
    return "".join(c for c in text if not (0xD800 <= ord(c) <= 0xDFFF))


def run_claude_cli(
    prompt: str,
    timeout_seconds: int = 120,
    execution_mode: str = "auto",
    custom_path: str | None = None,
    working_directory: str | None = None,
    permission_mode: str | None = None,
    fresh_context: bool = True,
    max_turns: int | None = 1,
    output_format: str = "text",
) -> dict[str, Any]:
    """Run Claude CLI with a prompt and return the output.

    This is the central function for all Claude CLI invocations.
    It respects the runner's AI settings for execution mode.

    Args:
        prompt: The prompt to send to Claude
        timeout_seconds: Maximum time to wait for response
        execution_mode: How to run Claude:
            - "auto" or "native": Use native Windows/macOS/Linux Claude CLI
            - "wsl": Use WSL on Windows to run Claude CLI
        custom_path: Optional custom path to Claude executable
        working_directory: Optional working directory for Claude
        permission_mode: Optional permission mode (e.g., "bypassPermissions")
        fresh_context: If True, use -p/--print for fresh context; if False, use -c for continue
        max_turns: Maximum turns (None for unlimited, 1 for single response)
        output_format: Output format ("text" or "json")

    Returns:
        Dict with:
            - success: bool
            - output: str - Raw output from Claude
            - error: str - Error message if failed
            - exit_code: int - Process exit code
            - duration_seconds: float - Execution duration
    """
    import time

    _debug_log("run_claude_cli() CALLED")
    _debug_log(
        f"prompt_len={len(prompt)}, timeout={timeout_seconds}, execution_mode={execution_mode}"
    )
    _debug_log(f"custom_path={custom_path}, working_directory={working_directory}")
    _debug_log(
        f"permission_mode={permission_mode}, fresh_context={fresh_context}, max_turns={max_turns}"
    )

    result = {"success": False, "output": "", "error": "", "exit_code": -1, "duration_seconds": 0.0}

    system = platform.system()
    _debug_log(f"system={system}")

    # Only use WSL if explicitly requested
    # "auto" and "native" both use native execution
    use_wsl = execution_mode == "wsl"
    _debug_log(f"use_wsl={use_wsl}")

    start_time = time.time()

    try:
        if use_wsl:
            _debug_log("Calling _run_via_wsl()...")
            result = _run_via_wsl(
                prompt=prompt,
                timeout_seconds=timeout_seconds,
                custom_path=custom_path,
                working_directory=working_directory,
                permission_mode=permission_mode,
                fresh_context=fresh_context,
                max_turns=max_turns,
                output_format=output_format,
            )
        else:
            _debug_log("Calling _run_native()...")
            result = _run_native(
                prompt=prompt,
                timeout_seconds=timeout_seconds,
                custom_path=custom_path,
                system=system,
                working_directory=working_directory,
                permission_mode=permission_mode,
                fresh_context=fresh_context,
                max_turns=max_turns,
                output_format=output_format,
            )
        _debug_log(
            f"Function returned: success={result.get('success')}, error={result.get('error')!r}"
        )

    except subprocess.TimeoutExpired:
        _debug_log("EXCEPTION: subprocess.TimeoutExpired")
        result["error"] = f"Claude CLI timed out after {timeout_seconds} seconds"
        result["duration_seconds"] = timeout_seconds
    except FileNotFoundError as e:
        _debug_log(f"EXCEPTION: FileNotFoundError: {e}")
        result["error"] = (
            f"Claude CLI not found: {e}. Please install Claude Code or check AI settings."
        )
    except Exception as e:
        _debug_log(f"EXCEPTION: {type(e).__name__}: {e}")
        result["error"] = f"Failed to invoke Claude CLI: {e}"

    if result["duration_seconds"] == 0.0:
        result["duration_seconds"] = time.time() - start_time

    _debug_log(
        f"run_claude_cli() RETURNING: success={result.get('success')}, duration={result.get('duration_seconds'):.2f}s"
    )
    return result


def _build_claude_args(
    prompt: str,
    permission_mode: str | None,
    fresh_context: bool,
    max_turns: int | None,
    output_format: str,
) -> list[str]:
    """Build Claude CLI arguments."""
    args = []

    # Output format
    args.extend(["--output-format", output_format])

    # Permission mode (for autonomous execution)
    if permission_mode:
        args.extend(["--permission-mode", permission_mode])

    # Max turns
    if max_turns is not None:
        args.extend(["--max-turns", str(max_turns)])

    # Context mode: -p/--print for fresh, -c for continue
    if fresh_context:
        args.extend(["-p", prompt])
    else:
        args.extend(["-c", prompt])

    return args


def _run_native(
    prompt: str,
    timeout_seconds: int,
    custom_path: str | None,
    system: str,
    working_directory: str | None = None,
    permission_mode: str | None = None,
    fresh_context: bool = True,
    max_turns: int | None = 1,
    output_format: str = "text",
) -> dict[str, Any]:
    """Run Claude CLI natively on Windows/macOS/Linux."""
    import time

    _debug_log("_run_native() CALLED")
    _debug_log(f"system={system}, custom_path={custom_path}")

    result = {"success": False, "output": "", "error": "", "exit_code": -1, "duration_seconds": 0.0}

    # Determine the Claude command
    # Use 'claude' on all platforms - let the OS resolve the extension
    # On Windows, this finds claude.exe via PATH/PATHEXT
    # On Unix, this finds the claude binary
    claude_cmd = custom_path or "claude"

    _debug_log(f"claude_cmd={claude_cmd}")

    # On Windows, use stdin piping instead of command line arguments
    # This avoids shell escaping issues with special characters in the prompt
    if system == "Windows":
        _debug_log("Windows detected, calling _run_native_windows_stdin()...")
        return _run_native_windows_stdin(
            prompt=prompt,
            claude_cmd=claude_cmd,
            timeout_seconds=timeout_seconds,
            working_directory=working_directory,
            permission_mode=permission_mode,
            fresh_context=fresh_context,
            max_turns=max_turns,
            output_format=output_format,
        )

    # For non-Windows, use command line arguments
    cmd = [claude_cmd] + _build_claude_args(
        prompt, permission_mode, fresh_context, max_turns, output_format
    )

    # Subprocess options
    kwargs: dict[str, Any] = {
        "capture_output": True,
        "text": True,
        "timeout": timeout_seconds,
    }

    if working_directory:
        kwargs["cwd"] = working_directory

    start_time = time.time()
    proc = subprocess.run(cmd, **kwargs)
    result["duration_seconds"] = time.time() - start_time
    result["exit_code"] = proc.returncode

    if proc.returncode == 0:
        result["success"] = True
        result["output"] = proc.stdout.strip()
    else:
        result["error"] = proc.stderr or f"Claude CLI exited with code {proc.returncode}"

    return result


def _run_native_windows_stdin(
    prompt: str,
    claude_cmd: str,
    timeout_seconds: int,
    working_directory: str | None = None,
    permission_mode: str | None = None,
    fresh_context: bool = True,
    max_turns: int | None = 1,
    output_format: str = "text",
) -> dict[str, Any]:
    """Run Claude CLI on Windows using a temp file for the prompt.

    This avoids shell escaping issues with special characters in the prompt.
    Uses PowerShell to read the prompt file and pipe it to Claude CLI.
    """
    import os
    import tempfile
    import time
    import uuid

    _debug_log("_run_native_windows_stdin() CALLED")
    _debug_log(f"claude_cmd={claude_cmd}, timeout={timeout_seconds}, cwd={working_directory}")
    _debug_log(
        f"prompt_len={len(prompt)}, permission_mode={permission_mode}, fresh_context={fresh_context}"
    )

    result = {"success": False, "output": "", "error": "", "exit_code": -1, "duration_seconds": 0.0}

    # Write prompt to a temp file
    temp_dir = tempfile.gettempdir()
    prompt_file = os.path.join(temp_dir, f"claude-prompt-{uuid.uuid4()}.txt")
    _debug_log(f"Writing prompt to: {prompt_file}")

    try:
        # Sanitize prompt to remove invalid UTF-8 surrogates (can occur from
        # reading files with incorrect encoding, e.g., component source)
        sanitized_prompt = _sanitize_text_for_utf8(prompt)
        with open(prompt_file, "w", encoding="utf-8") as f:
            f.write(sanitized_prompt)
        _debug_log(f"Prompt written successfully, file size={os.path.getsize(prompt_file)} bytes")
    except Exception as e:
        _debug_log(f"ERROR writing prompt file: {e}")
        result["error"] = f"Failed to write prompt to temp file: {e}"
        return result

    # Build Claude CLI arguments
    args_list = ["--output-format", output_format]

    if permission_mode:
        args_list.extend(["--permission-mode", permission_mode])

    if max_turns is not None:
        args_list.extend(["--max-turns", str(max_turns)])

    # Use --print for fresh context
    if fresh_context:
        args_list.append("--print")
    else:
        args_list.append("-c")

    args_str = " ".join(args_list)
    _debug_log(f"args_str={args_str}")

    # Build PowerShell command that reads the file and pipes to Claude
    # Use -Raw to read the entire file, -Encoding UTF8 for proper encoding
    ps_command = f"Get-Content -Path '{prompt_file}' -Raw -Encoding UTF8 | {claude_cmd} {args_str}"
    _debug_log(f"ps_command={ps_command}")

    cmd = ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", ps_command]
    _debug_log(f"Full command: {cmd}")

    start_time = time.time()
    _debug_log("Starting subprocess.run()...")

    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout_seconds,
            cwd=working_directory,
        )

        elapsed = time.time() - start_time
        _debug_log(f"subprocess.run() completed in {elapsed:.2f}s, returncode={proc.returncode}")
        _debug_log(f"stdout length={len(proc.stdout)}, stderr length={len(proc.stderr)}")
        if proc.stderr:
            _debug_log(f"stderr (first 500 chars): {proc.stderr[:500]!r}")

        result["duration_seconds"] = elapsed
        result["exit_code"] = proc.returncode

        if proc.returncode == 0:
            result["success"] = True
            output = proc.stdout.strip()
            result["output"] = output
            _debug_log(f"SUCCESS: output (first 200 chars): {output[:200]!r}")
        else:
            result["error"] = proc.stderr or f"Claude CLI exited with code {proc.returncode}"
            _debug_log(f"FAILURE: returncode={proc.returncode}, error={result['error']!r}")

    except subprocess.TimeoutExpired:
        _debug_log(f"TIMEOUT: subprocess.TimeoutExpired after {timeout_seconds}s")
        result["error"] = f"Claude CLI timed out after {timeout_seconds} seconds"
        result["duration_seconds"] = timeout_seconds
    except Exception as e:
        _debug_log(f"EXCEPTION: {type(e).__name__}: {e}")
        result["error"] = f"Failed to run Claude CLI: {e}"
        result["duration_seconds"] = time.time() - start_time

    finally:
        # Clean up temp file
        _debug_log(f"Cleaning up prompt file: {prompt_file}")
        try:
            os.remove(prompt_file)
            _debug_log("Prompt file deleted successfully")
        except Exception as e:
            _debug_log(f"Failed to delete prompt file: {e}")

    _debug_log(f"_run_native_windows_stdin() RETURNING: success={result['success']}")
    return result


def _convert_to_wsl_path(windows_path: str) -> str:
    """Convert a Windows path to a WSL path."""
    wsl_path = windows_path.replace("\\", "/")
    if len(wsl_path) > 1 and wsl_path[1] == ":":
        drive = wsl_path[0].lower()
        wsl_path = f"/mnt/{drive}/{wsl_path[3:]}"
    return wsl_path


def _run_via_wsl(
    prompt: str,
    timeout_seconds: int,
    custom_path: str | None,
    working_directory: str | None = None,
    permission_mode: str | None = None,
    fresh_context: bool = True,
    max_turns: int | None = 1,
    output_format: str = "text",
) -> dict[str, Any]:
    """Run Claude CLI via WSL on Windows."""
    import time

    result = {"success": False, "output": "", "error": "", "exit_code": -1, "duration_seconds": 0.0}
    prompt_file = None

    try:
        # Write prompt to temp file for WSL (handles escaping issues)
        # Sanitize prompt to remove invalid UTF-8 surrogates
        sanitized_prompt = _sanitize_text_for_utf8(prompt)
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".txt", delete=False, encoding="utf-8"
        ) as f:
            f.write(sanitized_prompt)
            prompt_file = f.name

        # Convert paths to WSL format
        wsl_prompt = _convert_to_wsl_path(prompt_file)
        wsl_cwd = _convert_to_wsl_path(working_directory) if working_directory else None

        # Build Claude command arguments
        claude_cmd = custom_path or "claude"
        args_parts = [f"--output-format {output_format}"]

        if permission_mode:
            args_parts.append(f"--permission-mode {permission_mode}")

        if max_turns is not None:
            args_parts.append(f"--max-turns {max_turns}")

        # Context mode
        if fresh_context:
            args_parts.append(f'-p "$(cat {wsl_prompt})"')
        else:
            args_parts.append(f'-c "$(cat {wsl_prompt})"')

        args_str = " ".join(args_parts)

        # Build the bash command
        if wsl_cwd:
            bash_cmd = f'cd "{wsl_cwd}" && {claude_cmd} {args_str} 2>&1'
        else:
            bash_cmd = f"{claude_cmd} {args_str} 2>&1"

        cmd = ["wsl.exe", "bash", "-c", bash_cmd]

        start_time = time.time()
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout_seconds,
        )
        result["duration_seconds"] = time.time() - start_time
        result["exit_code"] = proc.returncode

        if proc.returncode == 0:
            result["success"] = True
            result["output"] = proc.stdout.strip()
        else:
            result["error"] = proc.stderr or f"Claude CLI (WSL) exited with code {proc.returncode}"

    finally:
        # Clean up temp file
        if prompt_file:
            with contextlib.suppress(Exception):
                os.unlink(prompt_file)

    return result


def clean_code_output(code: str) -> str:
    """Remove markdown code blocks and extra whitespace from AI output.

    Args:
        code: Raw output from Claude

    Returns:
        Cleaned code string
    """
    code = code.strip()

    # Remove markdown code blocks
    if code.startswith("```"):
        lines = code.split("\n")
        # Remove first line (```typescript or similar)
        lines = lines[1:]
        # Remove last line if it's just ```
        if lines and lines[-1].strip() == "```":
            lines = lines[:-1]
        code = "\n".join(lines)

    return code.strip()
