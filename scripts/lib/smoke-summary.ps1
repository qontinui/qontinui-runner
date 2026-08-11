#!/usr/bin/env pwsh
# smoke-summary.ps1 -- DEFINITIONS ONLY. Dot-source it; it runs no top-level code.
#
# Owns the contract-smoke summary accounting, extracted out of contract-smoke.ps1
# so it can be unit-tested WITHOUT booting a runner. The tests live in
# scripts/tests/test-smoke-summary.ps1 and are a CI gate.
#
# WHY THIS IS ITS OWN FILE
# ------------------------
# The summary line is the half of the harness a human actually reads. On
# runner#1008 (job 92967302466) it printed
#
#     Smoke: 193 pass / 193 total, 8 skip
#     SMOKE-COMPLETE exit=1 pass=193 fail= skip=8
#
# while the run had genuinely FAILED: one probe (POST /debug/highlight/:id) died
# on a dropped keep-alive. pass == total reads as a clean sweep, `fail=` is blank
# rather than 1, and the only thing telling the truth was the exit code. A reader
# -- or any tool scraping that line -- concludes the suite is green and the red is
# infrastructural. That is the same failure family as a test that skips into a
# vacuous green: the report disagrees with the verdict, and the report wins.
#
# ROOT CAUSE (Windows PowerShell 5.1, verified -- not folklore)
# ------------------------------------------------------------
# `Where-Object` emits a SCALAR when exactly one object matches, an array
# otherwise. PS 5.1's unified scalar `Count` adapter covers .NET-backed scalars
# ([string], [int], [hashtable] all yield 1) but NOT `PSCustomObject`, where
# `.Count` evaluates to $null. So with exactly ONE failing row:
#
#     $fail  = ($results | Where-Object { $_.Status -eq "FAIL" }).Count   # -> $null
#     $total = $pass + $fail                                             # -> $pass
#     "... fail={0} ..." -f $fail                                        # -> "fail="
#     if ($fail -gt 0) { ... }                                           # -> never fires
#
# Two or more failures happened to report correctly, because that path returns an
# array. Wrapping each filter in `@()` forces an array in every arity and is the
# actual fix. Everything below that is the guard rail so this class cannot come
# back silently.
#
# THE INVARIANTS
# --------------
#   I1  pass, fail and skip are all non-null integers      (never renders blank)
#   I2  pass + fail + skip == the number of recorded rows  (nothing vanishes)
#   I3  every recorded row carries a PASS/FAIL/SKIP status (no unclassified bucket)
#   I4  exitCode != 0  IFF  fail > 0                       (summary can't contradict it)
#
# I4 is the load-bearing one and it is pure convention in the harness: every
# `$exitCode = 1` site is HAND-PAIRED with a `Record "FAIL"` call. A future probe
# that sets the exit code without recording a row -- or records one without
# setting the code -- reproduces the exact symptom above. I4 turns that from a
# silent lie into a loud failure.
#
# A violation NEVER lowers the exit code. It raises 0 -> 2 and leaves any
# non-zero code alone, so "the harness is broken" (2) stays distinguishable from
# "a probe failed" (1) and neither can be laundered into success.

# Deliberately NO `Set-StrictMode` here. This file is DOT-SOURCED, so a strict
# mode set at the top would leak into contract-smoke.ps1's scope and turn its
# many probe-response property reads (`$h.data`, `$d.format`, ...) into
# terminating errors on any absent field. Rendering the summary must not be able
# to change how the probes behave.

