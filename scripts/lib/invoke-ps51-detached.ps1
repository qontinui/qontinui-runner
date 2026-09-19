#!/usr/bin/env pwsh
# Run a Windows PowerShell 5.1 script under a BOUNDED wait, with its output in
# a file no pipe depends on, and echo that file afterwards.
#
# WHY THIS EXISTS. In published-parity.yml the contract-smoke leg ran as a bash
# step, `powershell -File scripts/contract-smoke.ps1 ... 2>&1 | tee smoke-dev.log`.
# On 2026-09-19 (run 35408109575) that leg printed its last line,
# `SMOKE-COMPLETE exit=0 pass=198 fail=0 skip=8`, 56 s after launch -- and the
# step then sat for 117 minutes until the job's 150-minute budget cancelled it.
# The script had exited; `tee` had not, because `tee` ends on EOF of its input
# pipe and EOF needs EVERY holder of the write end gone. `powershell.exe`'s
# stdout WAS that pipe, and every process it launches with redirection
# (`Start-Process -RedirectStandardOutput`, which is how contract-smoke boots
# the runner) is created with inheritable handles, so the runner and each of
# its descendants held a duplicate of the pipe. contract-smoke kills the
# runner's tree from a Win32_Process snapshot, but a process re-parented before
# the walk (WebView2 hosts, the embedded CLI) is not in that tree and outlives
# it -- and one survivor is enough to hold the pipe open forever. ci.yml runs
# the identical command with no `tee` and its step ends 5 s after
# SMOKE-COMPLETE (run 35424715044, 2026-09-19T06:48:46Z -> :51Z), which is the
# measurement that the pipe, not the script, is what hung.
#
# WHAT THIS DOES ABOUT IT. The 5.1 host is launched as
# `cmd /c powershell -File <script> <args> > <log> 2>&1` through ShellExecute
# (`Start-Process` with no redirection and no -NoNewWindow), which creates the
# process with bInheritHandles=FALSE: nothing downstream can hold THIS host's
# stdout, so the step ends when this script does. The 5.1 host's stdout is the
# log FILE, which is exactly the shape ci.yml's un-teed step gives it (a
# redirected stdout), so line width and encoding are unchanged from the shape
# the behavioural-axis grep already reads. The wait is bounded by -TimeoutSec;
# on timeout the host and its whole descendant tree are killed (the parity
# script boots two runners in sequence; a host left alive would boot the
# second one into the next leg's window) and the exit code is 124. Either way the
# processes created since the launch are listed by name, pid and creation time
# -- that listing is the diagnostic for the next hang, and it prints on the
# happy path too, so a quiet run is evidence rather than silence -- and the
# product images among them (`qontinui-runner.exe`, `Qontinui Runner.exe`) are
# stopped so the NEXT leg boots on a clean box. Only those two names, never a
# tree flag, never `node` or `powershell`: this runs on an ephemeral hosted
# image and nowhere else.
#
# The exit code is the 5.1 script's own (`exit N` under `-File` is the host's
# exit code, and `cmd /c` returns its last command's), so a step's
# `continue-on-error` / `outcome` keep their meaning.
#
# Usage (from a `shell: pwsh` step):
#   pwsh -File scripts/lib/invoke-ps51-detached.ps1 -ScriptPath scripts/contract-smoke.ps1 `
#       -LogPath smoke-dev.log -TimeoutSec 1200 -DirectExe target/debug/qontinui-runner.exe -Profile ci
# Every argument this script does not name is passed to the script verbatim,
# in order (ValueFromRemainingArguments). NOT `--`: under `pwsh -File` the
# host's own command-line parser binds the arguments, and it turns `--` into
# a named parameter with an empty name ("parameter name '' is ambiguous"),
# which fails the step before anything is launched. The language parser's
# `--` only exists for an in-process `& script.ps1 ... --` call.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string] $ScriptPath,
    [Parameter(Mandatory = $true)] [string] $LogPath,
    [int] $TimeoutSec = 1200,
    [Parameter(ValueFromRemainingArguments = $true)] [string[]] $ScriptArgs = @()
)

$ErrorActionPreference = 'Stop'

# Stop a process and every descendant, walking Win32_Process.ParentProcessId
# DOWNWARD from the root over one snapshot (the same shape as contract-smoke's
# Stop-ProcessTree, which this file cannot dot-source). Children first, so a
# parent's death cannot re-parent a child out of the walk. No tree flag.
function Stop-DescendantsThen([int] $RootPid) {
    $all = @(Get-CimInstance -ClassName Win32_Process -ErrorAction SilentlyContinue |
        Select-Object ProcessId, ParentProcessId, Name)
    $ordered = New-Object System.Collections.ArrayList
    $queue = New-Object System.Collections.Queue
    $visited = @{ $RootPid = $true }
    $queue.Enqueue($RootPid)
    while ($queue.Count -gt 0) {
        $cur = [int]$queue.Dequeue()
        foreach ($c in ($all | Where-Object { [int]$_.ParentProcessId -eq $cur })) {
            $cpid = [int]$c.ProcessId
            # A recycled pid can make a stale ParentProcessId point back into
            # the walk; the visited set is what keeps that a tree. Never the
            # system pids, never this host.
            if ($visited.ContainsKey($cpid) -or $cpid -le 4 -or $cpid -eq $PID) { continue }
            $visited[$cpid] = $true
            [void]$ordered.Add($c); $queue.Enqueue($cpid)
        }
    }
    for ($i = $ordered.Count - 1; $i -ge 0; $i--) {
        Write-Host "  stopping pid $($ordered[$i].ProcessId) ($($ordered[$i].Name)) under pid $RootPid"
        Stop-Process -Id ([int]$ordered[$i].ProcessId) -Force -ErrorAction SilentlyContinue
    }
    Stop-Process -Id $RootPid -Force -ErrorAction SilentlyContinue
}

