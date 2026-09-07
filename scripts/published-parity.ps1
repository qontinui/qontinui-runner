#!/usr/bin/env pwsh
# published-parity.ps1
#
# Phase 5 of plan 2026-08-31-published-build-parity-check: compare the
# capability manifest of the DEVELOPMENT build against the capability manifest
# of the INSTALLED PUBLISHED build, and emit the number
# `success_metric/published-runner-parity-defects` asks for -- "distinct
# capabilities that work in the development runner and not in the published
# runner".
#
# =============================================================================
# THIS SCRIPT REPORTS. IT DOES NOT JUDGE.
# =============================================================================
#
# Every parity outcome exits 0 -- including "many defects" and including the
# schema-version refusal. The exit code carries NO parity information at all.
#
#   0  a report was produced (whatever it says)
#   2  the comparison did not happen: an exe could not be located, or a
#      manifest could not be obtained. That is a statement about the HARNESS,
#      never about parity, and the report says which leg failed.
#
# Nothing here gates a merge or a release. The posture is copied from
# release.yml's "Report platform asset completeness" step, labelled in its own
# comment "VISIBILITY, not a gate": make the gap legible without changing what
# gates the publish.
#
# =============================================================================
# THE TWO DOORS, AND WHY THE DEFAULT IS HTTP
# =============================================================================
#
# The binary answers the same question two ways:
#
#   --capability-manifest --json   the COLD door. No Tauri runtime, no session.
#   GET /capability-manifest       the RUNNING door. Same renderer, same bytes,
#                                  but a live process holding an AppHandle.
#
# Measured 2026-09-02, dev build, cold CLI door: EIGHT of the nine rows report
# `unknown`.
#
#     workspace_root           operator_checkout
#     bundled_resources        unknown
#     spec_pages               unknown
#     fleet_commands           unknown
#     fleet_skills             unknown
#     fleet_agents             unknown
#     agent_definitions        unknown
#     agent_commands_registry  unknown
#     slash_commands           unknown
#
# A cold-vs-cold comparison therefore compares ONE row and finds the other eight
# "equal" only in the sense that neither side was read. See lib/parity-diff.ps1
# for why that must never be counted as parity.
#
# The HTTP door is strictly better and is the default:
#
#   * `bundled_resources` becomes observable. Its bundle rung is located through
#     Tauri's `BaseDirectory::Resource`, which needs an `AppHandle`;
#     `bundled_resources_observation()` checks `tauri_app_handle::current()` and
#     reports `unknown` when there is none. A booted instance has one. On a
#     published install that row should read `bundle_resource` where a dev box
#     reads `dev_checkout` / `exe_relative_checkout` -- a genuine, observable
#     parity difference that the cold door structurally cannot see.
#   * The published binary is GUI-subsystem in release
#     (`#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`), so
#     its redirected stdout can come back EMPTY. An empty CLI read there means
#     "GUI subsystem", NOT "produced no output" -- so the cold door is not even
#     reliably available on the leg that matters. HTTP is unaffected.
#
# =============================================================================
# WHAT THIS HARNESS CAN AND CANNOT SEE -- STATED, NOT IMPLIED
# =============================================================================
#
# Even over the HTTP door, a freshly booted instance has not spawned an agent
# session. Six of the nine rows -- fleet_commands, fleet_skills, fleet_agents,
# agent_definitions, agent_commands_registry, slash_commands -- are filled by
# the Phase 3 provisioning ledger, which records at SESSION SPAWN. No spawn, no
# reading. They will report `unknown` on BOTH legs and land in the `unobserved`
# bucket.
#
# This harness does NOT fake a spawn to fill them. A fabricated observation is
# the exact defect class the manifest's honesty rule exists to prevent.
#
# It DOES take one real read it can take honestly: a best-effort
# `GET /apps/qontinui-runner/spec/list`, which calls
# `spec_api::storage::list_pages` through the real handler and so records a real
# `spec_pages` observation. That is a genuine read through the production path,
# not an injected value; when it fails (no database, no apps-registry row) the
# row simply stays `unknown` and the report says so.
#
# So, today, the observable set over the HTTP door is at most:
#
#     workspace_root       always observed
#     bundled_resources    observed once the app handle exists
#     spec_pages           observed only if the warm-up read succeeds
#
# and the other six are structurally out of reach until this harness can drive a
# real session spawn on both legs. The report prints this per row, computed from
# the data rather than asserted, so a reader can never mistake a thin
# observation for a clean bill of health.
#
# =============================================================================
# NEVER THE DEV BINARY ON THE PUBLISHED LEG
# =============================================================================
#
# A comparator that fails to find the installed exe and quietly re-runs the dev
# build compares it against itself and reports PERFECT PARITY -- the exact
# blindness this plan exists to end. That is made structural, not conventional,
# by reusing Phase 6's locator verbatim (scripts/lib/installed-runner.ps1):
# every candidate it builds ends in 'Qontinui Runner.exe', it refuses any path
# under target\debug or target\release, and it has NO null-returning path -- on
# no match it THROWS, naming every path probed. This script adds nothing of its
# own that could reach a build directory: $DevExe and $PublishedExe are resolved
# by two separate functions that never see each other's inputs, and
# Assert-DevRunnerExe requires the dev path to be UNDER target\{debug,release}
# -- so the two sets are provably disjoint.
#
# Usage:
#   powershell -File scripts/published-parity.ps1
#   powershell -File scripts/published-parity.ps1 -InstallRoot 'C:\...\Qontinui Runner'
#   powershell -File scripts/published-parity.ps1 -JsonOut parity.json -Annotate
#   powershell -File scripts/published-parity.ps1 -Door cli    # cold door; observes ~1 row

