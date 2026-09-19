#!/usr/bin/env pwsh
# Unit test for scripts/lib/installed-runner.ps1, the one locator for the
# INSTALLED runner exe. Pins the property the parity gate rests on -- the
# locator never yields a dev binary -- now that the installed exe and the dev
# exe share a leaf name (qontinui-runner.exe; Tauri 2 does not rename the
# cargo binary to productName, measured on the v1.0.11 installer 2026-09-19),
# so the property lives on the PARENT DIRECTORY. Runs under pwsh 7 or 5.1.
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
    try { $null = Find-InstalledRunnerExe -InstallRoot $root } catch { $msg = $_.Exception.Message }
    Check ($msg -ne '') 'no main exe: throws rather than returning'
    Check ($msg.Contains("EXISTS")) 'no main exe: names the install dir that exists' $msg
    Check ($msg.Contains('uninstall.exe')) 'no main exe: lists what the install dir holds' $msg
    $probed = @(($msg -split "`n") | Where-Object { $_ -match '^\s+\S' -and $_ -notmatch '^\s+\(' -and $_ -notmatch 'uninstall' })
    Check (@($probed | Where-Object { $_ -match '(?i)[\\/]target[\\/]' }).Count -eq 0) 'no main exe: no probed candidate lies under a build dir' ($probed -join ' | ')

    # 2. The exe directly under the product-named directory is found.
    Set-Content -LiteralPath $exe -Value 'x'
    $found = Find-InstalledRunnerExe -InstallRoot $root
    Check ((Split-Path -Leaf $found) -eq 'qontinui-runner.exe') 'finds qontinui-runner.exe' $found
    Check ((Split-Path -Leaf (Split-Path -Parent $found)) -eq 'Qontinui Runner') 'found exe sits under the product directory' $found
    $foundDirect = Find-InstalledRunnerExe -InstallRoot $installDir
    Check ($foundDirect -eq $found) '-InstallRoot may name the install directory itself' $foundDirect
    $foundExe = Find-InstalledRunnerExe -InstallRoot $exe
    Check ($foundExe -eq $found) '-InstallRoot may name the exe itself' $foundExe

    # 3. The dev binary is refused by both guards, whichever fires first.
    $dev = Join-Path (Join-Path (Join-Path $root 'target') 'debug') 'qontinui-runner.exe'
    $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dev)
    Set-Content -LiteralPath $dev -Value 'x'
    $refused = ''
    try { $null = Find-InstalledRunnerExe -InstallRoot $dev } catch { $refused = $_.Exception.Message }
    Check ($refused -ne '' -and $refused.StartsWith('Refusing')) 'a -InstallRoot pointing at target/debug is refused' $refused
    $refused = ''
    try { Assert-InstalledRunnerExe -Path (Join-Path $root 'qontinui-runner.exe') } catch { $refused = $_.Exception.Message }
    Check ($refused.Contains("'Qontinui Runner'")) 'an exe outside a Qontinui Runner directory is refused' $refused
    $refused = ''
    try { Assert-InstalledRunnerExe -Path (Join-Path $installDir 'Qontinui Runner.exe') } catch { $refused = $_.Exception.Message }
    Check ($refused.Contains("'qontinui-runner.exe'")) 'the Tauri-1 name is refused, not found' $refused
} finally {
    foreach ($name in $savedEnv.Keys) { [System.Environment]::SetEnvironmentVariable($name, $savedEnv[$name], 'Process') }
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

if ($fail -gt 0) { Write-Host "test-installed-runner: $fail failure(s)"; exit 1 }
Write-Host 'test-installed-runner: all passed'
exit 0
