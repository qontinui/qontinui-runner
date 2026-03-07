# Qontinui Shell Integration for PowerShell (OSC 633 / VS Code compatible)
# Sourced automatically when the runner spawns a PowerShell terminal session.

# Guard: only install once
if ($env:QONTINUI_INTEGRATION -eq "1") {
    # Still source the user's profile if it exists
    if (Test-Path $PROFILE) { . $PROFILE }
    return
}
$env:QONTINUI_INTEGRATION = "1"

# Source user profile (skipped when PS runs with -Command)
if (Test-Path $PROFILE) {
    try { . $PROFILE } catch { }
}

# ESC and BEL characters for building OSC sequences (PS 5.1 compatible)
$__q_esc = [char]0x1b
$__q_bel = [char]7

# OSC 633 helper — writes the escape sequence directly to the console.
# Used for sequences emitted OUTSIDE the prompt (e.g. command execute markers).
function Write-Osc633 {
    param([string]$s)
    [Console]::Write("${__q_esc}]633;${s}${__q_bel}")
}

# Save original Prompt function
$__qontinui_original_prompt = $function:Prompt

# Override Prompt to emit shell integration markers.
# IMPORTANT: OSC sequences are embedded in the returned string so that ConPTY
# processes them together with the visible text. Writing them separately via
# [Console]::Write() can desync the cursor position that PSReadLine reads from
# the console buffer, causing typed characters to appear in the wrong place.
function global:Prompt {
    $exitCode = $LASTEXITCODE

    # Build result with embedded OSC 633 sequences (zero-width in the terminal)
    $result = "${__q_esc}]633;D;${exitCode}${__q_bel}"
    $result += "${__q_esc}]633;P;Cwd=$(Get-Location)${__q_bel}"
    $result += "${__q_esc}]633;A${__q_bel}"

    # Invoke original prompt (or a simple default)
    $text = if ($__qontinui_original_prompt) {
        try { & $__qontinui_original_prompt } catch { "PS> " }
    } else {
        "PS $($executionContext.SessionState.Path.CurrentLocation)$('>' * ($nestedPromptLevel + 1)) "
    }

    $result += $text
    $result += "${__q_esc}]633;B${__q_bel}"

    # Restore $LASTEXITCODE so it isn't clobbered by our string operations
    $global:LASTEXITCODE = $exitCode
    return $result
}

# ── Claude Code runner context ─────────────────────────────────────────────
# Wrap the `claude` command so sessions launched from this terminal
# automatically know they are running inside the Qontinui Runner.
if ($env:QONTINUI_RUNNER_TERMINAL -eq "1") {
    $__qontinui_api_port = if ($env:QONTINUI_RUNNER_API_PORT) { $env:QONTINUI_RUNNER_API_PORT } else { "9876" }
    $__qontinui_runner_context = @"
You are running inside the Qontinui Runner desktop application terminal.
The runner provides AI-driven development automation with structured workflows and verification feedback loops.

Runner HTTP API at http://localhost:${__qontinui_api_port}. Use PowerShell Invoke-WebRequest on Windows (not curl).
Key endpoints:
- GET /task-runs/running - list running task runs
- GET /task-runs/{id}/output?tail_chars=N - live AI conversation output
- GET /task-runs/{id}/workflow-state - workflow execution state
- POST /unified-workflows/execute-inline - execute a workflow inline
- GET /unified-workflows - list saved workflows

Runner SQLite DB (Windows): ~/AppData/Roaming/com.qontinui.runner/runner.db
Key tables: task_runs, task_run_events, unified_workflows, workflow_verification_phase_results
"@

    function global:claude {
        # Subcommands that do not accept --append-system-prompt
        $skip = @('mcp', 'config', 'update', 'doctor', 'api-key')
        $exe = (Get-Command claude -CommandType Application,ExternalScript -ErrorAction SilentlyContinue |
                Select-Object -First 1).Source
        if (-not $exe) {
            Write-Host "claude: command not found" -ForegroundColor Red
            return
        }
        if ($args.Count -gt 0 -and $skip -contains $args[0]) {
            & $exe @args
        } else {
            & $exe --append-system-prompt $__qontinui_runner_context @args
        }
    }
}

# Intercept PSReadLine to emit E;<command> and C before execution.
# These fire after the prompt is drawn, so [Console]::Write is safe here.
# PSReadLine is available in PS 5.1+ and all PS 7.x
if (Get-Module -Name PSReadLine -ErrorAction SilentlyContinue) {
    $__qontinui_original_readline = $function:PSConsoleHostReadLine

    function global:PSConsoleHostReadLine {
        $line = if ($__qontinui_original_readline) {
            & $__qontinui_original_readline
        } else {
            [Microsoft.PowerShell.PSConsoleReadLine]::ReadLine(
                $host.Runspace, $ExecutionContext)
        }
        if ($null -ne $line) {
            # E;<command> — command text (escape semicolons)
            $escaped = $line -replace ';', '\x3b'
            Write-Osc633 "E;$escaped"
            # C — command executing
            Write-Osc633 "C"
        }
        return $line
    }
}
