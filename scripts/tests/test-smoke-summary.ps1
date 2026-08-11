#!/usr/bin/env pwsh
# test-smoke-summary.ps1
#
# Pins the contract-smoke summary invariants (scripts/lib/smoke-summary.ps1).
# No runner, no supervisor, no network -- pure tally arithmetic, runs in ~1s.
#
# WHAT THIS EXISTS TO CATCH
# -------------------------
# The smoke's summary line once read
#     Smoke: 193 pass / 193 total, 8 skip
#     SMOKE-COMPLETE exit=1 pass=193 fail= skip=8
# on a run with a genuinely failing probe (runner#1008, job 92967302466). The
# exit code was right; the line a human reads was not. A harness whose summary
# can disagree with its exit code is the same failure family as a test that
# skips into a vacuous green -- so the disagreement is now an assertion, and
# this file is the thing that keeps the assertion honest.
#
# RUN IT UNDER WINDOWS POWERSHELL 5.1, NOT pwsh 7
# -----------------------------------------------
# The original bug is interpreter-specific: PS 5.1's unified scalar `Count`
# adapter covers .NET-backed scalars ([string], [int], [hashtable] -> 1) but NOT
# `PSCustomObject`, where `.Count` is $null. The EXACTLY-ONE-row cases below are
# the ones that reproduce it, and they only reproduce under the interpreter the
# gate actually uses. CI therefore runs this with `powershell -File` on
# windows-latest, alongside the smoke step itself -- running it on pwsh 7 would
# green-light a regression that still breaks the real gate.
#
# Usage:
#   powershell -File scripts/tests/test-smoke-summary.ps1

$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "../lib/smoke-summary.ps1")

$failures = 0
$checks   = 0

function Row {
    param([string]$Status, [string]$Method = "GET", [string]$Path = "/x")
    return [PSCustomObject]@{ Status = $Status; Method = $Method; Path = $Path; Detail = "" }
}

function Assert-Equal {
    param([string]$Name, $Expected, $Actual)
    $script:checks++
    # "$null" stringifies to "" -- the exact rendering failure under test -- so
    # compare through an explicit marker rather than plain string equality.
    $e = if ($null -eq $Expected) { "<null>" } else { "$Expected" }
    $a = if ($null -eq $Actual)   { "<null>" } else { "$Actual" }
    if ($e -eq $a) {
        Write-Host ("  PASS  {0,-58} {1}" -f $Name, $a)
    } else {
        Write-Host ("  FAIL  {0,-58} expected '{1}', got '{2}'" -f $Name, $e, $a) -ForegroundColor Red
        $script:failures++
    }
}

function Assert-True {
    param([string]$Name, [bool]$Condition, [string]$Detail = "")
    $script:checks++
    if ($Condition) {
        Write-Host ("  PASS  {0,-58} {1}" -f $Name, $Detail)
    } else {
        Write-Host ("  FAIL  {0,-58} {1}" -f $Name, $Detail) -ForegroundColor Red
        $script:failures++
    }
}

Write-Host "contract-smoke summary invariants (interpreter: $($PSVersionTable.PSVersion))"
Write-Host ""

