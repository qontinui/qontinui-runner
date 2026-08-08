# Regression test for regen_exclude.ps1's -Check EXIT-CODE CONTRACT, and for
# the pilot carve-out, using a stubbed psql. No database.
#
# WHY THIS FILE EXISTS. `.github/workflows/atlas-exclude-fresh.yml` decides
# whether to SELF-HEAL -- i.e. whether to auto-author a commit rewriting
# atlas/exclude.txt -- purely from this script's exit code:
#
#   2 => drift, the regenerated list is trustworthy, open/refresh the PR
#   1 => failure, establish nothing, never self-heal
#
# Get that backwards on the failure path and CI auto-proposes an exclude list
# built from a bad read, which is the data-loss footgun the whole atlas/ guard
# exists to prevent. That decision used to be made by grepping this script's
# human-readable prose for "is STALE relative to a fresh regen"; it is now a
# number, and this file is what keeps the number honest. An untested numeric
# coupling would be no better than the untested prose one it replaced.
#
# Run locally:
#   pwsh atlas/scripts/tests/test_regen_exit_contract.ps1
# (Windows PowerShell 5.1 works too, like the script under test.)

$ErrorActionPreference = "Stop"

$testsDir = $PSScriptRoot
$scriptsDir = Split-Path -Parent $testsDir
$atlasDir = Split-Path -Parent $scriptsDir
$isWin = ($env:OS -eq "Windows_NT")
$exe = (Get-Process -Id $PID).Path

# --- Work on a COPY of atlas/. The script under test resolves its output as
# --- "$PSScriptRoot/../exclude.txt", so running it in-tree would rewrite the
# --- repo's real exclude list.
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("atlas-exit-contract-" + [System.Guid]::NewGuid().ToString("N").Substring(0, 8))
$workAtlas = Join-Path $work "atlas"
New-Item -ItemType Directory -Path $work -Force | Out-Null
Copy-Item -Path $atlasDir -Destination $workAtlas -Recurse -Force
$script = Join-Path (Join-Path $workAtlas "scripts") "regen_exclude.ps1"
$excludeTxt = Join-Path $workAtlas "exclude.txt"

# --- Put a fake `psql` first on PATH. -----------------------------------------
# The launcher must receive the script's multi-line here-string SQL intact.
# On Windows that rules out a .cmd shim (cmd.exe mangles embedded newlines), so
# a .ps1 is used and .PS1 is added to PATHEXT; on Linux a plain exec shim works.
$binDir = Join-Path $work "bin"
New-Item -ItemType Directory -Path $binDir -Force | Out-Null
$stub = Join-Path $testsDir "psql_stub.py"
if ($isWin) {
    @"
`$out = & python "$stub" @args
`$rc = `$LASTEXITCODE
if (`$out) { `$out | ForEach-Object { `$_ } }
exit `$rc
"@ | Set-Content -LiteralPath (Join-Path $binDir "psql.ps1") -Encoding ASCII
    if ($env:PATHEXT -notlike "*.PS1*") { $env:PATHEXT = "$env:PATHEXT;.PS1" }
    $env:PATH = "$binDir;$env:PATH"
}
else {
    $shim = Join-Path $binDir "psql"
    "#!/usr/bin/env bash`nexec python3 `"$stub`" `"`$@`"`n" | Set-Content -LiteralPath $shim -Encoding ASCII
    & chmod +x $shim
    $env:PATH = "${binDir}:$env:PATH"
}

$failures = 0
function Assert-Equal {
    param([string]$Name, $Expected, $Actual)
    if ("$Expected" -eq "$Actual") { Write-Host ("  PASS  {0,-46} {1}" -f $Name, $Actual) }
    else { Write-Host ("  FAIL  {0,-46} expected '{1}', got '{2}'" -f $Name, $Expected, $Actual); $script:failures++ }
}

# -Container " " (blank), not "": invoking a .ps1 with -File silently DROPS an
# empty-string argument, so $Container would keep its `docker exec` default;
# a blank string still satisfies the script's IsNullOrWhiteSpace check and
# selects the native-psql path CI uses.
#
# -File, not -Command: only -File propagates the script's own exit code. Under
# -Command a script that `exit 2`s surfaces as 1 -- which would make this test
# agree with the contract for entirely the wrong reason.
function Invoke-Regen {
    param([string]$Mode, [switch]$CheckMode, [string]$OutPath)
    $env:PSQL_STUB_MODE = $Mode
    $a = @("-NoProfile", "-File", $script, "-Container", " ")
    if ($CheckMode) { $a += "-Check" }
    if ($OutPath) { $a += @("-Out", $OutPath) }
    # Windows PowerShell turns a redirected native command's stderr into an
    # ErrorRecord, which under the file-level `Stop` preference would abort the
    # run on exactly the scenario being tested (the DB-unreachable stub writes
    # to stderr, as psql does). The exit code is the assertion here, not the
    # stream, so relax the preference just around the child process.
    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try { & $exe @a *> $null }
    finally { $ErrorActionPreference = $prev }
    return $LASTEXITCODE
}

try {
    Write-Host "Pilot carve-out (stub schema includes the runner's self-healed tables):"
    Assert-Equal "regen against a healthy schema" 0 (Invoke-Regen -Mode "ok")
    $seeded = @(Get-Content -LiteralPath $excludeTxt)

    # The regression that motivated this: schema.hcl declares these as
    # Atlas-managed, so they must never appear in the --exclude list. The
    # historical hardcoded carve-out (regression_% + coordinator_shadow_decisions)
    # emitted spec_proposals and proposal_events here.
    $leaked = @($seeded | Where-Object { $_ -match 'spec_proposals|proposal_events|^orchestration\.|regression_|coordinator_shadow_decisions' })
    Assert-Equal "no pilot table leaks into exclude.txt" 0 $leaked.Count
    # ...and non-pilot objects must still be excluded, or the list is useless.
    foreach ($expected in @("project.alembic_version", "project.workflows", "coord.agents", "project.workflow_status")) {
        Assert-Equal "non-pilot '$expected' excluded" $true ($seeded -contains $expected)
    }

    Write-Host ""
    Write-Host "-Check exit-code contract:"
    Assert-Equal "fresh list => 0" 0 (Invoke-Regen -Mode "ok" -CheckMode)

    $outFile = Join-Path $work "exclude.fresh.txt"
    ($seeded + "project.zzz_table_added_by_a_web_migration") | Set-Content -LiteralPath $excludeTxt
    Assert-Equal "drift => 2" 2 (Invoke-Regen -Mode "ok" -CheckMode -OutPath $outFile)
    Assert-Equal "drift writes -Out" $true (Test-Path $outFile)

    # The two paths that must NEVER be mistaken for drift: if either returned 2,
    # CI would auto-author a commit built from a failed read.
    Assert-Equal "DB unreachable => 1, never 2" 1 (Invoke-Regen -Mode "fail" -CheckMode)
    Assert-Equal "every query empty => 1, never 2" 1 (Invoke-Regen -Mode "empty" -CheckMode)

    $seeded | Set-Content -LiteralPath $excludeTxt
    Assert-Equal "restored fresh list => 0" 0 (Invoke-Regen -Mode "ok" -CheckMode)
}
finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ""
if ($failures -gt 0) {
    Write-Host "::error::regen_exclude.ps1 exit-contract test: $failures failure(s)."
    exit 1
}
Write-Host "regen_exclude.ps1 exit-contract test: all assertions passed."
exit 0