function Get-SmokeSummary {
    <#
    .SYNOPSIS
        Tally contract-smoke result rows and check the reporting invariants.
    .PARAMETER Results
        The recorded rows. Each is expected to expose a .Status of PASS/FAIL/SKIP.
    .PARAMETER ExitCode
        The exit code the probe phase arrived at, BEFORE any invariant check.
    .OUTPUTS
        PSCustomObject with Pass, Fail, Skip, Total, Recorded, ExitCode,
        Violations, SummaryLine and SentinelLine.
    #>
    [CmdletBinding()]
    param(
        # The type annotation and the absence of `Mandatory` are BOTH deliberate;
        # this parameter is a minefield in Windows PowerShell 5.1.
        #
        #   - `Mandatory` rejects an empty collection, so a zero-row run could
        #     not be summarised at all.
        #   - The usual remedy, `[AllowEmptyCollection()]`, only applies to
        #     collection-typed parameters.
        #   - An UNTYPED (or `[object]`-typed) parameter throws
        #     "ArgumentException: Argument types do not match" when handed a
        #     System.Collections.Generic.List[Object] -- which is precisely what
        #     contract-smoke.ps1 accumulates $results into. Wrapping at the call
        #     site (`@($results)`) does not help; the binder fails first.
        #
        # `[System.Collections.IEnumerable]` is the annotation that accepts all
        # four shapes this is called with: $null, @(), object[] and
        # List[Object]. `@($Results)` below normalises whichever arrives.
        # scripts/tests/test-smoke-summary.ps1 pins the generic-list case, since
        # that is the only shape the real gate ever passes and the only one an
        # array-based test would miss.
        [System.Collections.IEnumerable]$Results = $null,

        [Parameter(Mandatory = $true)]
        [int]$ExitCode
    )

    # @() on BOTH sides is load-bearing, not style: it normalises the 0-row and
    # 1-row arities that would otherwise be $null and a bare scalar.
    $rows     = @($Results)
    $recorded = $rows.Count

    $pass = @($rows | Where-Object { $_.Status -eq 'PASS' }).Count
    $fail = @($rows | Where-Object { $_.Status -eq 'FAIL' }).Count
    $skip = @($rows | Where-Object { $_.Status -eq 'SKIP' }).Count

    $violations = @()

    # --- I1: never render a blank count -------------------------------------
    # Unreachable while the @() above stand -- which is the point. If a future
    # edit drops one, the count goes $null and the summary silently renders
    # `fail=` again. Substituting -1 makes the regression VISIBLE in the printed
    # line even in the split second before anyone reads the violation below it.
    if ($null -eq $pass) { $violations += 'pass count evaluated to $null (Where-Object scalar collapse -- restore the @() wrapper)'; $pass = -1 }
    if ($null -eq $fail) { $violations += 'fail count evaluated to $null (Where-Object scalar collapse -- restore the @() wrapper)'; $fail = -1 }
    if ($null -eq $skip) { $violations += 'skip count evaluated to $null (Where-Object scalar collapse -- restore the @() wrapper)'; $skip = -1 }

    # --- I3: no row may fall outside the three buckets -----------------------
    # `Record` takes a free-form [string]$Status. A typo'd or newly-invented
    # status would land in $results and be counted by NOTHING, disappearing from
    # the summary entirely -- a quieter version of the same bug.
    $unclassified = @($rows | Where-Object { $_.Status -ne 'PASS' -and $_.Status -ne 'FAIL' -and $_.Status -ne 'SKIP' })
    if ($unclassified.Count -gt 0) {
        $sample = ($unclassified | Select-Object -First 3 | ForEach-Object { "'$($_.Status)' ($($_.Method) $($_.Path))" }) -join ', '
        $violations += "$($unclassified.Count) recorded row(s) carry a status outside PASS/FAIL/SKIP and are counted by nothing: $sample"
    }

    # --- I2: the buckets must account for every recorded row -----------------
    if (($pass + $fail + $skip) -ne $recorded) {
        $violations += "bucket totals do not account for every row: pass($pass) + fail($fail) + skip($skip) = $($pass + $fail + $skip), but $recorded row(s) were recorded"
    }

    # --- I4: the summary must not be able to contradict the exit code --------
    $exitSaysFailed  = ($ExitCode -ne 0)
    $countSaysFailed = ($fail -gt 0)
    if ($exitSaysFailed -ne $countSaysFailed) {
        if ($exitSaysFailed) {
            $violations += "exit code is $ExitCode (failed) but fail=$fail -- a failing run whose summary reads green. Every `$exitCode = 1 site must also Record a FAIL row."
        } else {
            $violations += "exit code is 0 (passed) but fail=$fail -- failures were recorded without failing the run. Every Record `"FAIL`" must also set `$exitCode = 1."
        }
    }

    # Total is the denominator of "N pass / M total": probes ATTEMPTED, i.e.
    # pass + fail. Skips are reported alongside rather than folded in -- a
    # skipped route was never asserted, so counting it as attempted would
    # inflate the denominator the same way excluding a failure deflated it.
    $total = $pass + $fail

    # A violation may only RAISE the exit code. 2 = the harness itself is
    # unsound (distinct from 1 = a probe failed); a non-zero code is never
    # overwritten, and 0 is never returned when violations exist.
    $finalExit = $ExitCode
    if ($violations.Count -gt 0 -and $finalExit -eq 0) { $finalExit = 2 }

    return [PSCustomObject]@{
        Pass         = $pass
        Fail         = $fail
        Skip         = $skip
        Total        = $total
        Recorded     = $recorded
        ExitCode     = $finalExit
        Violations   = $violations
        SummaryLine  = ("Smoke: {0} pass / {1} total, {2} skip" -f $pass, $total, $skip)
        # Sentinel wording and field order are a CONSUMED FORMAT (CI logs, log
        # scrapers, humans grepping a captured run). Change it only deliberately.
        SentinelLine = ("SMOKE-COMPLETE exit={0} pass={1} fail={2} skip={3}" -f $finalExit, $pass, $fail, $skip)
    }
}

function Write-SmokeSummary {
    <#
    .SYNOPSIS
        Print a Get-SmokeSummary result -- summary line, violations, sentinel.
    .OUTPUTS
        The exit code the caller should exit with.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        $Summary
    )

    Write-Host ""
    Write-Host $Summary.SummaryLine
    if ($Summary.Fail -gt 0) {
        Write-Host ("FAIL count: {0}" -f $Summary.Fail) -ForegroundColor Red
    }

    if ($Summary.Violations.Count -gt 0) {
        Write-Host ""
        Write-Host "HARNESS-INVARIANT-VIOLATION -- the smoke's own accounting is unsound." -ForegroundColor Red
        Write-Host "The numbers above cannot be trusted; treat this run as INVALID, not as a result." -ForegroundColor Red
        foreach ($v in $Summary.Violations) {
            Write-Host "  - $v" -ForegroundColor Red
        }
    }

    # Completion sentinel. A caller (CI, a harness, a human reading a captured
    # log) can distinguish "the smoke ran to the end and this is its verdict"
    # from "the process died somewhere upstream" only if the script says so
    # explicitly -- a bare exit code cannot carry that. Absence of this line
    # means the run did NOT complete, whatever the exit code says.
    Write-Host $Summary.SentinelLine

    return $Summary.ExitCode
}
