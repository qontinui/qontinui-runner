#!/usr/bin/env pwsh
# Unit test for scripts/lib/installed-runner.ps1, the one locator for the
# INSTALLED runner exe. Pins the property the parity gate rests on -- the
# locator never yields a dev binary -- now that the installed exe and the dev
# exe share a leaf name (qontinui-runner.exe; Tauri 2 does not rename the
# cargo binary to productName, measured on the v1.0.11 installer 2026-09-19),
# so the property lives on the PARENT DIRECTORY. Runs under pwsh 7 or 5.1.
# The Windows cases pass -Platform windows so they also run on a Linux box; the
# Linux cases (Phase 6 of plan
# 2026-09-20-published-runner-parity-count-comes-from-a-run-not-from-reports)
# need GNU realpath and run only where $IsLinux is true.
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
. (Join-Path (Join-Path $here '..') (Join-Path 'lib' 'installed-runner.ps1'))
$fail = 0
function Check([bool] $Ok, [string] $Name, [string] $Detail = '') {
    if ($Ok) { Write-Host "  PASS  $Name" } else { Write-Host "  FAIL  $Name  $Detail"; $script:fail++ }
}

$root = Join-Path ([System.IO.Path]::GetTempPath()) ("installed-runner-test-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
$installDir = Join-Path $root 'Qontinui Runner'
$exe = Join-Path $installDir 'qontinui-runner.exe'
$null = New-Item -ItemType Directory -Force -Path $installDir
# The locator always falls through to the three machine bases, so on a box
# where the product IS installed the "no main exe" case would find the real
# one. Point every base at empty throwaway dirs for the test's duration.
$savedEnv = @{}
foreach ($name in @('LOCALAPPDATA', 'ProgramFiles', 'ProgramFiles(x86)')) {
    $savedEnv[$name] = [System.Environment]::GetEnvironmentVariable($name, 'Process')
    $scoped = Join-Path $root ("base-" + ($name -replace '[()]', ''))
    $null = New-Item -ItemType Directory -Force -Path $scoped
    [System.Environment]::SetEnvironmentVariable($name, $scoped, 'Process')
}
try {
    # 1. The install dir exists but holds no main exe: refuse, and say what is there.
    Set-Content -LiteralPath (Join-Path $installDir 'uninstall.exe') -Value 'x'
    $msg = ''
    try { $null = Find-InstalledRunnerExe -Platform windows -InstallRoot $root } catch { $msg = $_.Exception.Message }
    Check ($msg -ne '') 'no main exe: throws rather than returning'
    Check ($msg.Contains("EXISTS")) 'no main exe: names the install dir that exists' $msg
    Check ($msg.Contains('uninstall.exe')) 'no main exe: lists what the install dir holds' $msg
    $probed = @(($msg -split "`n") | Where-Object { $_ -match '^\s+\S' -and $_ -notmatch '^\s+\(' -and $_ -notmatch 'uninstall' })
    Check (@($probed | Where-Object { $_ -match '(?i)[\\/]target[\\/]' }).Count -eq 0) 'no main exe: no probed candidate lies under a build dir' ($probed -join ' | ')

    # 2. The exe directly under the product-named directory is found.
    Set-Content -LiteralPath $exe -Value 'x'
    $found = Find-InstalledRunnerExe -Platform windows -InstallRoot $root
    Check ((Split-Path -Leaf $found) -eq 'qontinui-runner.exe') 'finds qontinui-runner.exe' $found
    Check ((Split-Path -Leaf (Split-Path -Parent $found)) -eq 'Qontinui Runner') 'found exe sits under the product directory' $found
    # Both real callers omit -Platform; on a Windows host the default must be the Windows branch.
    if ($IsLinux) {
        Write-Host '  SKIP  host default takes the Windows branch (a Linux host defaults to linux; see 5.)'
    } else {
        $foundDefault = Find-InstalledRunnerExe -InstallRoot $root
        Check ($foundDefault -eq $found) 'with no -Platform (how both callers invoke it) the Windows branch is taken' $foundDefault
    }
    $foundDirect = Find-InstalledRunnerExe -Platform windows -InstallRoot $installDir
    Check ($foundDirect -eq $found) '-InstallRoot may name the install directory itself' $foundDirect
    $foundExe = Find-InstalledRunnerExe -Platform windows -InstallRoot $exe
    Check ($foundExe -eq $found) '-InstallRoot may name the exe itself' $foundExe

    # 3. The dev binary is refused by both guards, whichever fires first.
    $dev = Join-Path (Join-Path (Join-Path $root 'target') 'debug') 'qontinui-runner.exe'
    $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dev)
    Set-Content -LiteralPath $dev -Value 'x'
    $refused = ''
    try { $null = Find-InstalledRunnerExe -Platform windows -InstallRoot $dev } catch { $refused = $_.Exception.Message }
    Check ($refused -ne '' -and $refused.StartsWith('Refusing')) 'a -InstallRoot pointing at target/debug is refused' $refused
    $refused = ''
    try { Assert-InstalledRunnerExe -Path (Join-Path $root 'qontinui-runner.exe') } catch { $refused = $_.Exception.Message }
    Check ($refused.Contains("'Qontinui Runner'")) 'an exe outside a Qontinui Runner directory is refused' $refused
    $refused = ''
    try { Assert-InstalledRunnerExe -Path (Join-Path $installDir 'Qontinui Runner.exe') } catch { $refused = $_.Exception.Message }
    Check ($refused.Contains("'qontinui-runner.exe'")) 'the Tauri-1 name is refused, not found' $refused

    # 4. Linux asset selection and the typed-unknown vocabulary (pure; any host).
    $pick = Select-PublishedLinuxAsset -Version '1.0.11' -AssetNames @('checksums-linux-x64.txt', 'Qontinui.Runner_1.0.11_amd64.AppImage', 'Qontinui.Runner_1.0.11_amd64.deb')
    Check ($pick.Kind -eq 'deb' -and $pick.Name -eq 'Qontinui.Runner_1.0.11_amd64.deb') 'linux asset: the .deb is preferred' ($pick.Name)
    $pick = Select-PublishedLinuxAsset -Version '1.0.11' -AssetNames @('Qontinui.Runner_1.0.11_amd64.AppImage', 'Qontinui.Runner_1.0.10_amd64.deb')
    Check ($pick.Kind -eq 'appimage') 'linux asset: falls back to the AppImage, never another version''s .deb' ($pick.Name)
    $msg = ''
    # v1.0.7's real asset list (gh release view, 2026-10-04): no Linux leg shipped.
    $v107 = @('checksums-macos-arm64.txt', 'checksums-windows-x64.txt', 'latest.json', 'Qontinui.Runner_1.0.7_aarch64.dmg', 'Qontinui.Runner_1.0.7_x64-setup.exe', 'Qontinui.Runner_1.0.7_x64-setup.exe.sig')
    try { $null = Select-PublishedLinuxAsset -Version '1.0.7' -AssetNames $v107 } catch { $msg = $_.Exception.Message }
    Check ((Get-LinuxUnknownReason $msg) -eq 'no_linux_asset_on_release') 'linux asset: none on the release is unknown(no_linux_asset_on_release), not a pick' $msg
    Check ($null -eq (Get-LinuxUnknownReason 'Refusing x')) 'a refusal is not read as an unknown'
    Check ($null -eq (Get-LinuxUnknownReason 'unknown(made_up): x')) 'a reason outside the enumerated set is not read as an unknown'
    $msg = ''
    try { $null = New-LinuxUnknown 'zero' 'x' } catch { $msg = $_.Exception.Message }
    Check ($msg.StartsWith('internal:')) 'minting a reason outside the enumerated set throws' $msg
    $pick = @{ Name = '' }; $msg = ''
    try { $pick = Select-PublishedLinuxAsset -Version 'v1.0.11' -AssetNames @('Qontinui.Runner_1.0.11_amd64.deb') } catch { $msg = $_.Exception.Message }
    Check ($pick.Name -eq 'Qontinui.Runner_1.0.11_amd64.deb') 'linux asset: a tag-style v1.0.11 matches, not a false no_linux_asset_on_release' $msg
    foreach ($emptyVersion in @('', 'v', '  ')) {
        $msg = ''
        try { $null = Select-PublishedLinuxAsset -Version $emptyVersion -AssetNames @('Qontinui.Runner_1.0.11_amd64.deb') } catch { $msg = $_.Exception.Message }
        Check ($msg -ne '' -and $null -eq (Get-LinuxUnknownReason $msg)) "linux asset: version '$emptyVersion' is refused as empty, not read as an unknown" $msg
    }

    $msg = ''
    try { $null = Select-PublishedLinuxAsset -Version '1.0.11' -AssetNames $null } catch { $msg = $_.Exception.Message }
    Check ($msg -ne '' -and $null -eq (Get-LinuxUnknownReason $msg)) 'linux asset: a $null asset list is refused, not read as no_linux_asset_on_release' $msg
    $msg = ''
    try { $null = Select-PublishedLinuxAsset -Version '1.0.11' -AssetNames @() } catch { $msg = $_.Exception.Message }
    Check ((Get-LinuxUnknownReason $msg) -eq 'no_linux_asset_on_release') 'linux asset: an EMPTY asset list is still unknown(no_linux_asset_on_release)' $msg

    # 4b. Provenance: the downloaded asset is matched to the release checksums by
    # sha256. The checksums file names the pre-upload path, as v1.0.11's does.
    $assetDir = Join-Path $root 'asset'
    $null = New-Item -ItemType Directory -Force -Path $assetDir
    $asset = Join-Path $assetDir 'Qontinui.Runner_9.9.9_amd64.deb'
    $other = Join-Path $assetDir 'Qontinui.Runner_9.9.9_amd64.AppImage'
    Set-Content -LiteralPath $asset -Value 'published deb bytes'
    Set-Content -LiteralPath $other -Value 'published appimage bytes'
    $hAsset = (Get-FileHash -LiteralPath $asset -Algorithm SHA256).Hash.ToLowerInvariant()
    $hOther = (Get-FileHash -LiteralPath $other -Algorithm SHA256).Hash.ToLowerInvariant()
    $sums = Join-Path $assetDir 'checksums-linux-x64.txt'
    Set-Content -LiteralPath $sums -Value @("$hAsset  deb/Qontinui Runner_9.9.9_amd64.deb", "$hOther  appimage/Qontinui Runner_9.9.9_amd64.AppImage")
    $line = Confirm-PublishedLinuxAssetHash -AssetPath $asset -ChecksumsPath $sums
    Check ($line -eq "$hAsset  deb/Qontinui Runner_9.9.9_amd64.deb") 'provenance: matched by sha256 although the names differ; returns the matched line' $line
    $tampered = Join-Path $assetDir 'Qontinui Runner_9.9.9_amd64.deb'
    Set-Content -LiteralPath $tampered -Value 'a dev build repackaged'
    $msg = ''
    try { $null = Confirm-PublishedLinuxAssetHash -AssetPath $tampered -ChecksumsPath $sums } catch { $msg = $_.Exception.Message }
    Check ($msg.StartsWith('Refusing') -and $msg.Contains('matches no line') -and $msg.Contains('download is incomplete')) 'provenance: an asset whose hash matches no line is refused (and the message names an incomplete download), even with the checksums file''s own name' $msg
    $msg = ''
    try { $null = Confirm-PublishedLinuxAssetHash -AssetPath $asset -ChecksumsPath (Join-Path $assetDir 'absent.txt') } catch { $msg = $_.Exception.Message }
    Check ($msg.StartsWith('Refusing') -and $null -eq (Get-LinuxUnknownReason $msg)) 'provenance: a checksums file that is not on disk (a failed download) is refused, not an unknown' $msg
    foreach ($emptyPath in @('', '  ')) {
        $msg = ''
        try { $null = Confirm-PublishedLinuxAssetHash -AssetPath $asset -ChecksumsPath $emptyPath } catch { $msg = $_.Exception.Message }
        Check ($msg -ne '' -and -not $msg.StartsWith('Refusing') -and $null -eq (Get-LinuxUnknownReason $msg)) "provenance: -ChecksumsPath '$emptyPath' is a plain caller error" $msg
    }
    # Whether the release HAS checksums is decided from its asset list.
    $v1011 = @('checksums-linux-x64.txt', 'checksums-windows-x64.txt', 'Qontinui.Runner_1.0.11_amd64.deb')
    $msg = ''; $sumName = ''
    try { $sumName = Select-PublishedLinuxChecksums -AssetNames $v1011 } catch { $msg = $_.Exception.Message }
    Check ($sumName -eq 'checksums-linux-x64.txt') 'checksums: named from the asset list when the release carries it' $msg
    $msg = ''
    try { $null = Select-PublishedLinuxChecksums -AssetNames @('checksums-windows-x64.txt', 'Qontinui.Runner_1.0.11_amd64.deb') } catch { $msg = $_.Exception.Message }
    Check ((Get-LinuxUnknownReason $msg) -eq 'no_checksums_on_release') 'checksums: a release whose list lacks checksums-linux-x64.txt is unknown(no_checksums_on_release)' $msg
    $msg = ''
    try { $null = Select-PublishedLinuxChecksums -AssetNames $null } catch { $msg = $_.Exception.Message }
    Check ($msg -ne '' -and $null -eq (Get-LinuxUnknownReason $msg)) 'checksums: a $null asset list is refused, not read as no_checksums_on_release' $msg
    $msg = ''
    try { $null = Confirm-PublishedLinuxAssetHash -AssetPath (Join-Path $assetDir 'absent.deb') -ChecksumsPath $sums } catch { $msg = $_.Exception.Message }
    Check ($msg.StartsWith('Refusing')) 'provenance: a missing asset is refused, not an unknown' $msg
    Check ($msg -ne '' -and $null -eq (Get-LinuxUnknownReason $msg)) 'provenance: a missing asset does not read as an unknown' $msg

    # 5. Linux locator, on fixtures shaped like the measured v1.0.11 unpacks.
    if ($IsLinux) {
        $linuxRoot = Join-Path $root 'linux'
        $debPrefix = Join-Path $linuxRoot 'deb-prefix'
        $debBin = Join-Path (Join-Path (Join-Path $debPrefix 'usr') 'bin') 'qontinui-runner'
        $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $debBin)
        $null = New-Item -ItemType Directory -Force -Path (Join-Path (Join-Path $debPrefix 'usr') (Join-Path 'lib' 'Qontinui Runner'))
        Set-Content -LiteralPath $debBin -Value 'x'
        Set-Content -LiteralPath (Join-Path (Split-Path -Parent $debBin) 'qontinui-pr') -Value 'x'
        $found = Find-InstalledRunnerExe -InstallRoot $debPrefix
        Check ($found -eq (& realpath -- $debBin)) 'linux: the host default takes the Linux branch and finds <prefix>/usr/bin/qontinui-runner (dpkg -x)' $found
        $found = Find-InstalledRunnerExe -Platform linux -InstallRoot $debBin
        Check ($found -eq (& realpath -- $debBin)) 'linux: -InstallRoot may name the binary itself' $found

        $aiPrefix = Join-Path $linuxRoot 'appimage-prefix'
        $aiBin = Join-Path (Join-Path (Join-Path (Join-Path $aiPrefix 'squashfs-root') 'usr') 'bin') 'qontinui-runner'
        $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $aiBin)
        Set-Content -LiteralPath $aiBin -Value 'x'
        Set-Content -LiteralPath (Join-Path (Join-Path $aiPrefix 'squashfs-root') 'AppRun') -Value 'x'
        $found = Find-InstalledRunnerExe -Platform linux -InstallRoot $aiPrefix
        Check ($found -eq (& realpath -- $aiBin)) 'linux: finds <prefix>/squashfs-root/usr/bin/qontinui-runner (--appimage-extract)' $found

        # An empty prefix: typed unknown, naming every path probed.
        $empty = Join-Path $linuxRoot 'empty-prefix'
        $null = New-Item -ItemType Directory -Force -Path $empty
        $msg = ''
        try { $null = Find-InstalledRunnerExe -Platform linux -InstallRoot $empty } catch { $msg = $_.Exception.Message }
        Check ((Get-LinuxUnknownReason $msg) -eq 'binary_absent') 'linux: an empty prefix is unknown(binary_absent)' $msg
        $p1 = Join-Path (Join-Path (Join-Path $empty 'usr') 'bin') 'qontinui-runner'
        $p2 = Join-Path (Join-Path (Join-Path (Join-Path $empty 'squashfs-root') 'usr') 'bin') 'qontinui-runner'
        Check ($msg.Contains($p1) -and $msg.Contains($p2)) 'linux: an empty prefix names both probed paths' $msg
        Check ($msg.Contains('EMPTY')) 'linux: an empty prefix says it is empty' $msg
        $msg = ''
        try { $null = Find-InstalledRunnerExe -Platform linux -InstallRoot (Join-Path $linuxRoot 'never-unpacked') } catch { $msg = $_.Exception.Message }
        Check ((Get-LinuxUnknownReason $msg) -eq 'prefix_missing') 'linux: a prefix that does not exist is unknown(prefix_missing)' $msg
        $msg = ''
        try { $null = Find-InstalledRunnerExe -Platform linux -InstallRoot '' } catch { $msg = $_.Exception.Message }
        Check ((Get-LinuxUnknownReason $msg) -eq 'no_prefix_given') 'linux: no -InstallRoot is unknown(no_prefix_given), with no default to fall back to' $msg

        # The dev binary must be unreachable by every route a Linux run could take.
        $devDir = Join-Path (Join-Path $linuxRoot 'target') 'debug'
        $devBin = Join-Path $devDir 'qontinui-runner'
        $null = New-Item -ItemType Directory -Force -Path $devDir
        Set-Content -LiteralPath $devBin -Value 'x'
        $refused = ''
        try { $null = Find-InstalledRunnerExe -Platform linux -InstallRoot $devBin } catch { $refused = $_.Exception.Message }
        Check ($refused.StartsWith('Refusing')) 'linux: -InstallRoot naming target/debug/qontinui-runner is refused' $refused
        # A build-tree usr/bin (the AppDir tauri bundles from) is still a build.
        $appDir = Join-Path (Join-Path (Join-Path (Join-Path (Join-Path $linuxRoot 'target-agent') 'release') 'bundle') 'appimage') 'Qontinui Runner.AppDir'
        $appDirBin = Join-Path (Join-Path (Join-Path $appDir 'usr') 'bin') 'qontinui-runner'
        $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $appDirBin)
        Set-Content -LiteralPath $appDirBin -Value 'x'
        $refused = ''
        try { $null = Find-InstalledRunnerExe -Platform linux -InstallRoot $appDir } catch { $refused = $_.Exception.Message }
        Check ($refused.StartsWith('Refusing') -and $refused.Contains('cargo build directory')) 'linux: a usr/bin inside a target-*/ bundle dir is refused' $refused
        # A symlink planted in a well-shaped prefix is judged by where it points.
        $linkPrefix = Join-Path $linuxRoot 'link-prefix'
        $linkBin = Join-Path (Join-Path (Join-Path $linkPrefix 'usr') 'bin') 'qontinui-runner'
        $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $linkBin)
        $null = New-Item -ItemType SymbolicLink -Path $linkBin -Target $devBin
        $refused = ''
        try { $null = Find-InstalledRunnerExe -Platform linux -InstallRoot $linkPrefix } catch { $refused = $_.Exception.Message }
        Check ($refused.StartsWith('Refusing')) 'linux: a prefix whose usr/bin/qontinui-runner symlinks to target/debug is refused' $refused
        # A prefix inside this repo's checkout is refused even with the right shape.
        $inRepo = Join-Path (Join-Path (Join-Path (Join-Path $GuardRepoRoot 'unpacked') 'usr') 'bin') 'qontinui-runner'
        $refused = ''
        try { Assert-PublishedLinuxRunner -Path $inRepo } catch { $refused = $_.Exception.Message }
        Check ($refused.Contains("this repo's checkout")) 'linux: a usr/bin inside the repo checkout is refused' $refused
        $refused = ''
        try { Assert-PublishedLinuxRunner -Path (Join-Path (Split-Path -Parent $debBin) 'qontinui-runner.exe') } catch { $refused = $_.Exception.Message }
        Check ($refused.Contains("'qontinui-runner'")) 'linux: the Windows leaf name is refused, not found' $refused

        # A prefix DIRECTORY named qontinui-runner (dpkg -x into /tmp/qontinui-runner) is a prefix, not the binary.
        $namedPrefix = Join-Path $linuxRoot 'qontinui-runner'
        $namedBin = Join-Path (Join-Path (Join-Path $namedPrefix 'usr') 'bin') 'qontinui-runner'
        $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $namedBin)
        Set-Content -LiteralPath $namedBin -Value 'x'
        $found = ''
        try { $found = Find-InstalledRunnerExe -Platform linux -InstallRoot $namedPrefix } catch { $found = $_.Exception.Message }
        Check ($found -eq (& realpath -- $namedBin)) 'linux: a prefix directory literally named qontinui-runner is probed, not taken as the binary' $found

        # The returned path is the canonical one the guard checked, not the alias it was reached by.
        $alias = Join-Path $linuxRoot 'alias-prefix'
        $null = New-Item -ItemType SymbolicLink -Path $alias -Target $debPrefix
        $found = Find-InstalledRunnerExe -Platform linux -InstallRoot $alias
        Check ($found -eq (& realpath -- $debBin) -and -not $found.Contains('alias-prefix')) 'linux: returns the realpath that was checked' $found

        # THE LIMIT, pinned rather than implied: the path rule cannot see provenance.
        # A dev binary hard-linked or copied into a well-shaped prefix is FOUND by the
        # locator alone; only Confirm-PublishedLinuxAssetHash on the asset catches it.
        $hlPrefix = Join-Path $linuxRoot 'hardlink-prefix'
        $hlBin = Join-Path (Join-Path (Join-Path $hlPrefix 'usr') 'bin') 'qontinui-runner'
        $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $hlBin)
        $null = New-Item -ItemType HardLink -Path $hlBin -Target $devBin
        $found = ''
        try { $found = Find-InstalledRunnerExe -Platform linux -InstallRoot $hlPrefix } catch { $found = $_.Exception.Message }
        Check ($found -eq (& realpath -- $hlBin)) 'linux LIMIT: a hard-linked dev binary in a well-shaped prefix is FOUND (the hash check is the provenance)' $found
        $cpPrefix = Join-Path $linuxRoot 'copy-prefix'
        $cpBin = Join-Path (Join-Path (Join-Path $cpPrefix 'usr') 'bin') 'qontinui-runner'
        $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $cpBin)
        Copy-Item -LiteralPath $devBin -Destination $cpBin
        $found = ''
        try { $found = Find-InstalledRunnerExe -Platform linux -InstallRoot $cpPrefix } catch { $found = $_.Exception.Message }
        Check ($found -eq (& realpath -- $cpBin)) 'linux LIMIT: a copied dev binary in a well-shaped prefix is FOUND (the hash check is the provenance)' $found

        # A missing or non-GNU realpath is an environment failure, never a refusal.
        # A stand-in BSD/busybox realpath: a real executable on PATH that rejects -m.
        $fakeTools = Join-Path $linuxRoot 'fake-tools'
        $null = New-Item -ItemType Directory -Force -Path $fakeTools
        $fakeRealpath = Join-Path $fakeTools 'realpath'
        Set-Content -LiteralPath $fakeRealpath -Value @('#!/bin/sh', 'echo "realpath: illegal option -- m" >&2', 'exit 1')
        & chmod +x -- $fakeRealpath
        $savedPath = $env:PATH
        $env:PATH = $fakeTools + [System.IO.Path]::PathSeparator + $savedPath
        $msg = ''
        try { $null = Find-InstalledRunnerExe -Platform linux -InstallRoot $debPrefix } catch { $msg = $_.Exception.Message }
        $env:PATH = $savedPath
        Check ($msg.StartsWith('environment:') -and -not $msg.Contains('Refusing')) 'linux: a realpath without -m is an environment: failure, not a refusal' $msg
        $env:PATH = (Join-Path $linuxRoot 'no-tools')
        $msg = ''
        try { $null = Find-InstalledRunnerExe -Platform linux -InstallRoot $debPrefix } catch { $msg = $_.Exception.Message }
        $env:PATH = $savedPath
        Check ($msg.StartsWith('environment:') -and $msg.Contains('no realpath')) 'linux: no realpath on PATH is an environment: failure, not CommandNotFound' $msg
    } else {
        Write-Host '  SKIP  linux locator fixtures (not a Linux host; they need GNU realpath)'
    }
} finally {
    foreach ($name in $savedEnv.Keys) { [System.Environment]::SetEnvironmentVariable($name, $savedEnv[$name], 'Process') }
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

if ($fail -gt 0) { Write-Host "test-installed-runner: $fail failure(s)"; exit 1 }
Write-Host 'test-installed-runner: all passed'
exit 0
