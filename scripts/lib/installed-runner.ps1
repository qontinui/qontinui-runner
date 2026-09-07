#!/usr/bin/env pwsh
# installed-runner.ps1 -- DEFINITIONS ONLY. Dot-source it; it runs no top-level code.
#
# The ONE locator for the INSTALLED (published) runner exe, shared by every
# published-build parity leg of plan 2026-08-31-published-build-parity-check:
#
#   scripts/contract-smoke.ps1     (Phase 6 -- the behavioural axis)
#   scripts/published-parity.ps1   (Phase 5 -- the capability-manifest axis)
#
# It lives in lib/ rather than being copied into the second script because the
# "never fall back to the dev binary" property below is only worth anything if
# BOTH legs enforce it identically. Two copies would be two conventions, and the
# weaker one would decide what the parity report says.
#
# The published artifact is NOT named like the dev binary:
#
#   dev build        target/debug/qontinui-runner.exe
#                      <- the cargo package name (default-run = "qontinui-runner")
#   published build  "<install dir>/Qontinui Runner.exe"
#                      <- tauri.conf.json productName "Qontinui Runner", with NO
#                         mainBinaryName override to rename it back
#
# THE INSTALLED NAME CONTAINS A SPACE. Every path this script hands to
# Test-Path / Resolve-Path / Start-Process travels as a single argument
# (-LiteralPath / -FilePath) and is never spliced into a command string, so the
# space needs no quoting -- but any new call site must keep that property.
#
# src-tauri/tauri.conf.json declares NO bundle.windows section at all (verified
# 2026-09-02: bundle carries only active/targets/icon/externalBin/resources/
# createUpdaterArtifacts, and targets is "all"). So there is no nsis block and
# no installMode pin: the installer runs on Tauri's defaults and the install
# directory -- per-user vs per-machine -- is NOT decided in this repo and must
# not be hardcoded here. We probe the three directories an NSIS install can
# land in, in order.
#
# THIS FUNCTION MUST NEVER FALL BACK TO A DEV BINARY. A parity harness that
# failed to find the installed exe and quietly re-ran target/debug would compare
# the dev build against itself and report PERFECT PARITY -- which is precisely
# the blindness the published-build parity gate exists to end. That is made
# structural rather than conventional:
#
#   1. Every candidate this function builds ends in $InstalledExeName. The
#      string "qontinui-runner.exe" does not appear in any of them, and the
#      function has no reference to $DirectExe or to a build directory, so
#      there is no expression by which it could return the dev binary.
#   2. Assert-InstalledRunnerExe re-checks the leaf name AND refuses any path
#      under a cargo build dir (target/debug, target/release) even when the
#      caller pointed -InstallRoot straight at one.
#   3. On no match it THROWS, naming every path it probed. There is no return
#      path that yields $null, so a caller cannot mistake "not found" for a
#      usable exe.
# ---------------------------------------------------------------------------
$InstalledExeName = 'Qontinui Runner.exe'
$InstalledDirName = 'Qontinui Runner'

function Assert-InstalledRunnerExe {
    param([string]$Path)

    $leaf = Split-Path -Leaf $Path
    if ($leaf -ne $InstalledExeName) {
        throw ("Refusing '$Path': the installed runner is named '$InstalledExeName', not '$leaf'. " +
               "The published-build parity leg must never run the dev binary -- that would compare " +
               "the dev build against itself and report perfect parity.")
    }
    # Normalize separators so the build-dir guard is not defeated by forward slashes.
    $norm = ($Path -replace '/', '\')
    if ($norm -match '(?i)\\target\\(debug|release)\\') {
        throw ("Refusing '$Path': it lives under a cargo build directory. The published-build " +
               "parity leg must run the INSTALLED artifact, never anything out of target/.")
    }
}

function Find-InstalledRunnerExe {
    param([string]$InstallRoot)

    $candidates = New-Object System.Collections.Generic.List[string]
    $notes = New-Object System.Collections.Generic.List[string]

    if ($InstallRoot) {
        # An explicit root may name the install DIRECTORY or the exe itself.
        if ($InstallRoot -like '*.exe') {
            $candidates.Add($InstallRoot)
        } else {
            $candidates.Add((Join-Path $InstallRoot $InstalledExeName))
            $candidates.Add((Join-Path (Join-Path $InstallRoot $InstalledDirName) $InstalledExeName))
        }
    }

    # Probe order: per-user install first (what a CI runner's silent install
    # produces without elevation), then the two per-machine locations.
    $bases = @(
        @{ Name = 'LOCALAPPDATA';      Value = $env:LOCALAPPDATA },
        @{ Name = 'ProgramFiles';      Value = $env:ProgramFiles },
        @{ Name = 'ProgramFiles(x86)'; Value = ${env:ProgramFiles(x86)} }
    )
    foreach ($base in $bases) {
        if ([string]::IsNullOrWhiteSpace($base.Value)) {
            $notes.Add("  (`$env:$($base.Name) is unset on this box -- not probed)")
            continue
        }
        $candidates.Add((Join-Path (Join-Path $base.Value $InstalledDirName) $InstalledExeName))
    }

    foreach ($c in $candidates) {
        if (Test-Path -LiteralPath $c -PathType Leaf) {
            $resolved = (Resolve-Path -LiteralPath $c).Path
            Assert-InstalledRunnerExe -Path $resolved
            return $resolved
        }
    }

    $lines = @()
    $lines += "Could not locate the INSTALLED runner exe ('$InstalledExeName')."
    $lines += "Probed, in order:"
    foreach ($c in $candidates) { $lines += "  $c" }
    foreach ($n in $notes) { $lines += $n }
    $lines += ""
    $lines += "Install the published bundle first, or pass -InstallRoot <dir> naming the"
    $lines += "directory the installer wrote '$InstalledExeName' into."
    $lines += ""
    $lines += "This harness does NOT fall back to target/debug/qontinui-runner.exe: running"
    $lines += "the dev build here would compare it against itself and report perfect parity,"
    $lines += "hiding exactly the drift this gate exists to catch."
    throw ($lines -join [Environment]::NewLine)
}
