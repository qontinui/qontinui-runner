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
#
# LINUX (plan 2026-09-20-published-runner-parity-count-comes-from-a-run-not-from-reports,
# Phase 6). Find-InstalledRunnerExe takes a -Platform that defaults to the host,
# and on linux it delegates to Find-PublishedLinuxRunner, below. There is no
# install directory to probe there: the leg unpacks the release asset into a
# temp prefix without root, so -InstallRoot is REQUIRED and names that prefix.
# Measured on v1.0.11 (2026-10-04, `dpkg-deb -c` of the .deb, and the AppImage
# run with --appimage-extract), the two shapes put the binary at the same
# relative path:
#
#   dpkg -x Qontinui.Runner_<v>_amd64.deb <prefix>   <prefix>/usr/bin/qontinui-runner
#   <AppImage> --appimage-extract  (cwd <prefix>)    <prefix>/squashfs-root/usr/bin/qontinui-runner
#
# with the sidecars (qontinui-pr, qontinui_profile, ...) beside it in usr/bin
# and the bundled resources under "usr/lib/Qontinui Runner/".
#
# ON LINUX THE PATH RULE IS A GUARD, NOT A PROOF OF PROVENANCE. Neither shape
# has a product-named parent, so the structural property is usr/bin: the binary
# must sit directly in a `bin` directory whose parent is `usr`, which the dev
# binary (target/debug/qontinui-runner) does not. The CI build tree carries
# usr/bin layouts of its own (target/<triple>/release/bundle/appimage/
# Qontinui Runner.AppDir/usr/bin/, and the deb staging dir beside it), so
# Assert-PublishedLinuxRunner also refuses any path with a `target` or
# `target-*` segment (the shared target-agent/ dir included) and any path inside
# this repo's own checkout -- on the path as given and on its realpath, so a
# symlink planted in the prefix cannot point back at one of those. That refuses
# the KNOWN build and checkout shapes and nothing more: a build under a
# CARGO_TARGET_DIR with another name, or a dev binary hard-linked or copied
# into a well-shaped prefix, is FOUND (test-installed-runner.ps1 pins this).
# What proves the prefix came from the release is Confirm-PublishedLinuxAssetHash
# on the downloaded .deb / AppImage, matched by sha256 against the release's
# checksums-linux-x64.txt BEFORE it is unpacked. A leg that skips that call has
# a guard and no provenance.
#
# A Linux leg that cannot locate the binary is not a parity number. Its throws
# start `unknown(<reason>)` with <reason> from $LinuxUnknownReasons, and
# Get-LinuxUnknownReason reads it back, so the caller can record the typed
# UNKNOWN instead of a 0 or a red run. A fact about the RELEASE is an unknown
# too, and it is decided from the release's asset LIST, never from a missing
# local file: no Linux asset (Select-PublishedLinuxAsset), or no
# checksums-linux-x64.txt to prove one against (Select-PublishedLinuxChecksums).
# The other throws are NOT unknowns, and all of them mean the run is red:
#   `Refusing`      the guard fired; the asset or checksums file the harness
#                   downloaded is not on disk (a failed download is a harness
#                   fault, not a release fact); or the asset's hash matched no
#                   checksums line -- the harness was pointed at the wrong
#                   thing, or the download is incomplete
#   `environment:`  the box lacks what the guard needs, e.g. GNU realpath
#   plain error     the CALLER passed nothing to work with (an empty -Version,
#                   a $null -AssetNames, an empty -ChecksumsPath), which no
#                   release fact can explain
# ---------------------------------------------------------------------------
$InstalledExeName = 'qontinui-runner.exe'
$InstalledDirName = 'Qontinui Runner'
$LinuxBinaryName = 'qontinui-runner'
# The enumerated set. A reason outside it is an internal error, never a soft UNKNOWN.
#   no_linux_asset_on_release  the release carries neither a .deb nor an AppImage
#                              (the Linux release leg is continue-on-error; v1.0.6
#                              and v1.0.7 shipped none)
#   no_checksums_on_release    the release's asset list has no checksums-linux-x64.txt,
#                              so the asset's provenance cannot be proven
#   no_prefix_given            -InstallRoot was empty, so nothing was unpacked to look in
#   prefix_missing             the prefix does not exist (the unpack never ran or failed)
#   binary_absent              the prefix exists but holds the binary in neither shape
$LinuxUnknownReasons = @('no_linux_asset_on_release', 'no_checksums_on_release', 'no_prefix_given', 'prefix_missing', 'binary_absent')
# lib/ -> scripts/ -> the checkout this harness runs from.
$GuardRepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)

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
    param([string]$InstallRoot, [ValidateSet('windows', 'linux')] [string]$Platform = '')

    if (-not $Platform) {
        # $IsLinux does not exist on Windows PowerShell 5.1, and $null is false.
        if ($IsLinux) { $Platform = 'linux' } else { $Platform = 'windows' }
    }
    if ($Platform -eq 'linux') { return (Find-PublishedLinuxRunner -Prefix $InstallRoot) }

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