param(
    # The development build. Default: probe target/debug then target/release.
    [string]$DevExe = $null,
    # Optional short-circuit for the installed exe (a directory, or the exe).
    [string]$InstallRoot = $null,
    # 'auto'/'http': boot each artifact and ask GET /capability-manifest.
    # 'cli':          use the cold --capability-manifest --json door only.
    [ValidateSet('auto', 'http', 'cli')]
    [string]$Door = 'auto',
    # Where to write the machine-readable row-level diff.
    [string]$JsonOut = $null,
    # Where to append a Markdown summary table (CI passes $GITHUB_STEP_SUMMARY).
    [string]$SummaryOut = $null,
    # Emit one ::warning:: workflow annotation per differing row.
    [switch]$Annotate,
    [int]$BootTimeoutSecs = 180
)

$ErrorActionPreference = "Stop"

# The locator, shared verbatim with contract-smoke.ps1's -UseInstalledExe leg.
$InstalledRunnerLib = Join-Path $PSScriptRoot "lib/installed-runner.ps1"
if (-not (Test-Path $InstalledRunnerLib)) {
    Write-Host "ERROR: missing $InstalledRunnerLib -- cannot locate the published build." -ForegroundColor Red
    exit 2
}
. $InstalledRunnerLib

# The classifier. Unit-tested by scripts/tests/test-parity-diff.ps1.
$ParityDiffLib = Join-Path $PSScriptRoot "lib/parity-diff.ps1"
if (-not (Test-Path $ParityDiffLib)) {
    Write-Host "ERROR: missing $ParityDiffLib -- cannot compare manifests." -ForegroundColor Red
    exit 2
}
. $ParityDiffLib

$RepoRoot = (Get-Item $PSScriptRoot).Parent.FullName

# ---------------------------------------------------------------------------
# The DEV binary. Deliberately a separate resolver from the installed one, with
# the mirror-image assertion: the dev exe must live UNDER a cargo build dir and
# must carry the cargo package name. The two accept-sets are disjoint by
# construction, so no input can satisfy both.
# ---------------------------------------------------------------------------
$DevExeName = 'qontinui-runner.exe'

