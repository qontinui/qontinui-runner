#!/usr/bin/env pwsh
# probe-retry.ps1 -- DEFINITIONS ONLY. Dot-source it; it runs no top-level code.
#
# Owns the contract-smoke retry policy for connection-level probe failures,
# extracted out of contract-smoke.ps1 so it can be unit-tested WITHOUT booting a
# runner or opening a socket. The tests live in scripts/tests/test-probe-retry.ps1
# and are a CI gate, mirroring scripts/lib/smoke-summary.ps1.
#
# WHY THIS EXISTS
# ---------------
# Invoke-WebRequest (PS 5.1) pools TCP connections and reuses them with
# keep-alive. axum closes an idle connection when its keep-alive timeout expires.
# When that close races the client picking the same socket off the pool and
# writing the next request, .NET surfaces
#
#     The underlying connection was closed: A connection that was expected to be
#     kept alive was closed by the server.
#
# as a WebException carrying NO Response -- so the probe returns status 0 and the
# caller records a FAIL. The request never reached a handler, so nothing about
# the contract was exercised. That FAIL is pure noise, and it fails the job.
#
# This is neither hypothetical nor new. runner#1008 (job 92967302466) was this
# exact race on POST /debug/highlight/:id -- see the header of
# scripts/lib/smoke-summary.ps1. That work fixed how the flake was REPORTED and
# left the flake itself in place. It resurfaced on PR #1234 as POST
# /control/console-errors/clear: 194 of 195 probes passed, the probes 33ms before
# and 88ms after it both returned 200, and a 1.5-hour Windows job went red over a
# race that tested nothing.
#
# WHAT IS AND IS NOT RETRIED
# --------------------------
# Retrying the wrong thing is far more dangerous than the flake, so the predicate
# is deliberately narrow on BOTH axes:
#
#   1. A real HTTP status is never retried. Every genuine status is the contract
#      answer this suite asserts on (the route table deliberately expects
#      400/404/422 in many places). Retrying one would let a handler that returns
#      500 once and 200 the next time pass -- exactly the bug class the smoke
#      exists to catch.
#
#   2. A TIMEOUT is never retried either, even though it also yields status 0.
#      Status 0 is an overloaded sentinel: it covers a connection that broke
#      before delivery AND a request the server received but answered too slowly.
#      Verified on 5.1.26100 against a listener that accepts a POST and never
#      replies -- WebException.Status = Timeout, Response = null, byte-identical
#      to a dropped keep-alive at the transport layer. Retrying that would let a
#      handler that is slow cold and fast warm FAIL then PASS, which is the same
#      laundering as (1) through a side door. SLOW_ROUTES and the warm-up block
#      in contract-smoke.ps1 exist because cold-vs-warm variance on these routes
#      is observed, not theoretical.
#
# Invoke-ProbeOnce therefore classifies the failure where the exception is still
# in hand and passes the KIND as a third tuple element; this file only ever
# retries "transport". The classification is a DENYLIST (everything that is not
# a real response and not a timeout is transport) rather than an allowlist of
# WebExceptionStatus values, and that shape is deliberate: .NET transparently
# recovers most stale-pooled-connection cases, so the race could not be
# reproduced locally to confirm which status CI's failure actually carries. An
# allowlist that guessed wrong would silently not fix the bug -- a worse failure
# mode than a slightly wide denylist, whose one dangerous member (timeout) is
# named explicitly.
#
# IDEMPOTENCY, precisely: for a pre-delivery break there is no server-side effect
# to duplicate. "transport" is broader than that -- a mid-response failure such
# as ReceiveFailure lands here too, and re-sending that POST could repeat an
# effect the handler already applied. The blast radius is a throwaway CI runner,
# and SKIP_ROUTES already excludes POST /render-log/snapshot as non-idempotent;
# but note that SKIP_ROUTES is not a curated "safe to retry" list -- its other
# entries are feature-gated or fixture-less, not idempotency judgements.
#
# WHY THERE IS A RUN-WIDE BUDGET
# ------------------------------
# A per-probe retry with no global cap converts "the runner died" from a fast
# failure into a crawl across ~195 probes that ends in a job timeout -- strictly
# worse than the red it replaces. The budget is spent across the whole run: a
# healthy run uses 0-2 of it, while a dead server exhausts it early and every
# later probe then fails at first-attempt speed, as it does today.
#
# The budget bounds retry COUNT. Wall clock is bounded separately, and mostly by
# rule 2 above: a wedged-but-listening runner produces timeouts, which are not
# retried at all, so retries cannot each cost a full -TimeoutSec. What remains is
# transport failures, which fail fast (a dead port RSTs immediately).
#
# Retries are PRINTED but never Record'ed by the caller. A retry is not a probe
# outcome, and smoke-summary.ps1 invariant I3 requires every recorded row to
# carry a PASS/FAIL/SKIP status -- a RETRY row would trip the harness's own guard.