function New-LinuxUnknown {
    param([string]$Reason, [string]$Detail)

    if ($LinuxUnknownReasons -notcontains $Reason) {
        throw ("internal: '$Reason' is not one of the enumerated Linux unknown reasons (" +
               ($LinuxUnknownReasons -join ', ') + ").")
    }
    return ("unknown($Reason): $Detail")
}

# Reads the typed reason back out of a thrown message; $null when the message is
# not one of ours (a refusal, or any other failure), which the caller must then
# treat as a failure and not as an UNKNOWN.
function Get-LinuxUnknownReason {
    param([string]$Message)

    if ($Message -match '^unknown\((?<r>[a-z_]+)\)' -and $LinuxUnknownReasons -contains $Matches['r']) {
        return $Matches['r']
    }
    return $null
}

# Picks the Linux asset to unpack from a release's asset names: the .deb, else
# the AppImage. GitHub stores "Qontinui Runner_<v>_amd64.deb" as
# "Qontinui.Runner_<v>_amd64.deb" (measured on v1.0.11), so both spellings match.
# -Version may carry the tag's leading `v`; the asset names never do.
function Select-PublishedLinuxAsset {
    param([string[]]$AssetNames, [string]$Version)

    if ($null -eq $AssetNames) {
        throw ("Select-PublishedLinuxAsset: -AssetNames is null. That is a listing that never ran, " +
               "not a release with no Linux asset; pass @() only for a release that has no assets.")
    }
    $bare = ([string]$Version).Trim() -replace '^v', ''
    if ([string]::IsNullOrWhiteSpace($bare)) {
        throw ("Select-PublishedLinuxAsset: -Version is empty ('$Version'). An empty version would " +
               "match no asset and read as no_linux_asset_on_release, which it is not.")
    }
    $v = [regex]::Escape($bare)
    foreach ($kind in @(@{ Kind = 'deb'; Ext = 'deb' }, @{ Kind = 'appimage'; Ext = 'AppImage' })) {
        foreach ($n in @($AssetNames)) {
            if ($n -cmatch ('^Qontinui[. ]Runner_' + $v + '_amd64\.' + $kind.Ext + '$')) {
                return @{ Kind = $kind.Kind; Name = $n }
            }
        }
    }
    $listed = '(none)'
    if (@($AssetNames).Count -gt 0) { $listed = (@($AssetNames) -join ', ') }
    throw (New-LinuxUnknown 'no_linux_asset_on_release' ("release $bare carries no " +
           "Qontinui.Runner_${bare}_amd64.deb and no _amd64.AppImage. Assets: $listed"))
}

# Names the checksums asset to download from a release's asset names, or throws
# unknown(no_checksums_on_release) when the release carries none. This, not a
# missing local file, is where that reason is decided.
function Select-PublishedLinuxChecksums {
    param([string[]]$AssetNames)

    if ($null -eq $AssetNames) {
        throw ("Select-PublishedLinuxChecksums: -AssetNames is null. That is a listing that never ran, " +
               "not a release with no checksums; pass @() only for a release that has no assets.")
    }
    foreach ($n in @($AssetNames)) {
        if ($n -ceq 'checksums-linux-x64.txt') { return $n }
    }
    $listed = '(none)'
    if (@($AssetNames).Count -gt 0) { $listed = (@($AssetNames) -join ', ') }
    throw (New-LinuxUnknown 'no_checksums_on_release' ("the release carries no checksums-linux-x64.txt, " +
           "so no Linux asset's provenance can be proven. Assets: $listed"))
}

