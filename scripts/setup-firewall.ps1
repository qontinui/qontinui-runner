# Pre-create Windows Defender Firewall rules for every qontinui binary
# path the supervisor + dev-start.ps1 can ever spawn, so manual testing
# never blocks on a "Windows Security Alert" popup.
#
# Why this is needed even though the runner binds to 127.0.0.1:
# Windows Defender Firewall (and 3rd-party suites like Avast on this
# laptop) check the *executable path*, not just the bind address. Each
# fresh path (`qontinui-runner-test-9877.exe` vs `qontinui-runner-test-
# 9878.exe`) is treated as a new program and triggers an interactive
# "Allow access" dialog the first time it tries to listen -- even on
# 127.0.0.1.
#
# The supervisor's exe-naming scheme (src/config.rs::runner_exe_copy_path):
#   Temp:    target/debug/qontinui-runner-test-<port>.exe
#   Named:   target/debug/qontinui-runner-named-<port>.exe
#   Primary: target/debug/qontinui-runner-<id>.exe  (id = "primary" by default)
#   Plus:    target-pool/slot-{0,1,2}/debug/qontinui-runner.exe
#            target-pool/lkg/qontinui-runner.exe
#            target/debug/qontinui-runner.exe                  (legacy)
#
# Supervisor binary lives at:
#   qontinui-supervisor/target/debug/qontinui-supervisor.exe
#   qontinui-supervisor/target/debug/copies/qontinui-supervisor.exe
#
# Temp runner port range is hard-coded to 9877-9899 in supervisor; named
# runners can claim any free port but we pre-create rules for the same
# range as a reasonable default.
#
# Run ONCE per machine, AS ADMIN. Idempotent -- re-running is a no-op for
# rules that already exist. Add new ports/paths to the loops below if you
# extend the supervisor's spawn surface.

#Requires -RunAsAdministrator

param(
    [string]$RunnerRoot = "D:\qontinui-root\qontinui-runner",
    [string]$SupervisorRoot = "D:\qontinui-root\qontinui-supervisor",
    [int]$TempPortMin = 9877,
    [int]$TempPortMax = 9899
)

$ErrorActionPreference = "Stop"