# Deliberately NO `Set-StrictMode` here, for the same reason smoke-summary.ps1
# omits it: this file is DOT-SOURCED, so a strict mode set at the top would leak
# into contract-smoke.ps1's scope and turn its many absent-field probe-response
# reads into terminating errors.

$script:ConnectRetryBudget   = 12
$script:ConnectRetryCount    = 0
$script:ConnectRetryDefault  = 12
$script:ConnectRetryExhausted = $false

# Longest a single backoff sleep may become. The default -ConnectRetries 2 tops
# out at 400ms on its own, but a future caller passing a larger value would
# otherwise double into minutes inside one probe.
$script:ConnectRetryMaxDelayMs = 2000

# Reset the run-wide budget. contract-smoke calls this once at startup; the tests
# call it between cases so each starts from a known budget.
function Reset-ProbeRetryBudget {
    param([int]$Budget = $script:ConnectRetryDefault)
    $script:ConnectRetryBudget    = $Budget
    $script:ConnectRetryCount     = 0
    $script:ConnectRetryExhausted = $false
}

# Retries absorbed so far, and what is left of the budget.
function Get-ProbeRetryStats {
    return [PSCustomObject]@{
        Absorbed        = $script:ConnectRetryCount
        BudgetRemaining = $script:ConnectRetryBudget
    }
}

# Is this probe result a transient TRANSPORT failure -- the only thing worth
# retrying? Split out so the decision is directly testable.
#
# A result with no third element is treated as transport so an older or hand-
# rolled prober keeps working; Invoke-ProbeOnce always supplies the kind, so that
# default is unreachable in the real harness.
function Test-ProbeResultRetryable {
    param($Result)
    if ($null -eq $Result) { return $false }
    if ([int]$Result[0] -ne 0) { return $false }   # a real HTTP status is the answer
    $kind = if ($Result.Count -ge 3) { "$($Result[2])" } else { "transport" }
    if ($kind -eq "") { $kind = "transport" }
    return ($kind -eq "transport")
}

# Retrying wrapper. -Prober lets the tests inject a scripted sequence of results
# instead of doing real HTTP; contract-smoke leaves it unset and gets
# Invoke-ProbeOnce, which it defines.
#
# Returns whatever the underlying probe returned, so this is a drop-in for every
# existing call site (all of which index [0] and [1] only).
function Invoke-Probe {
    param(
        [string]$Method,
        [string]$Url,
        [string]$Body = $null,
        [int]$TimeoutSec = 10,
        [int]$ConnectRetries = 2,
        [int]$BaseDelayMs = 200,
        [scriptblock]$Prober = $null
    )
    if (-not $Prober) {
        $Prober = { param($m, $u, $b, $t) Invoke-ProbeOnce -Method $m -Url $u -Body $b -TimeoutSec $t }
    }

    $delayMs = $BaseDelayMs
    for ($attempt = 1; ; $attempt++) {
        $res = & $Prober $Method $Url $Body $TimeoutSec

        # A real HTTP status, or a timeout: either way it is the answer. Done.
        if (-not (Test-ProbeResultRetryable $res)) { return $res }

        # Per-probe attempts spent: report the connection error, as before.
        if ($attempt -gt $ConnectRetries) { return $res }

        # Run-wide budget spent: stop absorbing, and say so ONCE rather than
        # printing a "RETRY" line per probe for the rest of a dead run.
        if ($script:ConnectRetryBudget -le 0) {
            if (-not $script:ConnectRetryExhausted) {
                $script:ConnectRetryExhausted = $true
                Write-Host ("  RETRY  budget exhausted -- connection errors are reported as FAIL from here ({0} {1})" -f $Method, $Url)
            }
            return $res
        }

        $script:ConnectRetryBudget--
        $script:ConnectRetryCount++
        Write-Host ("  RETRY  {0} {1} (attempt {2}/{3}): {4}" -f $Method, $Url, $attempt, ($ConnectRetries + 1), $res[1])
        if ($delayMs -gt 0) { Start-Sleep -Milliseconds $delayMs }
        $delayMs = [Math]::Min($delayMs * 2, $script:ConnectRetryMaxDelayMs)
    }
}