# The provenance check. Call it on the DOWNLOADED .deb / AppImage before
# unpacking it. Matched by sha256, never by name: checksums-linux-x64.txt names
# the pre-upload path ("deb/Qontinui Runner_1.0.11_amd64.deb", with a space)
# while the asset is "Qontinui.Runner_1.0.11_amd64.deb", so a name match would
# never succeed. Returns the matched checksums line. Both paths are files the
# harness downloaded, so either one missing throws `Refusing` (a failed download
# is a harness fault; whether the release HAS a checksums file is
# Select-PublishedLinuxChecksums' question), as does a hash that matches no line,
# because an asset that cannot be proven published must not be measured. An
# empty -ChecksumsPath is a plain caller error.
function Confirm-PublishedLinuxAssetHash {
    param([string]$AssetPath, [string]$ChecksumsPath)

    if ([string]::IsNullOrWhiteSpace($AssetPath) -or -not (Test-Path -LiteralPath $AssetPath -PathType Leaf)) {
        throw ("Refusing '$AssetPath': cannot check its provenance, it is not a file.")
    }
    if ([string]::IsNullOrWhiteSpace($ChecksumsPath)) {
        throw ("Confirm-PublishedLinuxAssetHash: -ChecksumsPath is empty ('$ChecksumsPath').")
    }
    if (-not (Test-Path -LiteralPath $ChecksumsPath -PathType Leaf)) {
        throw ("Refusing '$AssetPath': the checksums file '$ChecksumsPath' is not on disk, so its " +
               "provenance cannot be proven. The download failed; that is a harness fault, not a release fact.")
    }
    $hash = (Get-FileHash -LiteralPath $AssetPath -Algorithm SHA256).Hash.ToLowerInvariant()
    foreach ($line in @(Get-Content -LiteralPath $ChecksumsPath)) {
        if ($line -match '^\s*(?<h>[0-9A-Fa-f]{64})\s') {
            if ($Matches['h'].ToLowerInvariant() -eq $hash) { return $line.Trim() }
        }
    }
    throw ("Refusing '$AssetPath': its sha256 $hash matches no line in '$ChecksumsPath'. It is not " +
           "an asset this release published, or the download is incomplete; re-download before treating " +
           "this as a defect. Nothing unpacked from it may stand for the published build.")
}

# GNU realpath -m canonicalizes without requiring the path to exist, so the
# guard can run on a candidate before it is probed. Linux-only by construction.
# A missing or non-GNU realpath is the BOX's problem, not the candidate's, so it
# throws `environment:` rather than `Refusing`.
function Resolve-LinuxRealPath {
    param([string]$Path)

    if (-not (Get-Command realpath -CommandType Application -ErrorAction SilentlyContinue)) {
        throw ("environment: no realpath on PATH. The never-the-dev-binary guard needs GNU " +
               "coreutils realpath (-m) and cannot run without it.")
    }
    $out = & realpath -m -- $Path 2>&1
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace([string]$out)) {
        throw ("environment: ``realpath -m`` exited $LASTEXITCODE (" + ([string]$out).Trim() + "). " +
               "The guard needs GNU coreutils realpath; a non-GNU one has no -m.")
    }
    return ([string]$out).Trim()
}

function Assert-PublishedLinuxRunner {
    param([string]$Path)

    $repo = Resolve-LinuxRealPath -Path $GuardRepoRoot
    $real = Resolve-LinuxRealPath -Path $Path
    foreach ($p in @($Path, $real)) {
        $leaf = Split-Path -Leaf $p
        if ($leaf -cne $LinuxBinaryName) {
            throw ("Refusing '$Path': the published Linux runner is named '$LinuxBinaryName', not '$leaf'.")
        }
        $bin = Split-Path -Parent $p
        if ((Split-Path -Leaf $bin) -cne 'bin' -or (Split-Path -Leaf (Split-Path -Parent $bin)) -cne 'usr') {
            throw ("Refusing '$Path' (as '$p'): the published Linux runner lives directly under usr/bin " +
                   "of an unpacked .deb or AppImage. The published-build parity leg must never run the " +
                   "dev binary -- that would compare the dev build against itself and report perfect parity.")
        }
        if ($p -match '(^|/)target(-[^/]*)?/') {
            throw ("Refusing '$Path' (as '$p'): it lives under a cargo build directory. The published-build " +
                   "parity leg must run the PUBLISHED artifact, never anything out of target/.")
        }
        if ($p -eq $repo -or $p.StartsWith($repo.TrimEnd('/') + '/')) {
            throw ("Refusing '$Path' (as '$p'): it lives inside this repo's checkout ('$repo'). The " +
                   "published prefix must be unpacked outside it, so nothing built here can stand in for it.")
        }
    }
    # The canonical path is the one every check above passed, so it is the one
    # the caller runs.
    return $real
}

