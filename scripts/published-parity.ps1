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
# Measured 2026-09-02, dev build, cold CLI door: EIGHT of the then-nine rows report
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
#     agent_skills_registry    unknown
#     slash_commands           unknown
#
# A cold-vs-cold comparison therefore compares ONE row and finds the others
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
# Seven of the eleven rows -- fleet_commands, fleet_skills, fleet_agents,
# agent_definitions, agent_commands_registry, agent_skills_registry,
# slash_commands -- are filled by the Phase 3 provisioning ledger, which records
# at SESSION SPAWN. A harness that only boots an artifact and asks it a question
# leaves all seven `unknown` on BOTH legs, and `unknown == unknown` is the
# absence of two readings rather than agreement.
#
# Since plan
# 2026-09-20-published-runner-parity-count-comes-from-a-run-not-from-reports
# (Phase 5) this harness DRIVES that provisioning through the artifact's own
# doors before reading the manifest -- see Invoke-ParityProvisioningDrive. It
# still does NOT fake a spawn: every row is filled by the artifact's real
# provisioning code, reached through an HTTP door that artifact serves, or it
# stays `unknown` with the refusing door named.
#
#   POST /terminals                 fleet_commands, agent_commands_registry,
#                                   fleet_skills, agent_skills_registry
#                                   (acquire_for_terminal, the same chokepoint
#                                   an operator's own terminal takes)
#   POST /slash-commands/sync       slash_commands
#   POST /capability-manifest/
#        provision-probe            fleet_agents, agent_definitions -- the two
#                                   rows written only by agent_runtime's spawn
#                                   path. A 404 here means the ARTIFACT predates
#                                   the probe route (every release up to and
#                                   including v1.0.11 does), which reads as
#                                   unknown(door_404...), never as absent rows.
#
# It also takes one read it could always take honestly: a best-effort
# `GET /apps/qontinui-runner/spec/list`, which calls
# `spec_api::storage::list_pages` through the real handler and so records a real
# `spec_pages` observation. When it fails (no database, no apps-registry row) the
# row simply stays `unknown` and the report says so.
#
# The rows that need no drive at all are observable on any boot:
#
#     workspace_root       always observed
#     bundled_resources    observed once the app handle exists
#     spec_pages           observed only if the warm-up read succeeds
#     session_cli          always observed -- probed read-only beside the exe,
#                          on the cold door too (plan 2026-09-27, Phase 1d).
#                          dev 'exe_relative_checkout' vs published
#                          'bundle_resource' is the one allowlisted pair; see
#                          lib/parity-diff.ps1.
#
# The report prints observation per row, computed from the data rather than
# asserted, so a reader can never mistake a thin observation for a clean bill
# of health.
#
# AND IT DOES NOT TRUST THE ANSWER. A capability manifest is a SELF-REPORT: a row
# says which rung answered, never that a file landed. So after the drive the
# harness LISTS `.claude/{commands,skills,agents}` in the scratch workdir itself
# and compares the listing with the rows (Get-ParitySelfReportDisagreements). A
# row claiming a rung over an empty directory -- or a directory with files under
# a row that took no reading -- is reported as `self_report_disagrees`: a finding
# about THIS INSTRUMENT, counted separately and never folded into parity_defects
# or unobserved. The slash-commands verdict (`slash_commands_status`) is derived
# from that listing too, not from the manifest's own claim.
#
# =============================================================================
# NEVER THE DEV BINARY ON THE PUBLISHED LEG
# =============================================================================
#
# A comparator that fails to find the installed exe and quietly re-runs the dev
# build compares it against itself and reports PERFECT PARITY -- the exact
# blindness this plan exists to end. That is made structural, not conventional,
# by reusing Phase 6's locator verbatim (scripts/lib/installed-runner.ps1):
# every candidate it builds sits directly under a 'Qontinui Runner' install directory, it refuses any path
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
#   pwsh -File scripts/published-parity.ps1 -InstallRoot <prefix>   # Linux: the dpkg -x prefix
#   pwsh -File scripts/published-parity.ps1 -CrossPlatform -WindowsReport w.json -LinuxReport l.json

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
    [int]$BootTimeoutSecs = 180,
    # How long the provisioning drive re-asks POST /terminals while it answers
    # 409 (coord's drain state is Unknown until its first read folds). Bounded so
    # the whole drive stays inside the CI step's own timeout; see the sizing note
    # on the workflow's -TimeoutSec.
    #
    # TRADE-OFF, stated rather than buried: 60s is a THIRD less patience than the
    # 90s this started at. If a box's drain fold takes longer than this, the
    # terminal never gets created and four provisioning rows stay `unknown` on
    # both legs. That reads as UNKNOWN with the refusing door named -- honest,
    # but it is a real cut in the harness's chance of filling those rows. If
    # `terminal_create_refused` starts appearing in the artifact, raise this
    # before concluding anything about the rows it left unread.
    [int]$TerminalRetrySecs = 60,
    # INSTRUMENT SELF-CHECK, not a parity run. Boots the DEVELOPMENT build twice
    # -- once with QONTINUI_ROOT pointed at a real workspace, once at an empty
    # directory -- drives provisioning on both, and requires the comparator to
    # report at least one defect row. If it reports none, this harness cannot
    # see a missing-checkout difference and every 0 it has ever printed was
    # worthless. Needs no release and no installed exe, so it runs on a PR.
    # Exit 1 means BLIND; exit 0 means the instrument demonstrably sees the class.
    [switch]$NegativeControl,
    # CROSS-PLATFORM READ, not a parity run (Phase 6B of plan
    # 2026-09-20-published-runner-parity-count-comes-from-a-run-not-from-reports).
    # Boots nothing. Reads the JSON report the Windows leg wrote and the one the
    # Linux leg wrote, and lists the capability rows whose PUBLISHED rung differs
    # between the two platforms. That list is its own number and is never added
    # to parity_defects. Exit 0 = a list was produced (whatever it says);
    # exit 2 = it could not be (a report missing, refused, mislabelled, or the
    # two published builds are different versions), which is UNKNOWN.
    [switch]$CrossPlatform,
    [string]$WindowsReport = $null,
    [string]$LinuxReport = $null,
    # The Linux leg's typed UNKNOWN reason, when it produced no report.
    [string]$LinuxUnknown = $null
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
# The platform this run measures, decided once. Every platform-shaped choice
# below (the dev binary's name, the process table the teardown walks, the
# published locator's branch) reads it, and the report records it, so a Windows
# artifact and a Linux artifact can never be mistaken for one another.
$ParityPlatform = Get-ParityHostPlatform
$DevExeName = Get-ParityDevExeName -Platform $ParityPlatform
# There is no macOS leg: the published locator knows a Windows install dir and a
# Linux unpacked prefix, nothing else. Refuse up front (harness, exit 2) rather
# than let a macOS run fail later with a Windows-locator message. -CrossPlatform
# boots nothing and needs no locator, so it is exempt.
if ($ParityPlatform -eq 'macos' -and -not $CrossPlatform) {
    Write-Host "PARITY-UNAVAILABLE platform: no macOS published leg exists (windows and linux only)." -ForegroundColor Red
    exit 2
}

