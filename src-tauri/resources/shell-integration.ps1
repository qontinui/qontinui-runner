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
# automatically know they are running inside the Qontinui Runner. The briefing
# text is the SINGLE SOURCE OF TRUTH rendered by the runner (Rust
# `terminal::runner_context`) and delivered via $env:QONTINUI_RUNNER_CONTEXT —
# this wrapper no longer authors its own copy. Fail-open: if the env var is
# empty we launch claude unmodified.
#
# When the runner composed a spawn file ($env:QONTINUI_RUNNER_CONTEXT_FILE: the
# same briefing plus the tenant's policy body — plan
# 2026-09-15-runner-policy-injection-off-sessionstart-hook-channel) and it still
# exists, it is passed via --append-system-prompt-file INSTEAD of the inline
# flag; Claude Code refuses both together, and a missing file is a fatal start.
if ($env:QONTINUI_RUNNER_TERMINAL -eq "1") {
    function global:claude {
        # Subcommands that do not accept --append-system-prompt
        $skip = @('mcp', 'config', 'update', 'doctor', 'api-key')
        $exe = (Get-Command claude -CommandType Application,ExternalScript -ErrorAction SilentlyContinue |
                Select-Object -First 1).Source
        if (-not $exe) {
            Write-Host "claude: command not found" -ForegroundColor Red
            return
        }
        $ctx = $env:QONTINUI_RUNNER_CONTEXT
        $ctxFile = $env:QONTINUI_RUNNER_CONTEXT_FILE
        if ($args.Count -gt 0 -and $skip -contains $args[0]) {
            & $exe @args
            return
        }
        # Classify the caller's own APPEND flags (mirrors the bash wrapper) —
        # the only pair Claude Code refuses together; --system-prompt[-file]
        # starts beside either and never changes which flag we add. A caller
        # --append-system-prompt-file owns the append slot and ours is not
        # added; caller inline flag(s) only get our briefing as ANOTHER inline
        # flag; with none, ours alone, the composed file when it exists. The
        # scan stops at `--`: what follows is the positional prompt. (A BARE
        # `--` typed at a function call is consumed by PowerShell's parameter
        # binder and never reaches $args; a quoted '--' or a splatted one does,
        # and is honored here.)
        # A caller replacement prompt (--system-prompt[-file]) is recorded
        # separately: whether Claude Code still applies an append file beside
        # it is unverified, so our file is passed but the delivered-policy
        # marker is BLANKED and the policy hook serves the full body.
        $callerPrompt = 'none'
        $replacement = $false
        foreach ($a in $args) {
            if ("$a" -eq '--') { break }
            if ("$a" -match '^--append-system-prompt-file(=.*)?$') { $callerPrompt = 'file' }
            elseif ("$a" -match '^--append-system-prompt(=.*)?$') { if ($callerPrompt -ne 'file') { $callerPrompt = 'inline' } }
            elseif ("$a" -match '^--system-prompt(-file)?(=.*)?$') { $replacement = $true }
        }
        # Touch FIRST, then test (narrows the 7-day prune race for a long-lived
        # pane). Fail-open and silent: a missing file is caught, not reported,
        # and takes the inline branch below.
        $ctxFileLive = $false
        if ($callerPrompt -eq 'none' -and -not [string]::IsNullOrEmpty($ctxFile)) {
            try {
                $item = Get-Item -LiteralPath $ctxFile -ErrorAction Stop
                $item.LastWriteTime = Get-Date
            } catch { }
            $ctxFileLive = Test-Path -LiteralPath $ctxFile -PathType Leaf
        }
        if ($ctxFileLive) {
            # The composed spawn file (briefing + policy body); the
            # delivered-policy marker rides along untouched —
            # QONTINUI_POLICY_DELIVERED_FILE names exactly this file — unless a
            # caller replacement prompt makes delivery unproven.
            if (-not $replacement) {
                & $exe --append-system-prompt-file $ctxFile @args
                return
            }
            $savedSha = $env:QONTINUI_POLICY_DELIVERED_SHA
            $savedFile = $env:QONTINUI_POLICY_DELIVERED_FILE
            $env:QONTINUI_POLICY_DELIVERED_SHA = $null
            $env:QONTINUI_POLICY_DELIVERED_FILE = $null
            try {
                & $exe --append-system-prompt-file $ctxFile @args
            } finally {
                $env:QONTINUI_POLICY_DELIVERED_SHA = $savedSha
                $env:QONTINUI_POLICY_DELIVERED_FILE = $savedFile
            }
            return
        }
        # Inline briefing, caller-owned prompt, or no briefing: no policy body
        # reaches this child, so its delivered-policy marker is BLANKED for the
        # call and restored after.
        $savedSha = $env:QONTINUI_POLICY_DELIVERED_SHA
        $savedFile = $env:QONTINUI_POLICY_DELIVERED_FILE
        $env:QONTINUI_POLICY_DELIVERED_SHA = $null
        $env:QONTINUI_POLICY_DELIVERED_FILE = $null
        try {
            if ($callerPrompt -ne 'file' -and -not [string]::IsNullOrEmpty($ctx)) {
                & $exe --append-system-prompt $ctx @args
            } else {
                & $exe @args
            }
        } finally {
            $env:QONTINUI_POLICY_DELIVERED_SHA = $savedSha
            $env:QONTINUI_POLICY_DELIVERED_FILE = $savedFile
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