function Find-PublishedLinuxRunner {
    param([string]$Prefix)

    if ([string]::IsNullOrWhiteSpace($Prefix)) {
        throw (New-LinuxUnknown 'no_prefix_given' ("no prefix to probe. Unpack the release asset first " +
               "(dpkg -x <deb> <prefix>, or <AppImage> --appimage-extract in <prefix>) and pass " +
               "-InstallRoot <prefix>. There is no default location and no fallback to target/."))
    }

    # An explicit path may name the binary itself -- but only an existing FILE
    # is taken as that; a prefix DIRECTORY that happens to be named
    # qontinui-runner (dpkg -x into /tmp/qontinui-runner) is probed like any other.
    $candidates = New-Object System.Collections.Generic.List[string]
    if ((Split-Path -Leaf $Prefix) -ceq $LinuxBinaryName -and (Test-Path -LiteralPath $Prefix -PathType Leaf)) {
        $candidates.Add($Prefix)
    } else {
        $candidates.Add((Join-Path (Join-Path (Join-Path $Prefix 'usr') 'bin') $LinuxBinaryName))
        $candidates.Add((Join-Path (Join-Path (Join-Path (Join-Path $Prefix 'squashfs-root') 'usr') 'bin') $LinuxBinaryName))
    }

    # Guard BEFORE probing: a candidate in a known build dir is refused whether
    # or not anything is there yet, so it cannot even reach the unknown arm.
    $checked = @{}
    foreach ($c in $candidates) { $checked[$c] = Assert-PublishedLinuxRunner -Path $c }

    foreach ($c in $candidates) {
        if (Test-Path -LiteralPath $c -PathType Leaf) {
            return $checked[$c]
        }
    }
    $reason = 'binary_absent'
    if (-not (Test-Path -LiteralPath $Prefix)) { $reason = 'prefix_missing' }
    $lines = @()
    $lines += (New-LinuxUnknown $reason "could not locate the PUBLISHED Linux runner ('$LinuxBinaryName').")
    $lines += "Probed, in order:"
    foreach ($c in $candidates) { $lines += "  $c" }
    if ($reason -eq 'binary_absent' -and (Test-Path -LiteralPath $Prefix -PathType Container)) {
        $entries = @(Get-ChildItem -LiteralPath $Prefix -Force -ErrorAction SilentlyContinue)
        if ($entries.Count -eq 0) {
            $lines += "Prefix '$Prefix' EXISTS and is EMPTY -- the unpack wrote nothing."
        } else {
            $lines += "Prefix '$Prefix' EXISTS; its top-level entries:"
            foreach ($e in $entries) {
                $lines += ("  {0}{1}" -f $e.Name, $(if ($e.PSIsContainer) { '/' } else { '' }))
            }
        }
    }
    $lines += ""
    $lines += "This harness does NOT fall back to target/debug/qontinui-runner: running the"
    $lines += "dev build here would compare it against itself and report perfect parity,"
    $lines += "hiding exactly the drift this gate exists to catch."
    throw ($lines -join [Environment]::NewLine)
}

# What the published Linux leg LAUNCHES, given the binary Find-PublishedLinuxRunner
# returned. A .deb install runs usr/bin/qontinui-runner directly -- that is what
# the package puts on a user's PATH. An AppImage never runs its inner binary
# directly: a user's launch goes through AppRun, which applies the bundle's own
# runtime environment (APPDIR, and the GTK hook's GDK/GTK/XDG_DATA_DIRS/pixbuf
# settings) before exec'ing the binary. Booting the inner binary bare skips that
# environment -- a configuration no user runs -- so for an --appimage-extract
# prefix (an AppDir holding AppRun beside usr/) the launch path is AppRun.
#
# AppRun is returned by its own path, NOT its realpath: AppRun resolves APPDIR from
# where it was invoked, and a symlinked AppRun (-> usr/bin/qontinui-runner) run by
# its target's name would lose that. The realpath containment check catches only a
# SYMLINKED AppRun leading out of the AppDir; a script AppRun can exec anything, so
# what proves the AppDir is the release's is Confirm-PublishedLinuxAssetHash on the
# AppImage, as for the binary. An AppDir named squashfs-root with no AppRun is not
# an unpacked AppImage and is refused rather than booted bare.
function Get-PublishedLinuxLaunchPath {
    param([string]$BinaryPath)

    if ([string]::IsNullOrWhiteSpace($BinaryPath)) {
        throw "Get-PublishedLinuxLaunchPath: -BinaryPath is empty ('$BinaryPath')."
    }
    # <AppDir>/usr/bin/qontinui-runner -> <AppDir>
    $appDir = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $BinaryPath))
    $appRun = Join-Path $appDir 'AppRun'
    if (-not (Test-Path -LiteralPath $appRun)) {
        if ((Split-Path -Leaf $appDir) -ceq 'squashfs-root') {
            throw ("Refusing '$BinaryPath': it sits in an --appimage-extract tree ('$appDir') with no AppRun. " +
                   "An AppImage is launched through AppRun, which points the loader at the bundled " +
                   "libraries; booting the inner binary bare would measure a configuration no user runs.")
        }
        return $BinaryPath
    }
    if (-not (Test-Path -LiteralPath $appRun -PathType Leaf)) {
        throw "Refusing '$appRun': the AppImage's AppRun is not a file."
    }
    $realDir = (Resolve-LinuxRealPath -Path $appDir).TrimEnd('/')
    $realRun = Resolve-LinuxRealPath -Path $appRun
    if (-not $realRun.StartsWith($realDir + '/', [StringComparison]::Ordinal)) {
        throw ("Refusing '$appRun': it resolves to '$realRun', outside its AppDir '$realDir'. AppRun " +
               "must be the AppImage's own, not a link to something the binary guard never checked.")
    }
    return $appRun
}
