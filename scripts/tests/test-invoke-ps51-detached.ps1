#!/usr/bin/env pwsh
# Unit test for scripts/lib/invoke-ps51-detached.ps1's command-line assembly:
# the part of that helper that can be wrong on any box. The launch itself
# (ShellExecute, the bounded wait, the survivor listing) needs Windows and is
# exercised by published-parity.yml on every run. Runs under pwsh 7 or 5.1.
#
# The helper is invoked the way the workflow invokes it -- `pwsh -File`, a
# separate host process -- and NOT in-process with `&`. The two bind
# arguments differently: the in-process call accepts a `--` separator that
# `-File` rejects ("parameter name '' is ambiguous"), so an in-process test
# passed on 2026-09-19 while the workflow's own shape failed at binding.
# 'Continue', not 'Stop': the helper is a NATIVE call here (a second pwsh
# host), and under Windows PowerShell 5.1 a native command's stderr captured
# with 2>&1 arrives as ErrorRecords, which 'Stop' turns into a terminating
# NativeCommandError -- measured on the refusal case (run 35428993046), whose
# whole point is that the helper writes an error. Outcomes are asserted on
# $LASTEXITCODE and the captured text, never on the absence of an error.
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$helper = Join-Path (Join-Path $here '..') (Join-Path 'lib' 'invoke-ps51-detached.ps1')
$fail = 0

function Assert-Contains([string] $Haystack, [string] $Needle, [string] $Name) {
    if ($Haystack.Contains($Needle)) { Write-Host "  PASS  $Name" }
    else { Write-Host "  FAIL  $Name`n        expected: $Needle`n        got:      $Haystack"; $script:fail++ }
}

# The host the workflow's `shell: pwsh` steps run the helper under.
$pwshExe = 'pwsh'

$env:INVOKE_PS51_DRY_RUN = '1'
try {
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("ps51-test-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
    $null = New-Item -ItemType Directory -Force -Path $tmp
    $target = Join-Path $tmp 'with space.ps1'
    Set-Content -LiteralPath $target -Value 'exit 0'

    Push-Location $tmp
    try {
        $line = & $pwshExe -NoProfile -File $helper -ScriptPath $target -LogPath 'out/run.log' -TimeoutSec 5 `
            -Profile ci -SdkTypesPath '../a b/types.ts' -Annotate 2>&1
        $rc = $LASTEXITCODE
    } finally { Pop-Location }
    $line = ($line | Out-String).Trim()

    if ($rc -eq 0) { Write-Host '  PASS  pwsh -File binds the workflow argument shape (exit 0)' }
    else { Write-Host "  FAIL  pwsh -File binds the workflow argument shape (exit $rc)`n        got: $line"; $fail++ }
    Assert-Contains $line 'powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' 'launches the 5.1 host with -File'
    Assert-Contains $line "with space.ps1`"" 'script path is double-quoted'
    Assert-Contains $line '"-Profile" "ci" "-SdkTypesPath" "../a b/types.ts" "-Annotate"' 'every unnamed argument is forwarded, quoted, in order'
    Assert-Contains $line ((Join-Path 'out' 'run.log') + '" 2>&1') 'stdout and stderr go to the log file'
    if (Test-Path -LiteralPath (Join-Path $tmp 'out')) { Write-Host '  PASS  log directory is created before the launch' }
    else { Write-Host '  FAIL  log directory is created before the launch'; $fail++ }

    $null = & $pwshExe -NoProfile -File $helper -ScriptPath $target -LogPath 'x.log' 'has"quote' 2>&1
    if ($LASTEXITCODE -ne 0) { Write-Host '  PASS  an argument carrying a double quote is refused' }
    else { Write-Host '  FAIL  an argument carrying a double quote is refused'; $fail++ }
} finally {
    Remove-Item Env:INVOKE_PS51_DRY_RUN -ErrorAction SilentlyContinue
    if ($tmp) { Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue }
}

if ($fail -gt 0) { Write-Host "test-invoke-ps51-detached: $fail failure(s)"; exit 1 }
Write-Host 'test-invoke-ps51-detached: all passed'
exit 0
