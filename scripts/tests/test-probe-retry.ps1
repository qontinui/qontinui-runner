#!/usr/bin/env pwsh
# test-probe-retry.ps1
#
# Pins the contract-smoke connection-retry policy (scripts/lib/probe-retry.ps1).
# No runner, no supervisor, no socket -- the prober is injected, so this is pure
# decision logic and runs in ~1s.
#
# WHAT THIS EXISTS TO CATCH
# -------------------------
# Three ways the retry can be wrong, and the suite is worth less than nothing if
# any of them lands:
#
#   1. Retrying too little -- the flake this was written for survives. A dropped
#      keep-alive returns status 0 from a request that never reached a handler,
#      and one of those fails a 1.5-hour job (runner#1008 on
#      POST /debug/highlight/:id; PR #1234 on POST /control/console-errors/clear).
#
#   2. Retrying a real HTTP status -- far worse, and silent. A handler returning
#      500 once and 200 the next time would pass, and the smoke would certify a
#      contract it had just watched break.
#
#   3. Retrying a TIMEOUT -- the same laundering as (2) through a side door, and
#      the easiest to miss, because a timeout yields status 0 exactly like a
#      dropped keep-alive. A handler that is slow cold and fast warm would FAIL
#      then PASS. Section 2 below is the case that pins this.
#
# The run-wide budget and the backoff have their own sections: without the budget
# "the runner died" degrades from a fast red into a crawl ending in a job
# timeout, and an unbounded or mis-scaled sleep does the same thing more quietly.
#
# RUN IT UNDER WINDOWS POWERSHELL 5.1, NOT pwsh 7
# -----------------------------------------------
# Same reasoning as test-smoke-summary.ps1: the gate runs `powershell -File` on
# windows-latest, so the tests must run on the interpreter that actually ships
# the behaviour.
#
# Usage:
#   powershell -File scripts/tests/test-probe-retry.ps1

$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "../lib/probe-retry.ps1")

$failures = 0
$checks   = 0

function Assert-Equal {
    param([string]$Name, $Expected, $Actual)
    $script:checks++
    $e = if ($null -eq $Expected) { "<null>" } else { "$Expected" }
    $a = if ($null -eq $Actual)   { "<null>" } else { "$Actual" }
    # -ceq, not -eq: PowerShell's -eq is case-INSENSITIVE, which would let "OK"
    # satisfy an expected "ok" and quietly weaken the body-preservation checks.
    if ($e -ceq $a) {
        Write-Host ("  PASS  {0,-58} {1}" -f $Name, $a)
    } else {
        Write-Host ("  FAIL  {0,-58} expected '{1}', got '{2}'" -f $Name, $e, $a) -ForegroundColor Red
        $script:failures++
    }
}

# A prober that replays a scripted sequence of @(status, body, kind) results and
# counts how many times it was asked. The last entry repeats once the list is
# exhausted, so "always fails" is one entry rather than a padded list.
$script:seq      = New-Object System.Collections.Generic.List[Object]
$script:calls    = 0
$script:lastArgs = $null

$fake = {
    param($m, $u, $b, $t)
    $script:lastArgs = @($m, $u, $b, $t)
    $i = $script:calls
    $script:calls++
    if ($i -ge $script:seq.Count) { $i = $script:seq.Count - 1 }
    return $script:seq[$i]
}

function Setup {
    param([Object[]]$Results, [int]$Budget = 12)
    $script:seq = New-Object System.Collections.Generic.List[Object]
    foreach ($r in $Results) { $script:seq.Add($r) | Out-Null }
    $script:calls = 0
    Reset-ProbeRetryBudget -Budget $Budget
}

Write-Host "contract-smoke probe retry policy (interpreter: $($PSVersionTable.PSVersion))"
Write-Host ""

# ---------------------------------------------------------------------------
Write-Host "1. A real HTTP status is the answer -- never retried."
# Each of these must cost exactly ONE prober call: a second call would mean a
# flapping handler could be laundered into a pass.
foreach ($code in @(200, 400, 404, 422, 500, 503)) {
    Setup -Results @(, @($code, "body-$code", ""))
    $res = Invoke-Probe -Method "GET" -Url "/x" -BaseDelayMs 0 -Prober $fake
    Assert-Equal "status $code returned as-is"         $code $res[0]
    Assert-Equal "status $code body preserved"         "body-$code" $res[1]
    Assert-Equal "status $code cost exactly 1 attempt" 1 $script:calls
    Assert-Equal "status $code absorbed no retries"    0 (Get-ProbeRetryStats).Absorbed
}

# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "2. A TIMEOUT is status 0 but must NOT be retried."
# The side door. A timeout means the request WAS delivered and the handler may
# have run; retrying it turns a slow-cold/fast-warm handler from FAIL into PASS.
Setup -Results @(@(0, "operation timed out", "timeout"), @(200, "warm-and-fast", ""))
$res = Invoke-Probe -Method "POST" -Url "/vision/capture" -BaseDelayMs 0 -Prober $fake
Assert-Equal "timeout reported, not retried into a pass" 0 $res[0]
Assert-Equal "timeout body preserved"    "operation timed out" $res[1]
Assert-Equal "timeout cost exactly 1 attempt"            1 $script:calls
Assert-Equal "timeout absorbed no retries"               0 (Get-ProbeRetryStats).Absorbed
Assert-Equal "timeout spent none of the budget"          12 (Get-ProbeRetryStats).BudgetRemaining

# The classifier is the load-bearing part, so assert it directly too.
Assert-Equal "classifier: transport is retryable"  $true  (Test-ProbeResultRetryable @(0, "x", "transport"))
Assert-Equal "classifier: timeout is not"          $false (Test-ProbeResultRetryable @(0, "x", "timeout"))
Assert-Equal "classifier: real status is not"      $false (Test-ProbeResultRetryable @(200, "x", ""))
Assert-Equal "classifier: 500 is not"              $false (Test-ProbeResultRetryable @(500, "x", ""))
Assert-Equal "classifier: null result is not"      $false (Test-ProbeResultRetryable $null)
Assert-Equal "classifier: 2-element legacy result defaults to transport" $true (Test-ProbeResultRetryable @(0, "x"))
# The [int] normalisation is what makes the status comparison total. Unreachable
# from Invoke-ProbeOnce today (it always returns a real int), but it is the
# contract that keeps a future prober returning "" or $null from being silently
# classified as a real HTTP status: uncast, "" -ne 0 evaluates TRUE.
Assert-Equal "classifier: empty status normalises to 0 (retryable)"  $true  (Test-ProbeResultRetryable @("", "x", "transport"))
Assert-Equal "classifier: string '200' is still a real status"       $false (Test-ProbeResultRetryable @("200", "x", ""))

# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "3. A transport error (dropped keep-alive) IS retried."
Setup -Results @(@(0, "kept-alive closed", "transport"), @(200, "ok", ""))
$res = Invoke-Probe -Method "POST" -Url "/control/console-errors/clear" -BaseDelayMs 0 -Prober $fake
Assert-Equal "recovers to the real status" 200 $res[0]
Assert-Equal "returns the recovered body"  "ok" $res[1]
Assert-Equal "took exactly 2 attempts"     2 $script:calls
Assert-Equal "recorded 1 absorbed retry"   1 (Get-ProbeRetryStats).Absorbed
Assert-Equal "spent 1 of the budget"       11 (Get-ProbeRetryStats).BudgetRemaining

# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "4. A persistent transport error still fails -- bounded, and honestly."
Setup -Results @(, @(0, "connection refused", "transport"))
$res = Invoke-Probe -Method "GET" -Url "/x" -BaseDelayMs 0 -Prober $fake
Assert-Equal "gives up and reports status 0"   0 $res[0]
Assert-Equal "preserves the connection error"  "connection refused" $res[1]
Assert-Equal "1 initial + 2 retries = 3 calls" 3 $script:calls
Assert-Equal "absorbed 2 retries"              2 (Get-ProbeRetryStats).Absorbed

# -ConnectRetries is honoured, so a caller can opt out entirely.
Setup -Results @(, @(0, "nope", "transport"))
$null = Invoke-Probe -Method "GET" -Url "/x" -BaseDelayMs 0 -ConnectRetries 0 -Prober $fake
Assert-Equal "ConnectRetries 0 makes exactly 1 attempt" 1 $script:calls

# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "5. The run-wide budget caps total retries across probes."
# Budget 1: the first probe may retry once, then the budget is gone and every
# later probe fails at first-attempt speed -- this is what keeps a dead runner a
# fast red instead of a job timeout.
Setup -Results @(, @(0, "down", "transport")) -Budget 1
$null = Invoke-Probe -Method "GET" -Url "/a" -BaseDelayMs 0 -Prober $fake
Assert-Equal "first probe: 1 initial + 1 budgeted retry" 2 $script:calls
Assert-Equal "budget is now empty"                       0 (Get-ProbeRetryStats).BudgetRemaining