# ---------------------------------------------------------------------------
# 1. THE REGRESSION ITSELF: exactly one FAIL.
#    This is the arity that produced the false green. `fail` must render as 1
#    (not blank), and `total` must INCLUDE the failure.
# ---------------------------------------------------------------------------
Write-Host "exactly-one-FAIL (the runner#1008 regression):"
$rows = @()
1..193 | ForEach-Object { $rows += Row "PASS" }
$rows += Row "FAIL" "POST" "/debug/highlight/:id"
1..8   | ForEach-Object { $rows += Row "SKIP" }
$s = Get-SmokeSummary -Results $rows -ExitCode 1
Assert-Equal "fail is 1, not blank"                      1   $s.Fail
Assert-Equal "pass is 193"                               193 $s.Pass
Assert-Equal "skip is 8"                                 8   $s.Skip
Assert-Equal "total INCLUDES the failure (194, not 193)" 194 $s.Total
Assert-Equal "exit code preserved"                       1   $s.ExitCode
Assert-Equal "no invariant violations"                   0   $s.Violations.Count
Assert-Equal "summary line"  "Smoke: 193 pass / 194 total, 8 skip"          $s.SummaryLine
Assert-Equal "sentinel line" "SMOKE-COMPLETE exit=1 pass=193 fail=1 skip=8" $s.SentinelLine
# The literal shape of the old lie, asserted as absent.
Assert-True  "sentinel never renders 'fail=' with no number" `
    ($s.SentinelLine -notmatch 'fail=(\s|$)') $s.SentinelLine

# ---------------------------------------------------------------------------
# 2. The other single-row arities collapse the same way. PASS and SKIP were
#    only ever correct in the field because there happened to be many of them.
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "exactly-one-of-each arity:"
$s = Get-SmokeSummary -Results @((Row "PASS"), (Row "FAIL"), (Row "SKIP")) -ExitCode 1
Assert-Equal "single PASS counts as 1"      1 $s.Pass
Assert-Equal "single FAIL counts as 1"      1 $s.Fail
Assert-Equal "single SKIP counts as 1"      1 $s.Skip
Assert-Equal "total is 2 (pass+fail)"       2 $s.Total
Assert-Equal "no invariant violations"      0 $s.Violations.Count

# ---------------------------------------------------------------------------
# 3. Clean run: fail must render the NUMBER 0, never an empty string.
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "all-pass run:"
$rows = @()
1..5 | ForEach-Object { $rows += Row "PASS" }
$rows += Row "SKIP"
$s = Get-SmokeSummary -Results $rows -ExitCode 0
Assert-Equal "fail renders 0"          0 $s.Fail
Assert-Equal "total is 5"              5 $s.Total
Assert-Equal "exit code stays 0"       0 $s.ExitCode
Assert-Equal "no invariant violations" 0 $s.Violations.Count
Assert-Equal "sentinel line" "SMOKE-COMPLETE exit=0 pass=5 fail=0 skip=1" $s.SentinelLine

# ---------------------------------------------------------------------------
# 4. Empty result set. Zero rows must not crash and must not look like a pass
#    if the exit code says otherwise.
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "empty result set:"
$s = Get-SmokeSummary -Results @() -ExitCode 0
Assert-Equal "pass 0"                  0 $s.Pass
Assert-Equal "fail 0"                  0 $s.Fail
Assert-Equal "total 0"                 0 $s.Total
Assert-Equal "no invariant violations" 0 $s.Violations.Count
$s = Get-SmokeSummary -Results $null -ExitCode 0
Assert-Equal "null result set tolerated" 0 $s.Total

# ---------------------------------------------------------------------------
# 5. I4 -- the summary may not contradict the exit code, in EITHER direction.
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "I4: exit code <-> fail count biconditional:"

# 5a. Failed exit, zero recorded failures. This is the shape of the original
#     defect generalised: a probe that sets $exitCode without Record'ing a FAIL.
$s = Get-SmokeSummary -Results @((Row "PASS"), (Row "PASS")) -ExitCode 1
Assert-True  "exit=1 with fail=0 is flagged" ($s.Violations.Count -ge 1) ($s.Violations -join '; ')
Assert-Equal "exit code NOT lowered to 0"    1 $s.ExitCode

# 5b. Clean exit with recorded failures: a Record "FAIL" that forgot to set the
#     exit code. Must NOT report success.
$s = Get-SmokeSummary -Results @((Row "PASS"), (Row "FAIL")) -ExitCode 0
Assert-True  "exit=0 with fail>0 is flagged" ($s.Violations.Count -ge 1) ($s.Violations -join '; ')
Assert-True  "exit code raised off 0"        ($s.ExitCode -ne 0) "exit=$($s.ExitCode)"
Assert-Equal "raised to 2 (harness unsound, not probe failure)" 2 $s.ExitCode

# ---------------------------------------------------------------------------
# 6. I3 -- a status outside PASS/FAIL/SKIP is counted by nothing and would
#    vanish from the summary. `Record` takes a free-form string, so this is
#    one typo away at any time.
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "I3: unclassified status:"
$s = Get-SmokeSummary -Results @((Row "PASS"), (Row "WARN" "GET" "/oops")) -ExitCode 0
Assert-True  "unclassified row is flagged" ($s.Violations.Count -ge 1) ($s.Violations -join '; ')
Assert-True  "violation names the status"  (($s.Violations -join ' ') -match "WARN") ""
Assert-Equal "exit code raised to 2"       2 $s.ExitCode

# ---------------------------------------------------------------------------
# 7. A violation must never launder a failing run into a passing one. This is
#    the one-way property the whole guard rail depends on.
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "violations only ever RAISE the exit code:"
$s = Get-SmokeSummary -Results @((Row "WARN")) -ExitCode 1
Assert-Equal "exit 1 + violation stays 1 (not 0, not 2)" 1 $s.ExitCode
$s = Get-SmokeSummary -Results @((Row "WARN")) -ExitCode 3
Assert-Equal "a non-zero code is never overwritten"      3 $s.ExitCode

# ---------------------------------------------------------------------------
# 8. I2 -- buckets account for every row, at realistic scale.
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "I2: buckets account for every recorded row:"
foreach ($n in @(0, 1, 2, 3, 17)) {
    $rows = @()
    1..40 | ForEach-Object { $rows += Row "PASS" }
    if ($n -gt 0) { 1..$n | ForEach-Object { $rows += Row "FAIL" } }
    1..6 | ForEach-Object { $rows += Row "SKIP" }
    $expectedExit = if ($n -gt 0) { 1 } else { 0 }
    $s = Get-SmokeSummary -Results $rows -ExitCode $expectedExit
    Assert-True ("fail=$n accounts for all $($rows.Count) rows") `
        (($s.Pass + $s.Fail + $s.Skip) -eq $rows.Count -and $s.Fail -eq $n -and $s.Violations.Count -eq 0) `
        ("pass=$($s.Pass) fail=$($s.Fail) skip=$($s.Skip) total=$($s.Total)")
}

# ---------------------------------------------------------------------------
# 9. The REAL container. contract-smoke.ps1 accumulates into a
#    System.Collections.Generic.List[Object], not a PowerShell array. Every case
#    above uses arrays, so this pins that the generic list enumerates the same
#    way through Where-Object -- including the one-element arity, where a
#    List[Object] is exactly as prone to scalar collapse as an array.
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "real container (System.Collections.Generic.List[Object]):"
$list = New-Object System.Collections.Generic.List[Object]
1..12 | ForEach-Object { $list.Add((Row "PASS")) }
$list.Add((Row "FAIL" "POST" "/debug/highlight/:id"))
1..2 | ForEach-Object { $list.Add((Row "SKIP")) }
$s = Get-SmokeSummary -Results $list -ExitCode 1
Assert-Equal "List[Object]: fail is 1"           1  $s.Fail
$emptyList = New-Object System.Collections.Generic.List[Object]
Assert-Equal "empty List[Object] binds and totals 0" 0 (Get-SmokeSummary -Results $emptyList -ExitCode 0).Total
Assert-Equal "List[Object]: total is 13"         13 $s.Total
Assert-Equal "List[Object]: recorded is 15"      15 $s.Recorded
Assert-Equal "List[Object]: no violations"       0  $s.Violations.Count
Assert-Equal "List[Object]: sentinel" "SMOKE-COMPLETE exit=1 pass=12 fail=1 skip=2" $s.SentinelLine

# Write-SmokeSummary is what contract-smoke.ps1 actually calls, and it must
# return the exit code rather than print it and drop it on the floor.
$rc = Write-SmokeSummary -Summary $s
Assert-Equal "Write-SmokeSummary returns the exit code" 1 $rc

# ---------------------------------------------------------------------------
Write-Host ""
if ($failures -gt 0) {
    Write-Host "::error::contract-smoke summary invariants: $failures of $checks assertion(s) FAILED."
    Write-Host "The smoke harness can report numbers that disagree with its own verdict. Fix scripts/lib/smoke-summary.ps1."
    exit 1
}
Write-Host "contract-smoke summary invariants: all $checks assertion(s) passed."
exit 0