function Assert-DevRunnerExe {
    param([string]$Path)
    $leaf = Split-Path -Leaf $Path
    if ($leaf -ne $DevExeName) {
        throw "Refusing '$Path' as the development build: expected '$DevExeName', got '$leaf'."
    }
    if (-not (Test-ParityDevBuildPath -Path $Path)) {
        throw ("Refusing '$Path' as the development build: it does not live under a cargo " +
               "build directory (target/debug or target/release). This leg must be the build " +
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
        # Built segment by segment so the separator is the host's: a literal
        # `target\debug\` is a FILE NAME containing backslashes on Linux.
        foreach ($base in @($RepoRoot, (Join-Path $RepoRoot 'src-tauri'))) {
            foreach ($buildProfile in @('debug', 'release')) {
                $candidates.Add((Join-Path (Join-Path (Join-Path $base 'target') $buildProfile) $DevExeName))
            }
        }
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
#
# On Linux there is no Win32_Process; the same four fields come from
# /proc/<pid>/stat (ConvertFrom-ParityProcStat, lib/parity-diff.ps1), with the
# kernel's starttime standing in for CreationDate. WebKitGTK spawns its web and
# network processes as children of the runner, so the walk is just as needed
# there: a surviving WebKitNetworkProcess holds the temp profile open.
function Get-ParityProcessTable {
    if ($ParityPlatform -eq 'linux') {
        return @(Get-ChildItem -LiteralPath '/proc' -Directory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^\d+$' } |
            ForEach-Object {
                $line = Get-Content -LiteralPath (Join-Path $_.FullName 'stat') -Raw -ErrorAction SilentlyContinue
                ConvertFrom-ParityProcStat -Line $line
            } |
            Where-Object { $null -ne $_ })
    }
    return @(Get-CimInstance -ClassName Win32_Process -ErrorAction SilentlyContinue |
        Select-Object ProcessId, ParentProcessId, Name, CreationDate)
}

function Stop-ParityProcessTree {
    param([int]$RootPid)

    $all = @(Get-ParityProcessTable)
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
    param([string]$ExePath, [int]$Port, [string]$Label, [hashtable]$EnvOverrides = $null)

    # -LiteralPath: the published exe is "qontinui-runner.exe" under
    # "...\Qontinui Runner\". The space is harmless, but a wildcard
    # metacharacter in a user-controlled install dir would make -Path glob.
    $resolved = (Resolve-Path -LiteralPath $ExePath -ErrorAction Stop).Path
    $instanceName = "parity-$Label-$Port"

    $tmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("published-parity-" + $instanceName + "-" + [System.Guid]::NewGuid().ToString("N").Substring(0, 8))
    $configDir = Join-Path $tmpRoot "config"
    $webviewDir = Join-Path $tmpRoot "webview2"
    $logDir = Join-Path $tmpRoot "logs"
    # Per-leg embedded-PostgreSQL root. NOT optional isolation, and not derivable
    # from QONTINUI_CONFIG_DIR: embedded_pg.rs's `default_data_root()` is
    # `dirs::data_local_dir()/com.qontinui.runner/embedded-pg`, a MACHINE-SHARED
    # path that the other four dirs below cannot reach. Its own doc comment
    # (embedded_pg.rs:205-217) says a fixed root means "a temp runner cannot
    # avoid joining the machine-shared database", and its test
    # `data_root_override_wins_over_the_shared_default` says such a runner
    # "provisions or ATTACHES to the machine-shared cluster".
    #
    # MEASURED on the run that certified this harness (PR #1844, head 79582c60c):
    # after leg A, 9 postgres.exe survived as ONE cluster -- postmaster pid 4116
    # plus 8 children all at ppid 4116 -- so leg B did not start its own, it
    # attached to leg A's. Two legs of an instrument whose entire premise is that
    # they differ in ONE property were sharing a database.
    #
    # Reaping harder cannot fix that: the next leg would re-attach to the same
    # shared root. Isolation is the fix.
    $pgDir = Join-Path $tmpRoot "embedded-pg"
    New-Item -ItemType Directory -Force -Path $configDir  | Out-Null
    New-Item -ItemType Directory -Force -Path $webviewDir | Out-Null
    New-Item -ItemType Directory -Force -Path $logDir     | Out-Null
    New-Item -ItemType Directory -Force -Path $pgDir      | Out-Null

    $stdoutFile = Join-Path $tmpRoot "runner-stdout.log"
    $stderrFile = Join-Path $tmpRoot "runner-stderr.log"

    # IMPORTANT: this table sets isolation knobs only. It deliberately does NOT
    # touch QONTINUI_ROOT or any checkout-locating variable, and it is IDENTICAL
    # for both legs. The difference the report measures must come from the
    # ARTIFACT, not from an environment this script arranged. Whatever
    # QONTINUI_ROOT happens to be is recorded in the report's observability
    # block so a reader can see what the two legs were measured under.
    #
    # $EnvOverrides is the ONE exception and it exists for exactly one caller:
    # -NegativeControl, whose whole purpose is to vary the environment on
    # purpose and check that this harness can still SEE the resulting
    # difference. It is $null on every parity path, so the rule above holds
    # wherever a parity number is produced. A future caller reaching for it to
    # arrange a parity leg would be defeating the invariant, not using a
    # feature.
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
        # A blank value reads as UNSET (embedded_pg.rs
        # `blank_data_root_override_is_treated_as_unset`), so this must carry a
        # real path -- which $pgDir always does, having just been created.
        "QONTINUI_EMBEDDED_PG_DIR"    = $pgDir
    }
    if ($EnvOverrides) {
        foreach ($k in $EnvOverrides.Keys) { $toSet[$k] = $EnvOverrides[$k] }
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
# NOTE: ConvertFrom-VerbatimPath lives in lib/parity-diff.ps1 (dot-sourced
# above) so scripts/tests/test-parity-diff.ps1 -- which CI runs FIRST, under real
# Windows PowerShell 5.1, before any compile -- pins it. It was here first, and a
# defect in it cost both legs of a negative-control run (36615500004) precisely
# because nothing tested it.

# ---------------------------------------------------------------------------
# Which DATABASE ARM a booted leg took, read positively from the leg itself.
#
# This is the acceptance signal for per-leg embedded-PG isolation, and it exists
# because the obvious signal does NOT work. The tempting test is the survivor
# census: with per-leg roots, each leg owns its cluster, so `Drop` should stop
# it and the leaked postgres tree should disappear. It cannot carry that weight
# -- `Stop-ParityProcessTree` ends in `Stop-Process -Force`, and a force-kill on
# Windows does not run Rust `Drop`, so a cluster can survive because it was
# KILLED rather than because it was ATTACHED. The identical observation has two
# causes and the census cannot separate them.
#
# The arm reads the property under test directly. From embedded_pg.rs's `DbArm`,
# surfaced at `/health` -> `data.database.arm` (mcp_api.rs, "Recording which is
# the only way /health can answer the question at all"):
#
#   shared root    leg A `embedded-owned`, leg B `embedded-attached`
#   per-leg roots  BOTH legs `embedded-owned`
#
# `data.database.embeddedPort` is the second, independent half: its own doc says
# "Two runners reporting the same port is the observable form of the attach
# path's claim: one cluster, joined, not two fighting over a locked data dir."
# Same port on both legs means one cluster however the arms read.
#
# Needs no teardown to cooperate, and is one GET beside the boot poll.
# $null on any failure -- UNKNOWN, never a claim that the arm is owned.
# ---------------------------------------------------------------------------
function Get-ParityDbArm {
    param([int]$Port)
    try {
        $resp = Invoke-WebRequest -Uri "http://127.0.0.1:$Port/health" `
            -UseBasicParsing -TimeoutSec 20 -ErrorAction Stop
        $h = $resp.Content | ConvertFrom-Json
        $db = $null
        if ($h.PSObject.Properties.Name -contains 'data' -and $h.data) { $db = $h.data.database }
        elseif ($h.PSObject.Properties.Name -contains 'database') { $db = $h.database }
        if ($null -eq $db) { return [PSCustomObject]@{ arm = $null; embedded_port = $null } }
        $arm = $null; $port = $null
        if ($db.PSObject.Properties.Name -contains 'arm') { $arm = [string]$db.arm }
        if ($db.PSObject.Properties.Name -contains 'embeddedPort') { $port = $db.embeddedPort }
        return [PSCustomObject]@{ arm = $arm; embedded_port = $port }
    } catch {
        return [PSCustomObject]@{ arm = $null; embedded_port = $null }
    }
}

# ---------------------------------------------------------------------------
# Provisioning drive (plan
# 2026-09-20-published-runner-parity-count-comes-from-a-run-not-from-reports,
# Phase 5).
#
# The seven session-provisioning rows are filled by the ledger at SESSION SPAWN.
# A harness that only boots an artifact and asks it a question leaves all seven
# 'unknown' on both legs -- and `unknown == unknown` is the absence of two
# readings, not parity. So before reading the manifest we drive the provisioning
# doors the artifact itself exposes, in this order:
#
#   1. POST /terminals            -> acquire_for_terminal(), which provisions
#                                    fleet_commands + agent_commands_registry +
#                                    fleet_skills + agent_skills_registry.
#                                    A REAL user-path door: this is the same
#                                    chokepoint an operator's terminal takes.
#                                    Provisioning happens BEFORE the PTY is
#                                    created, so no claude binary or login is
#                                    needed for the rows to land.
#   2. POST /slash-commands/sync  -> slash_commands.
#   3. POST /capability-manifest/provision-probe
#                                 -> fleet_agents + agent_definitions, the two
#                                    rows written ONLY by agent_runtime's spawn
#                                    path, which needs a launchable claude, a
#                                    coord credential and a DB this box has
#                                    none of. The probe calls the same function
#                                    that path calls.
#
# NOT `POST /sessions/spawn`, which was this plan's original door and provisions
# nothing at all: its handler goes straight to ClaudeSession::spawn, which never
# reaches acquire_for_terminal, and the route is DB-backed (pg_guard), so it 503s
# under QONTINUI_ALLOW_NO_DB, which this workflow sets.
#
# Every door is BOUNDED and every failure is TYPED. A door that refuses leaves
# its rows unobserved and says which door refused and why -- never a silent
# partial that reads as a clean bill of health.
# ---------------------------------------------------------------------------
function Invoke-ParityProvisioningDrive {
    param([int]$Port, [string]$Label, [int]$TerminalRetrySecs = 60)

    $base = "http://127.0.0.1:$Port"
    # RUNNER_TEMP on a GitHub runner, the OS temp dir otherwise. Either way this
    # is outside any git work tree, which the probe door REQUIRES: the agent-path
    # provisioners overwrite .claude/agents/*.md unconditionally, so a checkout
    # would be clobbered and left dirty-from-birth.
    $tempRoot = $env:RUNNER_TEMP
    if ([string]::IsNullOrWhiteSpace($tempRoot)) { $tempRoot = [System.IO.Path]::GetTempPath() }
    $workdir = Join-Path $tempRoot ("parity-provision-" + $Label + "-" + [System.Guid]::NewGuid().ToString("N").Substring(0, 8))
    New-Item -ItemType Directory -Force -Path $workdir | Out-Null

    $drive = [PSCustomObject]@{
        workdir        = $workdir
        terminal       = 'not_attempted'
        slash_sync     = 'not_attempted'
        provision_probe = 'not_attempted'
        # Where the probe actually wrote. It creates its OWN directory inside
        # $workdir rather than writing <workdir>/.claude -- a pre-placed .claude
        # symlink would otherwise be followed straight into a checkout -- so the
        # agents listing has to follow it there. $null when the probe did not
        # answer, or answered without the field (an artifact predating it).
        probe_workdir  = $null
    }

    # --- 1. POST /terminals -------------------------------------------------
    #
    # The 409 retry is not politeness, it is the boot-time drain state: coord's
    # drain gate starts at Unknown and an HTTP terminal create is an AUTONOMOUS
    # spawn, so it is DEFERRED until the first drain read folds. On a box with no
    # ~/.qontinui/machine.json that fold resolves to NotEnrolled (allow), but it
    # has to happen first. A bounded wait distinguishes "not yet folded" from
    # "refused", which a single attempt cannot.
    $terminalId = $null
    $created = $false
    $deadline = (Get-Date).AddSeconds($TerminalRetrySecs)
    $lastRefusal = $null
    while ((Get-Date) -lt $deadline) {
        try {
            $body = @{ workingDir = $workdir; title = "published-parity provisioning probe" } | ConvertTo-Json -Compress
            $resp = Invoke-WebRequest -Uri "$base/terminals" -Method Post -Body $body `
                -ContentType 'application/json' -UseBasicParsing -TimeoutSec 30 -ErrorAction Stop
            $parsed = $resp.Content | ConvertFrom-Json
            if ($parsed.data -and $parsed.data.id) { $terminalId = [string]$parsed.data.id }
            # CREATED is created. A 2xx with no id in the body means provisioning
            # ran (that happens inside the handler before the PTY) but this
            # harness cannot close what it opened -- a different and lesser
            # problem than a refusal, and it must not be reported as one with an
            # empty reason.
            $created = $true
            $drive.terminal = $(if ($terminalId) { "created($terminalId)" } else { 'created(no id in body; cannot close it)' })
            break
        } catch {
            $code = $null
            if ($_.Exception.Response) { $code = [int]$_.Exception.Response.StatusCode }
            $lastRefusal = "HTTP $code $($_.Exception.Message)"
            if ($code -eq 409) {
                # Deferred, not refused. Wait and re-ask.
                Start-Sleep -Seconds 5
                continue
            }
            break
        }
    }
    if (-not $created) {
        $reason = $(if ($lastRefusal) { $lastRefusal } else { 'no response and no error recorded' })
        $drive.terminal = "unknown(terminal_create_refused: $reason)"
        Write-Host "    provisioning drive: POST /terminals did not create a terminal -- $reason"
    } elseif (-not $terminalId) {
        Write-Host "    provisioning drive: terminal created in $workdir but the body carried no id"
    } else {
        Write-Host "    provisioning drive: terminal $terminalId created in $workdir"
        # Close ONLY what this harness opened. Exercising a live surface must
        # leave the state it found: the terminal we created is ours to remove,
        # and nothing else here is touched.
        try {
            $null = Invoke-WebRequest -Uri "$base/terminals/$terminalId" -Method Delete `
                -UseBasicParsing -TimeoutSec 20 -ErrorAction Stop
            Write-Host "    provisioning drive: terminal $terminalId closed"
        } catch {
            Write-Host "    provisioning drive: could not close terminal $terminalId ($($_.Exception.Message))"
            $drive.terminal = "$($drive.terminal);close_failed"
        }
    }

    # --- 2. POST /slash-commands/sync --------------------------------------
    try {
        $null = Invoke-WebRequest -Uri "$base/slash-commands/sync" -Method Post `
            -UseBasicParsing -TimeoutSec 30 -ErrorAction Stop
        $drive.slash_sync = 'ok'
    } catch {
        $code = $null
        if ($_.Exception.Response) { $code = [int]$_.Exception.Response.StatusCode }
        # 503 is the honest answer on a box with no database: the route is
        # DB-backed and QONTINUI_ALLOW_NO_DB degrades it. Typed, not swallowed.
        $reason = $(if ($code -eq 503) { 'db_unavailable' } else { "http_$code" })
        $drive.slash_sync = "unknown($reason)"
        Write-Host "    provisioning drive: /slash-commands/sync -> $reason"
    }

    # --- 3. POST /capability-manifest/provision-probe -----------------------
    try {
        $body = @{ workdir = $workdir } | ConvertTo-Json -Compress
        $resp = Invoke-WebRequest -Uri "$base/capability-manifest/provision-probe" -Method Post `
            -Body $body -ContentType 'application/json' -UseBasicParsing -TimeoutSec 60 -ErrorAction Stop
        $probeParsed = $resp.Content | ConvertFrom-Json
        if ($probeParsed -and $probeParsed.provisioned_into) {
            # Normalize HERE, at the boundary. Everything downstream -- Join-Path,
            # Test-Path, Get-ChildItem -- then sees a path 5.1 can carry.
            $drive.probe_workdir = ConvertFrom-VerbatimPath ([string]$probeParsed.provisioned_into)
        }
        $drive.provision_probe = 'ok'
        Write-Host "    provisioning drive: provision-probe ok (wrote into $($drive.probe_workdir))"
    } catch {
        $code = $null
        if ($_.Exception.Response) { $code = [int]$_.Exception.Response.StatusCode }
        # A 404 here is the expected answer from any artifact built before this
        # route landed -- including every release up to v1.0.11. That is a
        # statement about the ARTIFACT, and it must read as unknown(door_404),
        # never as "the rows are absent".
        $reason = $(if ($code -eq 404) { 'door_404_artifact_predates_probe' } else { "http_$code" })
        $drive.provision_probe = "unknown($reason)"
        Write-Host "    provisioning drive: provision-probe -> $reason"
    }

    return $drive
}

# Get-ParityProvisionWitness lives in lib/parity-diff.ps1, beside the rules that
# read it, so scripts/tests/test-parity-diff.ps1 can drive it on real directories.

function Get-ManifestOverHttp {
    param([string]$ExePath, [string]$Label, [int]$TimeoutSecs, [hashtable]$EnvOverrides = $null,
          [int]$TerminalRetrySecs = 60)

    $port = Get-FreeParityPort
    $runner = $null
    try {
        $runner = Start-ParityRunner -ExePath $ExePath -Port $port -Label $Label -EnvOverrides $EnvOverrides
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

        # Fill the session-provisioning rows BEFORE reading the manifest: the
        # ledger is process-wide state, so the read below carries whatever the
        # drive just recorded. Same step, same workdir shape, on both legs.
        # Positive read of the DB arm, before the drive touches anything.
        $dbArm = Get-ParityDbArm -Port $port
        Write-Host ("    database arm: {0}   embedded port: {1}" -f `
            $(if ($null -eq $dbArm.arm) { 'unknown' } else { $dbArm.arm }), `
            $(if ($null -eq $dbArm.embedded_port) { 'none' } else { $dbArm.embedded_port }))

        $drive = Invoke-ParityProvisioningDrive -Port $port -Label $Label -TerminalRetrySecs $TerminalRetrySecs
        # The witness is CORROBORATION. It must never be able to cost us the
        # manifest read it exists to check: on CI run 36615500004 a throw in here
        # was caught below as a manifest-read failure and lost both legs. A
        # witness that cannot be taken is an all-unknown witness, which the
        # comparator rules already treat as "contradicts nothing".
        $witness = $null
        try {
            $witness = Get-ParityProvisionWitness -Workdir $drive.workdir -ProbeWorkdir $drive.probe_workdir -TerminalOutcome $drive.terminal
        } catch {
            Write-Host "    provisioning witness: could not be taken ($($_.Exception.Message)) -- reporting UNKNOWN, keeping the leg"
            $witness = [PSCustomObject]@{ commands = $null; skills = $null; agents = $null }
        }
        Write-Host ("    provisioning witness: commands={0} skills={1} agents={2}" -f `
            $(if ($null -eq $witness.commands) { 'unknown' } else { $witness.commands }), `
            $(if ($null -eq $witness.skills) { 'unknown' } else { $witness.skills }), `
            $(if ($null -eq $witness.agents) { 'unknown' } else { $witness.agents }))

        $resp = Invoke-WebRequest -Uri "http://127.0.0.1:$port/capability-manifest" `
            -UseBasicParsing -TimeoutSec 30 -ErrorAction Stop
        $manifest = $resp.Content | ConvertFrom-Json
        return [PSCustomObject]@{ Manifest = $manifest; Door = "http:GET /capability-manifest"; Error = $null; Raw = $resp.Content; Drive = $drive; Witness = $witness; DbArm = $dbArm }
    } catch {
        if ($runner) { Dump-ParityRunnerDiagnostics -Runner $runner }
        return [PSCustomObject]@{ Manifest = $null; Door = "http:GET /capability-manifest"; Error = $_.Exception.Message; Raw = $null; Drive = $null; Witness = $null; DbArm = $null }
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
                Drive    = $null
                Witness  = $null
                DbArm    = $null
                Error    = ("the CLI door returned no output. On a RELEASE build that is expected " +
                            "rather than informative: the published binary is GUI-subsystem " +
                            "(windows_subsystem = `"windows`"), so redirected stdout can come back " +
                            "empty. It is NOT evidence the binary produced nothing. Use -Door http.")
            }
        }
        return [PSCustomObject]@{ Manifest = ($text | ConvertFrom-Json); Door = "cli:--capability-manifest --json"; Error = $null; Raw = $text; Drive = $null; Witness = $null; DbArm = $null }
    } catch {
        return [PSCustomObject]@{ Manifest = $null; Door = "cli:--capability-manifest --json"; Error = $_.Exception.Message; Raw = $null; Drive = $null; Witness = $null; DbArm = $null }
    }
}