function Assert-DevRunnerExe {
    param([string]$Path)
    $leaf = Split-Path -Leaf $Path
    if ($leaf -ne $DevExeName) {
        throw "Refusing '$Path' as the development build: expected '$DevExeName', got '$leaf'."
    }
    $norm = ($Path -replace '/', '\')
    if ($norm -notmatch '(?i)\\target\\(debug|release)\\') {
        throw ("Refusing '$Path' as the development build: it does not live under a cargo " +
               "build directory (target\debug or target\release). This leg must be the build " +
               "made from THIS checkout, not an installed artifact.")
    }
}

function Find-DevRunnerExe {
    param([string]$Explicit)

    $candidates = New-Object System.Collections.Generic.List[string]
    if ($Explicit) {
        $candidates.Add($Explicit)
    } else {
        # debug first: that is what ci.yml builds and what the dev leg of the
        # behavioural axis (contract-smoke) runs against.
        $candidates.Add((Join-Path $RepoRoot ("target\debug\" + $DevExeName)))
        $candidates.Add((Join-Path $RepoRoot ("target\release\" + $DevExeName)))
        $candidates.Add((Join-Path $RepoRoot ("src-tauri\target\debug\" + $DevExeName)))
        $candidates.Add((Join-Path $RepoRoot ("src-tauri\target\release\" + $DevExeName)))
    }

    foreach ($c in $candidates) {
        if (Test-Path -LiteralPath $c -PathType Leaf) {
            $resolved = (Resolve-Path -LiteralPath $c).Path
            Assert-DevRunnerExe -Path $resolved
            return $resolved
        }
    }

    $lines = @("Could not locate the DEVELOPMENT runner exe ('$DevExeName'). Probed, in order:")
    foreach ($c in $candidates) { $lines += "  $c" }
    $lines += ""
    $lines += "Build it first (cargo build) or pass -DevExe <path>."
    throw ($lines -join [Environment]::NewLine)
}

# ---------------------------------------------------------------------------
# Boot helpers. Deliberately NOT dot-sourced from contract-smoke.ps1, which
# executes a whole smoke run at top level and cannot be sourced for its
# functions. The readiness bar here is also lower ON PURPOSE: contract-smoke
# waits for `uiBridgeIpcObserved` because it is about to walk 198 UI Bridge
# routes; this script only needs the HTTP shell answering, because
# /capability-manifest is stateless and touches no page.
# ---------------------------------------------------------------------------
function Get-FreeParityPort {
    param([int]$Start = 9977)
    for ($p = $Start; $p -lt ($Start + 200); $p++) {
        $inUse = $false
        try {
            $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $p)
            $listener.Start()
            $listener.Stop()
        } catch {
            $inUse = $true
        }
        if (-not $inUse) { return $p }
    }
    throw "Could not find a free port in [$Start, $($Start + 200))"
}

# Downward-only process-tree kill (WebView2 hosts are re-parented to the OS by
# Windows, so Stop-Process on the root leaks them and locks the temp profile).
# Never a tree-kill FLAG: a mis-aimed `taskkill /T` on this fleet would take out
# live agent sessions. Visited-set + creation-time guard so a recycled PID can
# never pull an unrelated process in.
function Stop-ParityProcessTree {
    param([int]$RootPid)

    $all = @(Get-CimInstance -ClassName Win32_Process -ErrorAction SilentlyContinue |
        Select-Object ProcessId, ParentProcessId, Name, CreationDate)
    if ($all.Count -eq 0) { return }

    $root = $all | Where-Object { $_.ProcessId -eq $RootPid } | Select-Object -First 1
    if (-not $root) { return }
    $rootCreated = $root.CreationDate

    $byParent = @{}
    foreach ($p in $all) {
        $ppid = [int]$p.ParentProcessId
        if (-not $byParent.ContainsKey($ppid)) { $byParent[$ppid] = @() }
        $byParent[$ppid] += $p
    }

    $ordered = New-Object System.Collections.Generic.List[Object]
    $visited = @{ $RootPid = $true }
    $queue = New-Object System.Collections.Generic.Queue[int]
    $queue.Enqueue($RootPid)
    while ($queue.Count -gt 0) {
        $cur = $queue.Dequeue()
        if (-not $byParent.ContainsKey($cur)) { continue }
        foreach ($child in $byParent[$cur]) {
            $cpid = [int]$child.ProcessId
            if ($visited.ContainsKey($cpid)) { continue }
            if ($null -ne $rootCreated -and $null -ne $child.CreationDate -and $child.CreationDate -lt $rootCreated) { continue }
            $visited[$cpid] = $true
            $ordered.Add($child)
            $queue.Enqueue($cpid)
        }
    }

    for ($i = $ordered.Count - 1; $i -ge 0; $i--) {
        try { Stop-Process -Id ([int]$ordered[$i].ProcessId) -Force -ErrorAction SilentlyContinue } catch { }
    }
    try { Stop-Process -Id $RootPid -Force -ErrorAction SilentlyContinue } catch { }
}

function Start-ParityRunner {
    param([string]$ExePath, [int]$Port, [string]$Label)

    # -LiteralPath: the published exe is "Qontinui Runner.exe" under
    # "...\Qontinui Runner\". The space is harmless, but a wildcard
    # metacharacter in a user-controlled install dir would make -Path glob.
    $resolved = (Resolve-Path -LiteralPath $ExePath -ErrorAction Stop).Path
    $instanceName = "parity-$Label-$Port"

    $tmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("published-parity-" + $instanceName + "-" + [System.Guid]::NewGuid().ToString("N").Substring(0, 8))
    $configDir = Join-Path $tmpRoot "config"
    $webviewDir = Join-Path $tmpRoot "webview2"
    $logDir = Join-Path $tmpRoot "logs"
    New-Item -ItemType Directory -Force -Path $configDir  | Out-Null
    New-Item -ItemType Directory -Force -Path $webviewDir | Out-Null
    New-Item -ItemType Directory -Force -Path $logDir     | Out-Null

    $stdoutFile = Join-Path $tmpRoot "runner-stdout.log"
    $stderrFile = Join-Path $tmpRoot "runner-stderr.log"

    # IMPORTANT: this table sets isolation knobs only. It deliberately does NOT
    # touch QONTINUI_ROOT or any checkout-locating variable, and it is IDENTICAL
    # for both legs. The difference the report measures must come from the
    # ARTIFACT, not from an environment this script arranged. Whatever
    # QONTINUI_ROOT happens to be is recorded in the report's observability
    # block so a reader can see what the two legs were measured under.
    $prev = @{}
    $toSet = @{
        "QONTINUI_PORT"               = "$Port"
        "QONTINUI_INSTANCE_NAME"      = $instanceName
        "QONTINUI_PRIMARY_PORT"       = "$Port"
        "QONTINUI_CONFIG_DIR"         = $configDir
        "QONTINUI_SECURE_STORAGE_DIR" = $configDir
        "WEBVIEW2_USER_DATA_FOLDER"   = $webviewDir
        "QONTINUI_DISABLE_KEYCHAIN"   = "1"
        "QONTINUI_RUNNER_LOG_DIR"     = $logDir
    }
    foreach ($k in $toSet.Keys) {
        $prev[$k] = [System.Environment]::GetEnvironmentVariable($k, "Process")
        [System.Environment]::SetEnvironmentVariable($k, $toSet[$k], "Process")
    }
    $prev["CLAUDECODE"] = [System.Environment]::GetEnvironmentVariable("CLAUDECODE", "Process")
    [System.Environment]::SetEnvironmentVariable("CLAUDECODE", $null, "Process")

    Write-Host "  launching $Label runner on port $Port"
    Write-Host "    exe: $resolved"
    try {
        $proc = Start-Process -FilePath $resolved -PassThru -WorkingDirectory $tmpRoot `
            -RedirectStandardOutput $stdoutFile -RedirectStandardError $stderrFile
    } finally {
        foreach ($k in $prev.Keys) {
            [System.Environment]::SetEnvironmentVariable($k, $prev[$k], "Process")
        }
    }

    return [PSCustomObject]@{
        Process    = $proc
        Port       = $Port
        Label      = $Label
        TmpRoot    = $tmpRoot
        LogDir     = $logDir
        StdoutFile = $stdoutFile
        StderrFile = $stderrFile
    }
}

function Wait-ParityRunnerHttp {
    param([int]$Port, [int]$TimeoutSecs, $Process)
    $healthUrl = "http://127.0.0.1:$Port/health"
    $deadline = (Get-Date).AddSeconds($TimeoutSecs)
    Write-Host "    polling $healthUrl (timeout ${TimeoutSecs}s)"
    while ((Get-Date) -lt $deadline) {
        if ($Process -and $Process.HasExited) {
            throw "runner exited early with code $($Process.ExitCode) before answering /health"
        }
        try {
            $resp = Invoke-WebRequest -Uri $healthUrl -UseBasicParsing -TimeoutSec 5 -ErrorAction Stop
            if ($resp.StatusCode -eq 200) { return }
        } catch {
            # not listening yet
        }
        Start-Sleep -Milliseconds 1000
    }
    throw "runner did not answer $healthUrl within ${TimeoutSecs}s"
}

function Dump-ParityRunnerDiagnostics {
    param($Runner)
    Write-Host "  --- diagnostics for $($Runner.Label) ---" -ForegroundColor Yellow
    foreach ($f in @($Runner.StdoutFile, $Runner.StderrFile)) {
        if (Test-Path -LiteralPath $f) {
            $c = (Get-Content -LiteralPath $f -Raw -ErrorAction SilentlyContinue)
            if ([string]::IsNullOrWhiteSpace($c)) {
                # NOT "produced no output". A release build is GUI-subsystem
                # (`windows_subsystem = "windows"`), so redirected stdio comes
                # back empty by construction; the *.log sweep below is the only
                # channel that carries anything on that leg.
                Write-Host "    $(Split-Path -Leaf $f): (empty -- GUI subsystem, not evidence of silence)"
            } else {
                Write-Host "    $(Split-Path -Leaf $f):"
                Write-Host $c
            }
        }
    }
    if (Test-Path -LiteralPath $Runner.LogDir) {
        foreach ($log in @(Get-ChildItem -LiteralPath $Runner.LogDir -Filter *.log -ErrorAction SilentlyContinue)) {
            Write-Host "    $($log.Name):"
            Write-Host (Get-Content -LiteralPath $log.FullName -Raw -ErrorAction SilentlyContinue)
        }
    }
}

# ---------------------------------------------------------------------------
# Manifest acquisition. Returns [PSCustomObject]@{ Manifest; Door; Error }.
# Manifest is $null when the read failed; Error says why. Never throws past the
# caller -- a failed leg is a reported inability, not a crash.
# ---------------------------------------------------------------------------
function Get-ManifestOverHttp {
    param([string]$ExePath, [string]$Label, [int]$TimeoutSecs)

    $port = Get-FreeParityPort
    $runner = $null
    try {
        $runner = Start-ParityRunner -ExePath $ExePath -Port $port -Label $Label
        Wait-ParityRunnerHttp -Port $port -TimeoutSecs $TimeoutSecs -Process $runner.Process

        # Best-effort real read so `spec_pages` has an observation. This drives
        # the production handler (spec_api::storage::list_pages); it injects
        # nothing. A failure here is fine and leaves the row `unknown`.
        try {
            $null = Invoke-WebRequest -Uri "http://127.0.0.1:$port/apps/qontinui-runner/spec/list" `
                -UseBasicParsing -TimeoutSec 20 -ErrorAction Stop
            Write-Host "    spec corpus warm-up: read ok"
        } catch {
            Write-Host "    spec corpus warm-up: no reading taken ($($_.Exception.Message))"
        }

        $resp = Invoke-WebRequest -Uri "http://127.0.0.1:$port/capability-manifest" `
            -UseBasicParsing -TimeoutSec 30 -ErrorAction Stop
        $manifest = $resp.Content | ConvertFrom-Json
        return [PSCustomObject]@{ Manifest = $manifest; Door = "http:GET /capability-manifest"; Error = $null; Raw = $resp.Content }
    } catch {
        if ($runner) { Dump-ParityRunnerDiagnostics -Runner $runner }
        return [PSCustomObject]@{ Manifest = $null; Door = "http:GET /capability-manifest"; Error = $_.Exception.Message; Raw = $null }
    } finally {
        if ($runner -and $runner.Process) {
            Stop-ParityProcessTree -RootPid $runner.Process.Id
        }
        if ($runner -and (Test-Path -LiteralPath $runner.TmpRoot)) {
            Remove-Item -LiteralPath $runner.TmpRoot -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

function Get-ManifestOverCli {
    param([string]$ExePath, [string]$Label)
    try {
        # & with a single argument-bound path: the installed name contains a
        # space and is never spliced into a command string.
        $out = & $ExePath --capability-manifest --json 2>$null
        $text = ($out | Out-String)
        if ([string]::IsNullOrWhiteSpace($text)) {
            return [PSCustomObject]@{
                Manifest = $null
                Door     = "cli:--capability-manifest --json"
                Raw      = $null
                Error    = ("the CLI door returned no output. On a RELEASE build that is expected " +
                            "rather than informative: the published binary is GUI-subsystem " +
                            "(windows_subsystem = `"windows`"), so redirected stdout can come back " +
                            "empty. It is NOT evidence the binary produced nothing. Use -Door http.")
            }
        }
        return [PSCustomObject]@{ Manifest = ($text | ConvertFrom-Json); Door = "cli:--capability-manifest --json"; Error = $null; Raw = $text }
    } catch {
        return [PSCustomObject]@{ Manifest = $null; Door = "cli:--capability-manifest --json"; Error = $_.Exception.Message; Raw = $null }
    }
}

function Get-Manifest {
    param([string]$ExePath, [string]$Label, [string]$Mode, [int]$TimeoutSecs)
    if ($Mode -eq 'cli') { return Get-ManifestOverCli -ExePath $ExePath -Label $Label }
    return Get-ManifestOverHttp -ExePath $ExePath -Label $Label -TimeoutSecs $TimeoutSecs
}

# ===========================================================================
# Run.
# ===========================================================================
Write-Host ""
Write-Host "published-parity: capability-manifest parity, development build vs installed published build"
Write-Host ""

try {
    $devPath = Find-DevRunnerExe -Explicit $DevExe
} catch {
    Write-Host "PARITY-UNAVAILABLE dev_leg" -ForegroundColor Red
    Write-Host $_.Exception.Message
    exit 2
}

try {
    $pubPath = Find-InstalledRunnerExe -InstallRoot $InstallRoot
} catch {
    Write-Host "PARITY-UNAVAILABLE published_leg" -ForegroundColor Red
    Write-Host $_.Exception.Message
    exit 2
}

Write-Host "  development build : $devPath"
Write-Host "  published build   : $pubPath"
Write-Host "  door              : $Door"
Write-Host ""

if ($Door -eq 'cli') {
    Write-Host "WARNING: the cold CLI door observes at most one capability row on either leg." -ForegroundColor Yellow
    Write-Host "         Eight of nine rows report 'unknown' there and land in 'unobserved'." -ForegroundColor Yellow
    Write-Host ""
}

$devRead = Get-Manifest -ExePath $devPath -Label "dev" -Mode $Door -TimeoutSecs $BootTimeoutSecs
$pubRead = Get-Manifest -ExePath $pubPath -Label "published" -Mode $Door -TimeoutSecs $BootTimeoutSecs

$failed = @()
if ($null -eq $devRead.Manifest) { $failed += "development ($($devRead.Door)): $($devRead.Error)" }
if ($null -eq $pubRead.Manifest) { $failed += "published ($($pubRead.Door)): $($pubRead.Error)" }
if ($failed.Count -gt 0) {
    Write-Host ""
    Write-Host "PARITY-UNAVAILABLE manifest_read" -ForegroundColor Red
    foreach ($f in $failed) { Write-Host "  $f" }
    Write-Host ""
    Write-Host "  No defect count is reported. A leg that could not be read is UNKNOWN, never 0."
    exit 2
}

$result = Compare-CapabilityManifests -Dev $devRead.Manifest -Published $pubRead.Manifest `
    -Allowlist $ParityExpectedDifferences -DevDoor $devRead.Door -PublishedDoor $pubRead.Door

# ---------------------------------------------------------------------------
# Observability block -- computed from the data, plus the one thing the data
# cannot state: WHY a row is out of reach for this harness.
# ---------------------------------------------------------------------------
$sessionLedgerRows = @('fleet_commands', 'fleet_skills', 'fleet_agents',
                       'agent_definitions', 'agent_commands_registry', 'slash_commands')
$unobservedBoth = @($result.Rows | Where-Object { -not $_.DevObserved -and -not $_.PublishedObserved } | ForEach-Object { $_.Id })
$observability = [PSCustomObject]@{
    door                       = $Door
    comparable_rows            = $result.ComparableCount
    unobserved_rows            = $result.UnobservedCount
    unobserved_on_both_legs    = @($unobservedBoth)
    session_ledger_rows        = @($sessionLedgerRows)
    session_ledger_limitation  = ("These rows are filled by the Phase 3 provisioning ledger, which records at " +
                                  "AGENT SESSION SPAWN. This harness boots each artifact and asks it a question; " +
                                  "it never spawns a session and never fabricates one, so these rows report " +
                                  "'unknown' on both legs and are excluded from the defect count. Until this " +
                                  "harness can drive a real spawn on both legs, a clean report says nothing " +
                                  "about them.")
    qontinui_root_env          = $(if ($env:QONTINUI_ROOT) { $env:QONTINUI_ROOT } else { "<unset>" })
    qontinui_root_note         = ("Recorded, not manipulated. Both legs are launched under the SAME environment; " +
                                  "the difference the report measures must come from the artifact. A dev box with " +
                                  "QONTINUI_ROOT set and a clean runner without it is exactly the parity class " +
                                  "this plan describes, and workspace_root differing that way is a TRUE POSITIVE.")
}

$reportText = Format-ParityReportText -Result $result
Write-Host ""
Write-Host $reportText
Write-Host ""
Write-Host "-- Observability -----------------------------------------------------------"
Write-Host "   door: $Door   comparable rows: $($result.ComparableCount)   unobserved: $($result.UnobservedCount)"
if (@($unobservedBoth).Count -gt 0) {
    Write-Host "   unobserved on BOTH legs: $($unobservedBoth -join ', ')"
}
Write-Host "   $($observability.session_ledger_limitation)"
Write-Host "   QONTINUI_ROOT during this run: $($observability.qontinui_root_env)"
Write-Host ""

# ---------------------------------------------------------------------------
# Emission 1 of 3 -- the machine artifact.
# ---------------------------------------------------------------------------
$generatedAt = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
$reportObj = ConvertTo-ParityReportObject -Result $result -GeneratedAt $generatedAt -Observability $observability
if ($JsonOut) {
    $dir = Split-Path -Parent $JsonOut
    if ($dir -and -not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    # -Depth 10: the default of 2 would flatten rows[] into type names.
    # UTF8 without BOM via .NET so a downstream JSON parser is not fed a BOM.
    $json = $reportObj | ConvertTo-Json -Depth 10
    [System.IO.File]::WriteAllText($JsonOut, $json, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "Wrote machine-readable diff: $JsonOut"
}

# ---------------------------------------------------------------------------
# Emission 2 of 3 -- the job-summary table.
# ---------------------------------------------------------------------------
if ($SummaryOut) {
    $md = New-Object System.Collections.Generic.List[string]
    $md.Add("### Published-build capability parity")
    $md.Add("")
    if ($result.SchemaRefusal) {
        $md.Add("**Refused — schema version mismatch.** $($result.SchemaRefusalReason)")
        $md.Add("")
        $md.Add("No defect count is reported. A row diff across two manifest formats is meaningless, and ``0`` would be a claim this run did not earn.")
    } else {
        $md.Add("**parity_defects = $($result.ParityDefectCount)** (rung_differs $($result.RungDifferCount) + only_in_dev $($result.OnlyInDevCount)) — out of **$($result.ComparableCount) comparable** rows.")
        $md.Add("")
        $md.Add("**$($result.UnobservedCount) rows were unobserved** on at least one leg, so no comparison was possible for them. ``unknown`` is the absence of a reading, never agreement -- read ``parity_defects`` as a floor over the comparable set, not a verdict on the roster.")
        $md.Add("")
        $md.Add("| Capability | Development build | Published build | Disposition |")
        $md.Add("|---|---|---|---|")
        foreach ($r in @($result.Rows)) {
            $devCell = $(if ($null -eq $r.DevRung) { "_(no row)_" } else { "``$($r.DevRung)``" })
            $pubCell = $(if ($null -eq $r.PublishedRung) { "_(no row)_" } else { "``$($r.PublishedRung)``" })
            $disp = switch ($r.Disposition) {
                'defect'                { "**DEFECT**" }
                'only_in_dev'           { "**DEFECT** (absent from published roster)" }
                'only_in_dev_unobserved' { "roster difference, unobserved" }
                'only_in_published'     { "only in published roster" }
                'expected_difference'   { "expected (allowlisted)" }
                'unobserved'            { "unobserved" }
                default                 { "in parity" }
            }
            $md.Add("| ``$($r.Id)`` | $devCell | $pubCell | $disp |")
        }
        $md.Add("")
        $md.Add("Allowlisted expected differences: **$(@($result.Allowlist).Count)** entries" + $(if (@($result.Allowlist).Count -eq 0) { " — the allowlist is empty; nothing was excused." } else { ":" }))
        foreach ($e in @($result.Allowlist)) {
            $md.Add("- ``$($e.Id)`` (dev ``$($e.DevRung)`` / published ``$($e.PublishedRung)``): $($e.Reason)")
        }
        $md.Add("")
        $md.Add("Rows unobservable by this harness today (filled only at agent-session spawn): " +
                (($sessionLedgerRows | ForEach-Object { "``$_``" }) -join ", ") + ". This harness never fabricates a spawn, so a clean report says nothing about them.")
    }
    $md.Add("")
    $md.Add("Development build: ``$($result.Identity.DevAppVersion)`` / ``$($result.Identity.DevGitSha)`` via ``$($result.Identity.DevDoor)``  ")
    $md.Add("Published build: ``$($result.Identity.PublishedAppVersion)`` / ``$($result.Identity.PublishedGitSha)`` via ``$($result.Identity.PublishedDoor)``")
    $md.Add("")
    $md.Add("_This report gates nothing._")
    # UTF8 WITHOUT a BOM, via .NET: PS 5.1's `Add-Content -Encoding UTF8` writes a
    # BOM, and $GITHUB_STEP_SUMMARY is appended to -- a BOM landing mid-file renders
    # as literal garbage in the rendered summary.
    [System.IO.File]::AppendAllText($SummaryOut, (($md -join [Environment]::NewLine) + [Environment]::NewLine), (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "Appended job summary: $SummaryOut"
}

# ---------------------------------------------------------------------------
# Emission 3 of 3 -- one annotation per differing row.
# ---------------------------------------------------------------------------
if ($Annotate) {
    if ($result.SchemaRefusal) {
        Write-Host "::warning::Published-build parity REFUSED: $($result.SchemaRefusalReason). No defect count was produced."
    } else {
        foreach ($r in @($result.Rows | Where-Object { $_.Disposition -eq 'defect' -or $_.Disposition -eq 'only_in_dev' })) {
            $pub = $(if ($null -eq $r.PublishedRung) { "<absent from the published roster>" } else { $r.PublishedRung })
            Write-Host "::warning::Parity defect - $($r.Id): development build resolves '$($r.DevRung)', published build '$pub'. $($r.Note)"
        }
        foreach ($r in @($result.Rows | Where-Object { $_.Disposition -eq 'only_in_published' })) {
            Write-Host "::warning::Roster difference - $($r.Id): present only in the published build's roster (published '$($r.PublishedRung)'). Not counted as a parity defect."
        }
        if ($result.ComparableCount -eq 0) {
            Write-Host "::warning::Published-build parity compared ZERO rows. parity_defects=$($result.ParityDefectCount) is not a statement of parity."
        } elseif ($result.UnobservedCount -gt 0) {
            Write-Host "::warning::Published-build parity: $($result.UnobservedCount) of $(@($result.Rows).Count) rows were unobserved on at least one leg and could not be compared."
        }
    }
}

# The one machine-readable line, and the GitHub step output.
$verdict = Format-ParityVerdictLine -Result $result
Write-Host $verdict
if ($env:GITHUB_OUTPUT) {
    $countOut = $(if ($null -eq $result.ParityDefectCount) { "" } else { "$($result.ParityDefectCount)" })
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "parity-count=$countOut"
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "comparable-count=$($result.ComparableCount)"
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "unobserved-count=$($result.UnobservedCount)"
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "expected-difference-count=$($result.ExpectedDiffCount)"
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "schema-refused=$($result.SchemaRefusal.ToString().ToLower())"
}

# Report mode: a parity outcome NEVER sets the exit code.
exit 0
