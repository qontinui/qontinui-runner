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
# The published artifact has the SAME leaf name as the dev binary, and a
# different PARENT:
#
#   dev build        <checkout>/target/debug/qontinui-runner.exe
#   published build  "<install dir>/Qontinui Runner/qontinui-runner.exe"
#                      <- install dir from tauri.conf.json productName
#                         "Qontinui Runner"; exe from the cargo package name,
#                         because Tauri 2 "uses the output binary from cargo"
#                         unless mainBinaryName overrides it (tauri-utils 2.9.2,
#                         Config.main_binary_name), and this repo sets none.
#
# Until 2026-09-19 this file said the installed exe was "Qontinui Runner.exe"
# -- a Tauri 1 rule (productName renamed the binary) that nothing had measured.
# The v1.0.11 installer, listed with `7z l Qontinui.Runner_1.0.11_x64-setup.exe`,
# carries ONE main binary and it is `qontinui-runner.exe` (320 MB); no file
# named "Qontinui Runner.exe" exists in it. Parity run 35429143803 had already
# shown the shape: `%LOCALAPPDATA%\Qontinui Runner` created by the installer,
# and every probe for "Qontinui Runner.exe" inside it refused.
#
# THE INSTALL DIRECTORY CONTAINS A SPACE. Every path this script hands to
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
# the blindness the published-build parity gate exists to end. The leaf name
# cannot carry that property any more (both builds are qontinui-runner.exe),
# so it is carried by the PARENT DIRECTORY, structurally:
#
#   1. Every candidate this function builds is `<base>\$InstalledDirName\
#      $InstalledExeName` -- the exe directly inside a directory named after
#      the product. The function has no reference to $DirectExe or to a build
#      directory, so there is no expression by which it could return the dev
#      binary; a checkout's target/debug is not named "Qontinui Runner".
#   2. Assert-InstalledRunnerExe re-checks the leaf name, REQUIRES the parent
#      directory's leaf to be $InstalledDirName, and refuses any path under a
#      cargo build dir (target/debug, target/release) even when the caller
#      pointed -InstallRoot straight at one.
#   3. On no match it THROWS, naming every path it probed and listing what the
#      install directory actually holds when it exists, so the next rename is
#      measured on the first run rather than guessed at. There is no return
#      path that yields $null, so a caller cannot mistake "not found" for a
#      usable exe.
# ---------------------------------------------------------------------------
$InstalledExeName = 'qontinui-runner.exe'
$InstalledDirName = 'Qontinui Runner'

function Assert-InstalledRunnerExe {
    param([string]$Path)

    $leaf = Split-Path -Leaf $Path
    if ($leaf -ne $InstalledExeName) {
        throw ("Refusing '$Path': the installed runner is named '$InstalledExeName', not '$leaf'.")
    }
    # The parent directory is what separates the installed exe from the dev one
    # now that both are qontinui-runner.exe.
    $parentLeaf = Split-Path -Leaf (Split-Path -Parent $Path)
    if ($parentLeaf -ne $InstalledDirName) {
        throw ("Refusing '$Path': the installed runner lives directly under a '$InstalledDirName' " +
               "directory, and this one is under '$parentLeaf'. The published-build parity leg must " +
               "never run the dev binary -- that would compare the dev build against itself and " +
               "report perfect parity.")
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
        # Either shape still has to pass Assert-InstalledRunnerExe's parent-dir
        # check, so an explicit root cannot smuggle target/debug in.
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
    # An install directory that exists without the exe is the measurement the
    # next rename needs: say what is actually in it.
    foreach ($dir in @($candidates | ForEach-Object { Split-Path -Parent $_ } | Select-Object -Unique)) {
        if ((Split-Path -Leaf $dir) -eq $InstalledDirName -and (Test-Path -LiteralPath $dir -PathType Container)) {
            $lines += "Directory '$dir' EXISTS; its top-level entries:"
            foreach ($e in @(Get-ChildItem -LiteralPath $dir -Force -ErrorAction SilentlyContinue)) {
                $lines += ("  {0}{1}" -f $e.Name, $(if ($e.PSIsContainer) { '\' } else { '' }))
            }
        }
    }
    $lines += ""
    $lines += "Install the published bundle first, or pass -InstallRoot <dir> naming the"
    $lines += "directory the installer wrote '$InstalledExeName' into."
    $lines += ""
    $lines += "This harness does NOT fall back to target/debug/qontinui-runner.exe: running"
    $lines += "the dev build here would compare it against itself and report perfect parity,"
    $lines += "hiding exactly the drift this gate exists to catch."
    throw ($lines -join [Environment]::NewLine)
}