function Quote-CmdArg([string] $s) {
    # One double-quoted cmd.exe argument. A literal double quote cannot be
    # carried through cmd /c reliably, so refuse it rather than mangle it.
    if ($s.Contains('"')) { throw "argument contains a double quote, which cmd /c cannot carry: $s" }
    return '"' + $s + '"'
}

$script = (Resolve-Path -LiteralPath $ScriptPath).Path
$log = [System.IO.Path]::GetFullPath((Join-Path (Get-Location).Path $LogPath))
if (Test-Path -LiteralPath $log) { Remove-Item -LiteralPath $log -Force }
$null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $log)

$argText = ($ScriptArgs | ForEach-Object { Quote-CmdArg $_ }) -join ' '

$inner = "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $(Quote-CmdArg $script) $argText > $(Quote-CmdArg $log) 2>&1"

# Test seam: print the assembled command line and stop, so the quoting can be
# checked on a box with no cmd.exe (scripts/tests/test-invoke-ps51-detached.ps1).
if ($env:INVOKE_PS51_DRY_RUN -eq '1') { Write-Output $inner; exit 0 }

$launchedAt = Get-Date
Write-Host "invoke-ps51-detached: running $script"
Write-Host "  args:    $argText"
Write-Host "  log:     $log"
Write-Host "  timeout: ${TimeoutSec}s"

# No -RedirectStandard*, no -NoNewWindow: that is what selects ShellExecute
# and therefore non-inheritable handles. -WindowStyle Hidden keeps the
# console off-screen; the log file is where the output goes. The argument
# string is passed as ONE element so Start-Process forwards it verbatim.
$p = Start-Process -FilePath 'cmd.exe' -PassThru -WindowStyle Hidden `
    -WorkingDirectory (Get-Location).Path `
    -ArgumentList "/d /c $inner"

$exitCode = 124
if ($p.WaitForExit($TimeoutSec * 1000)) {
    $exitCode = $p.ExitCode
    Write-Host ("invoke-ps51-detached: host exited {0} after {1:n0}s" -f $exitCode, ((Get-Date) - $launchedAt).TotalSeconds)
} else {
    Write-Host "::warning::invoke-ps51-detached: $script still running after ${TimeoutSec}s; killing pid $($p.Id) and its tree. Exit code 124."
    Stop-DescendantsThen -RootPid $p.Id
}

if (Test-Path -LiteralPath $log) {
    Write-Host "--- $LogPath ---"
    Get-Content -LiteralPath $log | ForEach-Object { Write-Host $_ }
    Write-Host "--- end $LogPath ---"
} else {
    Write-Host "::warning::invoke-ps51-detached: no log was written at $log"
}

# Survivors: every process created after the launch. Printed in full so the
# next hang names its holder; only the product images are stopped.
$survivors = @(Get-CimInstance -ClassName Win32_Process -ErrorAction SilentlyContinue |
    Where-Object { $_.CreationDate -and $_.CreationDate -gt $launchedAt -and $_.ProcessId -ne $PID })
if ($survivors.Count -eq 0) {
    Write-Host "invoke-ps51-detached: no process created since launch survives."
} else {
    Write-Host "invoke-ps51-detached: $($survivors.Count) process(es) created since launch still alive:"
    foreach ($s in $survivors) {
        Write-Host ("  pid {0,-6} ppid {1,-6} {2}  created {3:HH:mm:ss}" -f $s.ProcessId, $s.ParentProcessId, $s.Name, $s.CreationDate)
    }
    # The kill is for the hosted image only: on a developer box a runner
    # created inside this window can be a peer session's secondary or a
    # supervisor restart of the primary, and neither is this script's to stop.
    # The listing above is printed everywhere; the stop needs the CI marker.
    if ($env:GITHUB_ACTIONS -eq 'true') {
        foreach ($s in @($survivors | Where-Object { $_.Name -in @('qontinui-runner.exe', 'Qontinui Runner.exe') })) {
            Write-Host "  stopping product image pid $($s.ProcessId) ($($s.Name))"
            Stop-Process -Id $s.ProcessId -Force -ErrorAction SilentlyContinue
        }
    } else {
        Write-Host "  (not GitHub Actions: product images listed, not stopped)"
    }
}

exit $exitCode