# Resolve any `subst`-mapped drive prefix in a path to the underlying
# physical path. The Windows Firewall service runs as SYSTEM and does NOT
# see per-logon subst drives, so a `Program=` rule registered against
# `D:\qontinui-root\...` will not match a process whose kernel image path
# is `C:\Users\jspin\Documents\qontinui-root\...` (the substed source).
# Result: rules silently fail to match and every spawn pops the "Allow
# access" dialog. Resolve substs at rule-creation time so the recorded
# Program path is what the firewall service actually sees at match time.
function Resolve-SubstPath {
    param([Parameter(Mandatory)][string]$Path)
    # Parse `subst` output once into a drive-letter -> real-root map.
    # Lines look like: `D:\: => C:\Users\jspin\Documents`
    $script:_SubstMap = $script:_SubstMap
    if ($null -eq $script:_SubstMap) {
        $script:_SubstMap = @{}
        try {
            $substOutput = & subst.exe 2>$null
            foreach ($line in $substOutput) {
                if ($line -match '^([A-Z]):\\?:\s*=>\s*(.+)$') {
                    $script:_SubstMap[$matches[1].ToUpper()] = $matches[2].TrimEnd('\')
                }
            }
        } catch { }
    }
    if ($Path.Length -ge 2 -and $Path[1] -eq ':') {
        $letter = $Path.Substring(0,1).ToUpper()
        if ($script:_SubstMap.ContainsKey($letter)) {
            $rest = $Path.Substring(2).TrimStart('\')
            return Join-Path $script:_SubstMap[$letter] $rest
        }
    }
    return $Path
}

# Verify admin. New-NetFirewallRule silently no-ops without it.
$current = [System.Security.Principal.WindowsPrincipal]::new(
    [System.Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $current.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Host "ERROR: this script must run elevated. Re-launch from an admin PowerShell or:" -ForegroundColor Red
    Write-Host "  Start-Process pwsh -Verb RunAs -ArgumentList '-File','D:\qontinui-root\qontinui-runner\scripts\setup-firewall.ps1'"
    exit 1
}

# Purge ANY pre-existing qontinui-multi-app-* rules before recreating. The
# rule-name format embeds the path basename, so a rename or subst-drive
# change produces a NEW name on next run and the stale entries would
# otherwise accumulate forever. The script is the sole owner of this
# DisplayName prefix; nuking the whole set on each run is safe.
$stale = Get-NetFirewallRule -DisplayName 'qontinui-multi-app-*' -ErrorAction SilentlyContinue
$staleCount = @($stale).Count
if ($staleCount -gt 0) {
    Write-Host "Removing $staleCount pre-existing qontinui-multi-app-* rules (recreating with current paths)..." -ForegroundColor Yellow
    $stale | Remove-NetFirewallRule -ErrorAction SilentlyContinue
}

# Collect every path the supervisor + dev-start might launch.
$paths = New-Object System.Collections.Generic.List[string]

# Slot-pool binaries (one per build slot -- the source for runner copies).
foreach ($slot in 0..2) {
    $paths.Add((Join-Path $RunnerRoot "target-pool\slot-$slot\debug\qontinui-runner.exe"))
}

# Last-known-good copy (survives slot-pool failures).
$paths.Add((Join-Path $RunnerRoot "target-pool\lkg\qontinui-runner.exe"))

# Legacy direct cargo-build output (rare fallback path).
$paths.Add((Join-Path $RunnerRoot "target\debug\qontinui-runner.exe"))

# Primary runner exe (id-based naming; "primary" is the default).
$paths.Add((Join-Path $RunnerRoot "target\debug\qontinui-runner-primary.exe"))

# Temp + named runner exes for every port in the supervisor's pool.
for ($port = $TempPortMin; $port -le $TempPortMax; $port++) {
    $paths.Add((Join-Path $RunnerRoot "target\debug\qontinui-runner-test-$port.exe"))
    $paths.Add((Join-Path $RunnerRoot "target\debug\qontinui-runner-named-$port.exe"))
}

# Supervisor exe + the "copies" launch path dev-start.ps1 uses.
$paths.Add((Join-Path $SupervisorRoot "target\debug\qontinui-supervisor.exe"))
$paths.Add((Join-Path $SupervisorRoot "target\debug\copies\qontinui-supervisor.exe"))

# Add an inbound rule per path. We scope `-LocalAddress 127.0.0.1` because
# every qontinui HTTP server binds loopback -- the rule grants no real
# network exposure, just lets the bind happen without a popup.
$added = 0
$failed = 0

foreach ($path in $paths) {
    # Resolve `subst`-mapped drive prefix so the Program path matches what
    # the Windows Firewall service (running as SYSTEM, no subst visibility)
    # sees on the running process's image path.
    $resolved = Resolve-SubstPath -Path $path
    $name = "qontinui-multi-app-{0}" -f ([System.IO.Path]::GetFileNameWithoutExtension($resolved) + "-" + ([System.IO.Path]::GetDirectoryName($resolved) -split '\\' | Select-Object -Last 1))
    # No per-rule skip: the bulk purge above cleared the namespace, so we
    # always create fresh. This catches the case where the same DisplayName
    # would be reused but the Program path has changed (subst, rename, etc.).
    try {
        New-NetFirewallRule -DisplayName $name -Description "Auto-added by setup-firewall.ps1: allows qontinui binaries to bind on 127.0.0.1 without triggering Windows Security Alert popups during testing." -Program $resolved -Direction Inbound -Action Allow -Protocol TCP -LocalAddress 127.0.0.1 -Profile Any -EdgeTraversalPolicy Block | Out-Null
        $added++
    } catch {
        Write-Host ("WARN: failed to add rule for {0}: {1}" -f $resolved, $_) -ForegroundColor Yellow
        $failed++
    }
}

Write-Host ""
Write-Host "Firewall setup complete:" -ForegroundColor Green
Write-Host "  purged=$staleCount (stale rules cleared at start)"
Write-Host "  added=$added (new rules created)"
Write-Host "  failed=$failed"
Write-Host ""
Write-Host "Total rules cover $($paths.Count) binary paths:"
Write-Host "  - $(3) slot-pool exes (slot-0..2)"
Write-Host "  - 1 LKG exe"
Write-Host "  - 1 legacy target/debug exe"
Write-Host "  - 1 primary runner exe"
Write-Host "  - $(($TempPortMax - $TempPortMin + 1) * 2) temp+named runner exes (ports $TempPortMin-$TempPortMax)"
Write-Host "  - 2 supervisor exes (direct + copies/)"
Write-Host ""
Write-Host "All rules are inbound TCP on 127.0.0.1 only -- no external network exposure."
Write-Host "Re-run this script after adding new binary paths or expanding the temp port range."