$script:calls = 0
$null = Invoke-Probe -Method "GET" -Url "/b" -BaseDelayMs 0 -Prober $fake
Assert-Equal "next probe makes exactly 1 attempt"  1 $script:calls
Assert-Equal "total absorbed never exceeds budget" 1 (Get-ProbeRetryStats).Absorbed

# A spent budget must not go negative -- that would render as nonsense in the
# "budget remaining" line the summary prints.
$script:calls = 0
$null = Invoke-Probe -Method "GET" -Url "/c" -BaseDelayMs 0 -Prober $fake
Assert-Equal "budget floors at 0, never negative" 0 (Get-ProbeRetryStats).BudgetRemaining

# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "6. Bookkeeping and pass-through."
Setup -Results @(, @(204, "", "")) -Budget 7
Assert-Equal "Reset-ProbeRetryBudget sets the budget"  7 (Get-ProbeRetryStats).BudgetRemaining
Assert-Equal "Reset-ProbeRetryBudget zeroes the count" 0 (Get-ProbeRetryStats).Absorbed

$null = Invoke-Probe -Method "PUT" -Url "/control/thing" -Body '{"a":1}' -TimeoutSec 42 -BaseDelayMs 0 -Prober $fake
Assert-Equal "method reaches the prober"  "PUT" $script:lastArgs[0]
Assert-Equal "url reaches the prober"     "/control/thing" $script:lastArgs[1]
Assert-Equal "body reaches the prober"    '{"a":1}' $script:lastArgs[2]
Assert-Equal "timeout reaches the prober" 42 $script:lastArgs[3]

# An empty body must survive as "" rather than collapsing to $null: several
# probes read body shape, and a null would throw under the caller's reads.
Assert-Equal "empty body survives as empty string" "" (Invoke-Probe -Method "GET" -Url "/x" -BaseDelayMs 0 -Prober $fake)[1]

# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "7. The DEFAULT prober resolves Invoke-ProbeOnce."
# Every case above injects -Prober, so without this a rename or signature change
# to Invoke-ProbeOnce would leave this gate green and only blow up inside the
# 1.5-hour smoke step -- the exact cost the gate exists to avoid.
function Invoke-ProbeOnce {
    param([string]$Method, [string]$Url, [string]$Body = $null, [int]$TimeoutSec = 10)
    return @(299, "via-default-prober:$Method$Url", "")
}
Reset-ProbeRetryBudget
$res = Invoke-Probe -Method "GET" -Url "/defaulted" -BaseDelayMs 0
Assert-Equal "default prober is used when -Prober is omitted" 299 $res[0]
Assert-Equal "default prober passes method and url through"   "via-default-prober:GET/defaulted" $res[1]

# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "8. Backoff actually sleeps, in the right unit, and stays bounded."
# Section 7 of the previous revision timed a path with -BaseDelayMs 0, so it
# contained no sleep at all and four mutants survived it -- including
# `-Milliseconds` -> `-Seconds`, a 1000x blowup. These cases run the real sleep.
Setup -Results @(, @(0, "flap", "transport"))
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$null = Invoke-Probe -Method "GET" -Url "/x" -BaseDelayMs 40 -ConnectRetries 2 -Prober $fake
$sw.Stop()
$el = $sw.ElapsedMilliseconds
# Two sleeps: 40ms then 80ms = 120ms. The lower bound proves both ran AND that
# the delay doubles; the upper bound catches a unit or scale error (-Seconds
# would be 120s; a x100 factor would hit the 2000ms clamp on the second sleep).
Assert-Equal "two backoff sleeps ran and doubled (>=100ms)" $true ($el -ge 100)
Assert-Equal "backoff is milliseconds, not seconds (<1000ms)" $true ($el -lt 1000)

# The DEFAULT delay must be sane too -- the case above passes an explicit value,
# so it would not notice the default being raised to something absurd.
Setup -Results @(, @(0, "flap", "transport"))
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$null = Invoke-Probe -Method "GET" -Url "/x" -ConnectRetries 1 -Prober $fake
$sw.Stop()
Assert-Equal "default BaseDelayMs stays sub-second" $true ($sw.ElapsedMilliseconds -lt 1000)

# ---------------------------------------------------------------------------
Write-Host ""
if ($failures -gt 0) {
    Write-Host "::error::contract-smoke probe retry policy: $failures of $checks assertion(s) FAILED."
    Write-Host "Either a transient transport error is no longer absorbed, or -- far worse -- a real HTTP status or a timeout is being retried. Fix scripts/lib/probe-retry.ps1."
    exit 1
}
Write-Host "contract-smoke probe retry policy: all $checks assertion(s) passed."
exit 0
