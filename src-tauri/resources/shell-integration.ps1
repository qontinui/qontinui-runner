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

# OSC 633 helper — writes the escape sequence directly to the console
function Write-Osc633 {
    param([string]$s)
    [Console]::Write([char]27 + "]633;" + $s + [char]7)
}

# Save original Prompt function
$__qontinui_original_prompt = $function:Prompt

# Override Prompt to emit shell integration markers.
# IMPORTANT: Return the actual prompt text so PSReadLine knows its width.
# Writing it via [Console]::Write and returning "" breaks cursor positioning.
function global:Prompt {
    # D;<exitcode> — command finished
    Write-Osc633 "D;$LASTEXITCODE"
    # P;Cwd=<path> — current working directory
    Write-Osc633 "P;Cwd=$(Get-Location)"
    # A — prompt start
    Write-Osc633 "A"
    # Invoke original prompt (or a simple default)
    $text = if ($__qontinui_original_prompt) {
        try { & $__qontinui_original_prompt } catch { "PS> " }
    } else {
        "PS $($executionContext.SessionState.Path.CurrentLocation)$('>' * ($nestedPromptLevel + 1)) "
    }
    # B — command input ready (emitted before returning prompt text)
    Write-Osc633 "B"
    return $text
}

# Intercept PSReadLine to emit E;<command> and C before execution
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