function Get-Manifest {
    param([string]$ExePath, [string]$Label, [string]$Mode, [int]$TimeoutSecs, [hashtable]$EnvOverrides = $null,
          [int]$TerminalRetrySecs = 60)
    # The CLI door takes no overrides: it is a cold process this function does
    # not launch through Start-ParityRunner, so an override would be silently
    # dropped. -NegativeControl refuses the cli door up front for that reason.
    if ($Mode -eq 'cli') { return Get-ManifestOverCli -ExePath $ExePath -Label $Label }
    return Get-ManifestOverHttp -ExePath $ExePath -Label $Label -TimeoutSecs $TimeoutSecs -EnvOverrides $EnvOverrides -TerminalRetrySecs $TerminalRetrySecs
}

# ===========================================================================
# Run.
# ===========================================================================

# ---------------------------------------------------------------------------
# -CrossPlatform: the published-windows vs published-linux list. Runs instead of
# a parity comparison and needs no binary at all, so it is decided before the
# dev locator is ever called.
# ---------------------------------------------------------------------------
if ($CrossPlatform) {
    Write-Host ""
    Write-Host "published-parity -CrossPlatform: the published build, windows vs linux (no binary is booted)"
    Write-Host ""
    $readReport = {
        param([string]$Path, [string]$Expected)
        if ([string]::IsNullOrWhiteSpace($Path) -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
            return [PSCustomObject]@{ Report = $null; Problem = $null }
        }
        try {
            $obj = Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json
        } catch {
            return [PSCustomObject]@{ Report = $null; Problem = "${Expected}_report_unparseable" }
        }
        # A report that names the WRONG platform was passed in the wrong slot,
        # and comparing it would report a platform difference that is really no
        # difference at all. One with NO platform predates Phase 6B, so it can
        # only be a Windows report -- accepted in neither slot, not guessed at.
        if ([string]$obj.platform -ne $Expected) {
            $label = $(if ($obj.platform) { [string]$obj.platform } else { 'unlabelled' })
            return [PSCustomObject]@{ Report = $null; Problem = "${Expected}_report_is_$label" }
        }
        return [PSCustomObject]@{ Report = $obj; Problem = $null }
    }
    $w = & $readReport $WindowsReport 'windows'
    $l = & $readReport $LinuxReport 'linux'
    # A leg that reported a typed UNKNOWN uploads no report; carry its reason
    # so the refusal names the release fact instead of a generic "missing".
    if ($null -eq $l.Report -and -not $l.Problem -and $LinuxUnknown) {
        $l.Problem = "linux_report_missing($LinuxUnknown)"
    }
    if ($w.Problem) {
        $cp = New-ParityCrossPlatformRefusal $w.Problem
    } elseif ($l.Problem) {
        $cp = New-ParityCrossPlatformRefusal $l.Problem
    } else {
        $cp = Compare-ParityPublishedAcrossPlatforms -WindowsReport $w.Report -LinuxReport $l.Report
    }

    $md = New-Object System.Collections.Generic.List[string]
    $md.Add("### Published build, windows vs linux")
    $md.Add("")
    if (-not $cp.Available) {
        Write-Host "CROSS-PLATFORM-UNAVAILABLE $($cp.Reason)" -ForegroundColor Yellow
        if ($cp.WindowsVersion -or $cp.LinuxVersion) {
            Write-Host "  published windows: $($cp.WindowsVersion)   published linux: $($cp.LinuxVersion)"
        }
        $md.Add("**UNKNOWN** -- ``$($cp.Reason)``. No cross-platform list was produced; this is not a statement that the two published builds agree.")
    } else {
        Write-Host ("cross-platform (published {0}): differs {1}, same {2}, unobserved {3}, roster-only {4}" -f `
            $cp.WindowsVersion, $cp.DifferCount, $cp.SameCount, $cp.UnobservedCount, $cp.OnlyOnOneCount)
        $md.Add("Published ``$($cp.WindowsVersion)`` on both platforms. Rows whose published rung differs: **$($cp.DifferCount)** (same: $($cp.SameCount), unobserved on at least one platform: $($cp.UnobservedCount), in one platform's roster only: $($cp.OnlyOnOneCount)).")
        $md.Add("")
        $md.Add("This list is NOT part of ``parity_defects``: that number is development-vs-published on one platform. ``unobserved`` is the absence of a reading on a platform, never agreement.")
        $md.Add("")
        $md.Add("| Capability | Published (windows) | Published (linux) | Disposition |")
        $md.Add("|---|---|---|---|")
        foreach ($r in @($cp.Rows)) {
            $wc = $(if ($null -eq $r.windows_published_rung) { "_(no row)_" } else { "``$($r.windows_published_rung)``" })
            $lc = $(if ($null -eq $r.linux_published_rung) { "_(no row)_" } else { "``$($r.linux_published_rung)``" })
            $md.Add("| ``$($r.id)`` | $wc | $lc | $($r.disposition) |")
            if ($r.disposition -eq 'differs') {
                Write-Host "  differs: $($r.id)  windows '$($r.windows_published_rung)'  linux '$($r.linux_published_rung)'"
                if ($Annotate) {
                    Write-Host "::warning::Cross-platform difference (published) - $($r.id): windows '$($r.windows_published_rung)', linux '$($r.linux_published_rung)'. Not a parity defect; a fact about one platform's artifact."
                }
            }
        }
    }
    $md.Add("")
    $md.Add("_This report gates nothing._")
    if ($SummaryOut) {
        [System.IO.File]::AppendAllText($SummaryOut, (($md -join [Environment]::NewLine) + [Environment]::NewLine), (New-Object System.Text.UTF8Encoding($false)))
    }
    if ($JsonOut) {
        $dir = Split-Path -Parent $JsonOut
        if ($dir -and -not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
        $cpObj = [PSCustomObject]@{
            report_kind      = 'published-build-cross-platform'
            report_version   = 1
            generated_at     = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
            available        = $cp.Available
            reason           = $cp.Reason
            windows_version  = $cp.WindowsVersion
            linux_version    = $cp.LinuxVersion
            counts           = [PSCustomObject]@{ differs = $cp.DifferCount; same = $cp.SameCount; unobserved = $cp.UnobservedCount; only_on_one = $cp.OnlyOnOneCount }
            rows             = @($cp.Rows)
        }
        [System.IO.File]::WriteAllText($JsonOut, ($cpObj | ConvertTo-Json -Depth 6), (New-Object System.Text.UTF8Encoding($false)))
    }
    if ($env:GITHUB_OUTPUT) {
        # EMPTY, never 0, when no list was produced.
        $out = $(if ($cp.Available) { "$($cp.DifferCount)" } else { "" })
        Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "cross-platform-differ-count=$out"
        Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "cross-platform-reason=$($cp.Reason)"
    }
    if (-not $cp.Available) { exit 2 }
    exit 0
}

Write-Host ""
Write-Host "published-parity: capability-manifest parity, development build vs installed published build ($ParityPlatform)"
Write-Host ""

try {
    $devPath = Find-DevRunnerExe -Explicit $DevExe
} catch {
    Write-Host "PARITY-UNAVAILABLE dev_leg" -ForegroundColor Red
    Write-Host $_.Exception.Message
    exit 2
}

# ---------------------------------------------------------------------------
# INSTRUMENT SELF-CHECK (-NegativeControl). Runs instead of a parity comparison,
# needs no release and no installed build, and answers one question: CAN this
# harness see a capability that resolves from a checkout on one side and not on
# the other? That is the negative control the parity number is worthless
# without -- a harness reporting 0 defects while structurally blind is the
# failure mode this whole plan exists to remove, and until now nothing proved it
# was not the state we were in.
#
# Both legs are the DEVELOPMENT build. That is legitimate here precisely because
# no parity claim is made: the legs are labelled by their ENVIRONMENT, the
# result is never written to $JsonOut, and no parity-count output is emitted.
# The published-leg locator is never called, so the "never the dev binary on the
# published leg" invariant above is untouched.
# ---------------------------------------------------------------------------
if ($NegativeControl) {
    if ($Door -eq 'cli') {
        Write-Host "NEGATIVE-CONTROL-UNAVAILABLE cli_door" -ForegroundColor Red
        Write-Host "  The cold door observes one row and cannot carry a provisioning reading."
        exit 2
    }

    # WITH a workspace: a SYNTHETIC one this control builds itself.
    #
    # It used to be the real workspace root above this checkout, which worked on
    # a dev box and cannot work in CI: qontinui-runner is PUBLIC and
    # qontinui-claude-config is PRIVATE, so no job here can check the latter out
    # (see the note in published-parity.yml). With no checkout present, leg A and
    # leg B would BOTH resolve the embedded floor, no row would move, and this
    # control would report the instrument BLIND -- a false alarm about the one
    # thing it exists to certify.
    #
    # Synthesising is legitimate HERE and would be fabrication in a parity leg.
    # The difference is what is being measured: a parity number is a claim about
    # two artifacts, so an invented input corrupts it; this control is a claim
    # about THIS HARNESS's sensitivity, and the honest way to test that is to
    # feed it a difference we constructed and check that it notices. Nothing from
    # this fixture reaches a parity count -- the control writes no JSON artifact
    # and emits no parity-count output.
    $withRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("parity-synthetic-root-" + [System.Guid]::NewGuid().ToString("N").Substring(0, 8))
    $synthClaude = Join-Path (Join-Path $withRoot 'qontinui-claude-config') '.claude'
    foreach ($leaf in @('agents', 'commands')) {
        New-Item -ItemType Directory -Force -Path (Join-Path $synthClaude $leaf) | Out-Null
    }
    # The shape the resolvers look for: <root>/qontinui-claude-config/.claude/agents/*.md
    # (agent_runtime.rs provision_agent_definitions_from_root) and the sibling
    # commands dir the registry overlays.
    "# synthetic agent definition, negative control only" |
        Out-File -FilePath (Join-Path (Join-Path $synthClaude 'agents') 'parity-control-a.md') -Encoding ascii
    "# synthetic agent definition, negative control only" |
        Out-File -FilePath (Join-Path (Join-Path $synthClaude 'agents') 'parity-control-b.md') -Encoding ascii
    "# synthetic command body, negative control only" |
        Out-File -FilePath (Join-Path (Join-Path $synthClaude 'commands') 'parity-control.md') -Encoding ascii
    # WITHOUT one: an empty directory. Not an unset variable -- unset would let
    # the runner fall back to its own resolution and the two legs would not
    # differ by the one thing under test.
    $emptyRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("parity-empty-root-" + [System.Guid]::NewGuid().ToString("N").Substring(0, 8))
    New-Item -ItemType Directory -Force -Path $emptyRoot | Out-Null

    Write-Host "negative control: the SAME development build, two environments"
    Write-Host "  leg A  QONTINUI_ROOT = $withRoot   (a synthetic checkout this control built)"
    Write-Host "  leg B  QONTINUI_ROOT = $emptyRoot   (an empty directory)"
    Write-Host ""

    $legA = Get-Manifest -ExePath $devPath -Label "ncontrol-with-checkout" -Mode $Door `
        -TimeoutSecs $BootTimeoutSecs -TerminalRetrySecs $TerminalRetrySecs -EnvOverrides @{ "QONTINUI_ROOT" = $withRoot }
    $legB = Get-Manifest -ExePath $devPath -Label "ncontrol-empty-root" -Mode $Door `
        -TimeoutSecs $BootTimeoutSecs -TerminalRetrySecs $TerminalRetrySecs -EnvOverrides @{ "QONTINUI_ROOT" = $emptyRoot }

    Remove-Item -LiteralPath $emptyRoot -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $withRoot -Recurse -Force -ErrorAction SilentlyContinue

    $ncFailed = @()
    if ($null -eq $legA.Manifest) { $ncFailed += "leg A ($($legA.Door)): $($legA.Error)" }
    if ($null -eq $legB.Manifest) { $ncFailed += "leg B ($($legB.Door)): $($legB.Error)" }
    if ($ncFailed.Count -gt 0) {
        Write-Host "NEGATIVE-CONTROL-UNAVAILABLE manifest_read" -ForegroundColor Red
        foreach ($f in $ncFailed) { Write-Host "  $f" }
        Write-Host "  The control did not run. That is UNKNOWN, not a pass."
        exit 2
    }

    # The real comparator, over the two real manifests.
    $nc = Compare-CapabilityManifests -Dev $legA.Manifest -Published $legB.Manifest `
        -Allowlist $ParityExpectedDifferences -DevDoor $legA.Door -PublishedDoor $legB.Door
    Write-Host (Format-ParityReportText -Result $nc)
    Write-Host ""

    $wA = $legA.Witness
    $wB = $legB.Witness
    $shown = { param($v) if ($null -eq $v) { 'unknown' } else { "$v" } }
    Write-Host ("  leg A witness: commands={0} skills={1} agents={2}" -f (& $shown $wA.commands), (& $shown $wA.skills), (& $shown $wA.agents))
    Write-Host ("  leg B witness: commands={0} skills={1} agents={2}" -f (& $shown $wB.commands), (& $shown $wB.skills), (& $shown $wB.agents))
    Write-Host ("  slash_commands_status across the two legs: {0}" -f (Get-ParitySlashCommandsStatus -DevWitness $wA -PublishedWitness $wB))
    # The per-leg embedded-PG isolation claim, read positively rather than
    # inferred from teardown residue. BOTH legs owning is the fix working; either
    # leg reading `embedded-attached`, or both reporting the same embedded port,
    # means they shared one cluster.
    $armA = $(if ($legA.DbArm) { $legA.DbArm.arm } else { $null })
    $armB = $(if ($legB.DbArm) { $legB.DbArm.arm } else { $null })
    $portA = $(if ($legA.DbArm) { $legA.DbArm.embedded_port } else { $null })
    $portB = $(if ($legB.DbArm) { $legB.DbArm.embedded_port } else { $null })
    Write-Host ("  database arm: leg A {0} (port {1}) / leg B {2} (port {3})" -f `
        $(if ($null -eq $armA) { 'unknown' } else { $armA }), `
        $(if ($null -eq $portA) { 'none' } else { $portA }), `
        $(if ($null -eq $armB) { 'unknown' } else { $armB }), `
        $(if ($null -eq $portB) { 'none' } else { $portB }))
    if ($armA -eq 'embedded-attached' -or $armB -eq 'embedded-attached') {
        Write-Host "  NOTE: a leg ATTACHED to another cluster -- the two legs shared a database. Per-leg QONTINUI_EMBEDDED_PG_DIR is not in effect." -ForegroundColor Yellow
    } elseif ($null -ne $portA -and $portA -eq $portB) {
        Write-Host "  NOTE: both legs report embedded port $portA -- one cluster, joined. Per-leg QONTINUI_EMBEDDED_PG_DIR is not in effect." -ForegroundColor Yellow
    } elseif ($armA -eq 'embedded-owned' -and $armB -eq 'embedded-owned') {
        Write-Host "  per-leg embedded-PG isolation CONFIRMED: both legs own their own cluster."
    }
    Write-Host ""

    # The assertion. A row difference on a checkout-resolved capability is what
    # "this instrument can see the class" means. `agent_definitions` is the one
    # that must move: leg A overlays the checkout's defs, leg B finds no
    # claude-config dir and reports the embedded floor / unresolved instead.
    # Only rows that CAN move when the checkout is taken away. Two that look
    # eligible are not, and listing them would overstate how many independent
    # signals back this control: `slash_commands` is `unknown` on both legs
    # wherever the sync has no database (so `unobserved`, never a defect), and
    # `workspace_root` resolves on BOTH legs because an empty directory is an
    # accepted QONTINUI_ROOT. `agent_definitions` is the row that carries this
    # assertion; the registries are genuine secondary signals.
    $sensitive = @('agent_definitions', 'agent_commands_registry', 'agent_skills_registry')
    $moved = @($nc.Rows | Where-Object {
        ($sensitive -contains $_.Id) -and
        ($_.Disposition -eq 'defect' -or $_.Disposition -eq 'only_in_dev') })
    $observedBoth = @($nc.Rows | Where-Object { $_.DevObserved -and $_.PublishedObserved })

    Write-Host "-- Negative control verdict -------------------------------------------------"
    Write-Host ("   rows observed on BOTH legs: {0}" -f @($observedBoth).Count)
    Write-Host ("   checkout-sensitive rows that differ: {0}" -f (@($moved | ForEach-Object { $_.Id }) -join ', '))

    if (@($moved).Count -lt 1) {
        Write-Host ""
        Write-Host "NEGATIVE-CONTROL FAILED: the instrument is BLIND" -ForegroundColor Red
        Write-Host ("  Removing the entire qontinui-claude-config checkout from the runner's view " +
                    "changed NO checkout-sensitive capability row. A parity run cannot see the " +
                    "class it exists to measure, so its defect count -- including a 0 -- carries " +
                    "no information. Fix the harness or the ledger before trusting another report.")
        Write-Host "::error::published-parity negative control FAILED -- the harness cannot see a missing-checkout difference."
        exit 1
    }

    Write-Host ""
    Write-Host "NEGATIVE-CONTROL OK: the instrument sees the class" -ForegroundColor Green
    $okMsg = "  {0} checkout-sensitive row(s) moved when the checkout was taken away, and {1} row(s) were observed on both legs."
    Write-Host ($okMsg -f @($moved).Count, @($observedBoth).Count)
    exit 0
}

try {
    $pubPath = Find-InstalledRunnerExe -InstallRoot $InstallRoot -Platform $ParityPlatform
} catch {
    Write-Host "PARITY-UNAVAILABLE published_leg" -ForegroundColor Red
    Write-Host $_.Exception.Message
    # A Linux locator throw that starts `unknown(<reason>)` is a typed UNKNOWN
    # from an enumerated set (lib/installed-runner.ps1 $LinuxUnknownReasons),
    # not a harness fault -- surface the reason as its own output so the
    # workflow can print it without parsing prose. Any other throw (a refusal,
    # an environment fault) has no reason and the output stays absent.
    $linuxReason = Get-LinuxUnknownReason -Message $_.Exception.Message
    if ($linuxReason -and $env:GITHUB_OUTPUT) {
        Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "linux-unknown-reason=$linuxReason"
    }
    exit 2
}

Write-Host "  development build : $devPath"
Write-Host "  published build   : $pubPath"
Write-Host "  door              : $Door"
Write-Host ""

if ($Door -eq 'cli') {
    Write-Host "WARNING: the cold CLI door observes at most one capability row on either leg." -ForegroundColor Yellow
    Write-Host "         All but one row report 'unknown' there and land in 'unobserved'." -ForegroundColor Yellow
    Write-Host ""
}

$devRead = Get-Manifest -ExePath $devPath -Label "dev" -Mode $Door -TimeoutSecs $BootTimeoutSecs -TerminalRetrySecs $TerminalRetrySecs
$pubRead = Get-Manifest -ExePath $pubPath -Label "published" -Mode $Door -TimeoutSecs $BootTimeoutSecs -TerminalRetrySecs $TerminalRetrySecs

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
                       'agent_definitions', 'agent_commands_registry',
                       'agent_skills_registry', 'slash_commands')
$unobservedBoth = @($result.Rows | Where-Object { -not $_.DevObserved -and -not $_.PublishedObserved } | ForEach-Object { $_.Id })

# The provisioning drive's own results (Phase 5). Computed here because this is
# the only place that holds BOTH legs' manifests and BOTH legs' filesystem
# listings. A disagreement between the two is a finding about the INSTRUMENT and
# is carried in its own field -- never added to parity_defects, never to
# unobserved.
$selfReportDisagreements = @()
$selfReportDisagreements += @(Get-ParitySelfReportDisagreements -Manifest $devRead.Manifest -Witness $devRead.Witness |
    ForEach-Object { $_ | Add-Member -NotePropertyName leg -NotePropertyValue 'dev' -PassThru })
$selfReportDisagreements += @(Get-ParitySelfReportDisagreements -Manifest $pubRead.Manifest -Witness $pubRead.Witness |
    ForEach-Object { $_ | Add-Member -NotePropertyName leg -NotePropertyValue 'published' -PassThru })
$slashCommandsStatus = Get-ParitySlashCommandsStatus -DevWitness $devRead.Witness -PublishedWitness $pubRead.Witness

$observability = [PSCustomObject]@{
    door                       = $Door
    comparable_rows            = $result.ComparableCount
    unobserved_rows            = $result.UnobservedCount
    unobserved_on_both_legs    = @($unobservedBoth)
    session_ledger_rows        = @($sessionLedgerRows)
    # Phase 5: what the provisioning drive did on each leg, and what the
    # directory listing witnessed afterwards.
    provisioning_drive         = [PSCustomObject]@{
        dev       = $devRead.Drive
        published = $pubRead.Drive
    }
    provisioning_witness       = [PSCustomObject]@{
        dev       = $devRead.Witness
        published = $pubRead.Witness
    }
    # Which database arm each leg took, and the embedded cluster's port. Two
    # legs reporting `embedded-attached` / the SAME port are sharing one
    # cluster, which couples two legs that must differ in one property only.
    database_arm               = [PSCustomObject]@{
        dev       = $devRead.DbArm
        published = $pubRead.DbArm
    }
    self_report_disagrees      = @($selfReportDisagreements)
    slash_commands_status      = $slashCommandsStatus
    # WHAT that verdict was measured over, so nobody has to infer it from the
    # name: the command bodies provisioned into a session workdir, NOT the
    # `slash_commands` capability row (a different mechanism -- the import of a
    # checkout's commands as runner workflows, which writes nothing here).
    slash_commands_status_source = 'session_workdir_command_listing(.claude/commands/*.md)'
    session_ledger_limitation  = ("These rows are filled by the Phase 3 provisioning ledger, which records at " +
                                  "SESSION SPAWN. This harness now DRIVES that provisioning through the " +
                                  "artifact's own doors before reading the manifest (POST /terminals for the " +
                                  "commands/skills rows, POST /slash-commands/sync for slash_commands, and " +
                                  "POST /capability-manifest/provision-probe for fleet_agents and " +
                                  "agent_definitions), and lists the resulting directories itself as an " +
                                  "independent witness. A row still reading 'unknown' after that means the " +
                                  "door it needed refused -- read provisioning_drive for which one and why. " +
                                  "Nothing here fabricates an observation.")
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
Write-Host "   slash_commands_status: $slashCommandsStatus"
if (@($selfReportDisagreements).Count -gt 0) {
    Write-Host "   SELF-REPORT DISAGREES with the filesystem on $(@($selfReportDisagreements).Count) row(s) -- a finding about the INSTRUMENT, counted separately:"
    foreach ($d in @($selfReportDisagreements)) {
        Write-Host "     [$($d.leg)] $($d.id): $($d.kind) -- $($d.note)"
    }
} else {
    Write-Host "   self-report vs filesystem witness: no disagreement recorded"
}
Write-Host "   QONTINUI_ROOT during this run: $($observability.qontinui_root_env)"
Write-Host ""

# ---------------------------------------------------------------------------
# Emission 1 of 3 -- the machine artifact.
# ---------------------------------------------------------------------------
$generatedAt = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
$reportObj = ConvertTo-ParityReportObject -Result $result -GeneratedAt $generatedAt -Observability $observability
# Which platform BOTH legs ran on (Phase 6B). Added here rather than inside
# ConvertTo-ParityReportObject because it is a fact about this run, not about
# the two manifests -- and the cross-platform comparison (-CrossPlatform, below)
# refuses a pair of reports whose platforms are not one windows and one linux.
$reportObj | Add-Member -NotePropertyName platform -NotePropertyValue $ParityPlatform
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
    $md.Add("### Published-build capability parity ($ParityPlatform)")
    $md.Add("")
    if ($result.SchemaRefusal) {
        $md.Add("**Refused -- schema version mismatch.** $($result.SchemaRefusalReason)")
        $md.Add("")
        $md.Add("No defect count is reported. A row diff across two manifest formats is meaningless, and ``0`` would be a claim this run did not earn.")
    } else {
        $md.Add("**parity_defects = $($result.ParityDefectCount)** (rung_differs $($result.RungDifferCount) + only_in_dev $($result.OnlyInDevCount)) -- out of **$($result.ComparableCount) comparable** rows.")
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
        $md.Add("Allowlisted expected differences: **$(@($result.Allowlist).Count)** entries" + $(if (@($result.Allowlist).Count -eq 0) { " -- the allowlist is empty; nothing was excused." } else { ":" }))
        foreach ($e in @($result.Allowlist)) {
            $md.Add("- ``$($e.Id)`` (dev ``$($e.DevRung)`` / published ``$($e.PublishedRung)``): $($e.Reason)")
        }
        $md.Add("")
        $md.Add("**slash_commands_status = ``" + $slashCommandsStatus + "``** (from the filesystem witness, not the manifest's self-report).")
        $md.Add("")
        if (@($selfReportDisagreements).Count -gt 0) {
            $md.Add("**Self-report disagrees with the filesystem on " + @($selfReportDisagreements).Count + " row(s).** A finding about the INSTRUMENT, counted separately from both numbers:")
            foreach ($d in @($selfReportDisagreements)) {
                $md.Add("- ``" + $d.id + "`` (" + $d.leg + "): " + $d.kind + " -- " + $d.note)
            }
            $md.Add("")
        }
        $md.Add("Provisioning rows, driven through the artifact's own doors before the manifest read: " +
                (($sessionLedgerRows | ForEach-Object { "``$_``" }) -join ", ") + ". A row still reading ``unknown`` means the door it needed refused -- see ``provisioning_drive`` in the JSON artifact for which one and why. Nothing here fabricates a spawn.")
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
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "slash-commands-status=$slashCommandsStatus"
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "self-report-disagreements=$(@($selfReportDisagreements).Count)"
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "platform=$ParityPlatform"
}

# Report mode: a parity outcome NEVER sets the exit code.
exit 0
