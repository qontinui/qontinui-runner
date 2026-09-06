#!/usr/bin/env pwsh
# test-effect-probe.ps1
#
# Mutation-tests the verdict half of contract-smoke Probe 2b
# (scripts/lib/effect-probe.ps1). No runner, no supervisor, no network — pure
# decision logic over parsed JSON, runs in ~1s on any OS with pwsh.
#
# WHAT THIS EXISTS TO CATCH
# -------------------------
# Probe 2b's live half needs a supervisor and a Windows CI lane, so on most
# machines its decision is never executed at all — let alone executed FAILING.
# The defect the whole probe exists to catch (a per-action field silently
# dropped by `serializeComponent`'s closed allow-list) is invisible to a check
# that has only ever been observed green. These cases drive the verdict function
# through every red it is supposed to produce.
#
# Usage:
#   pwsh -NoProfile -File scripts/tests/test-effect-probe.ps1

$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "../lib/effect-probe.ps1")

$failures = 0
$checks = 0

$Expected = @{ "list-tabs" = "read"; "switch-tab" = "write" }

# Actions are built from JSON text rather than PSCustomObject literals so the
# "key is absent" case is genuinely absent — the exact shape ConvertFrom-Json
# produces for a serializer that dropped the field, not a null-valued property.
function Surface {
    param([string]$Name, [string]$ActionsJson)
    return @{ Name = $Name; Actions = @($ActionsJson | ConvertFrom-Json) }
}

$GOOD_ACTIONS = @'
[
  {"id":"save","label":"Save Settings"},
  {"id":"switch-tab","label":"Switch settings tab","effect":"write"},
  {"id":"list-tabs","label":"List available settings tabs","effect":"read"}
]
'@

function Assert-Problems {
    param([string]$Name, [int]$ExpectedCount, $Problems, [string]$MustContain = $null)
    $script:checks++
    $arr = @($Problems)
    $ok = ($arr.Count -eq $ExpectedCount)
    if ($ok -and $MustContain) {
        $ok = [bool](@($arr | Where-Object { $_ -like "*$MustContain*" }).Count -gt 0)
    }
    if ($ok) {
        Write-Host ("  PASS  {0,-56} {1} problem(s)" -f $Name, $arr.Count)
    } else {
        Write-Host ("  FAIL  {0,-56} expected {1} problem(s){2}, got {3}: {4}" -f `
                $Name, $ExpectedCount, $(if ($MustContain) { " containing '$MustContain'" } else { "" }), `
                $arr.Count, ($arr -join ' | ')) -ForegroundColor Red
        $script:failures++
    }
}

Write-Host "test-effect-probe: Get-EffectProbeProblems"

# --- The green case ---------------------------------------------------------
Assert-Problems "both surfaces fully annotated -> no problems" 0 (
    Get-EffectProbeProblems -Surfaces @(
        (Surface 'components-list' $GOOD_ACTIONS),
        (Surface 'component-detail' $GOOD_ACTIONS)
    ) -ExpectedEffects $Expected)

# --- The defect this probe was built for ------------------------------------
# `effect` stripped from EVERY action, which is what the runner emitted before
# `effect: a.effect` was added to serializeComponent's per-action allow-list.
$STRIPPED = @'
[
  {"id":"save","label":"Save Settings"},
  {"id":"switch-tab","label":"Switch settings tab"},
  {"id":"list-tabs","label":"List available settings tabs"}
]
'@
Assert-Problems "effect stripped from both surfaces -> 4 problems" 4 (
    Get-EffectProbeProblems -Surfaces @(
        (Surface 'components-list' $STRIPPED),
        (Surface 'component-detail' $STRIPPED)
    ) -ExpectedEffects $Expected) "has NO effect key"

# --- One surface regressing alone -------------------------------------------
# `/control/components` and `/control/component/:id` are separate
# serializeComponent call sites; a fix or a break can land on one only.
Assert-Problems "detail surface alone loses effect -> 2 problems" 2 (
    Get-EffectProbeProblems -Surfaces @(
        (Surface 'components-list' $GOOD_ACTIONS),
        (Surface 'component-detail' $STRIPPED)
    ) -ExpectedEffects $Expected) "component-detail"

# --- Wrong value, key present -----------------------------------------------
$MISCLASSIFIED = @'
[
  {"id":"switch-tab","effect":"read"},
  {"id":"list-tabs","effect":"read"}
]
'@
Assert-Problems "switch-tab misclassified as read -> 1 problem" 1 (
    Get-EffectProbeProblems -Surfaces @((Surface 'components-list' $MISCLASSIFIED)) `
        -ExpectedEffects $Expected) "expected 'write'"

# --- Explicit null is not the same as a declared value ----------------------
# A layer that started emitting `"effect": null` would satisfy a naive
# key-presence check while carrying no classification at all.
$NULLED = @'
[
  {"id":"switch-tab","effect":null},
  {"id":"list-tabs","effect":"read"}
]
'@
Assert-Problems "explicit null effect is a problem" 1 (
    Get-EffectProbeProblems -Surfaces @((Surface 'components-list' $NULLED)) `
        -ExpectedEffects $Expected) "<null>"

# --- Missing action ---------------------------------------------------------
$MISSING_ACTION = @'
[
  {"id":"list-tabs","effect":"read"}
]
'@
Assert-Problems "annotated action removed entirely -> 1 problem" 1 (
    Get-EffectProbeProblems -Surfaces @((Surface 'components-list' $MISSING_ACTION)) `
        -ExpectedEffects $Expected) "'switch-tab' absent"

# --- Empty action list is a FAILURE, never a pass ---------------------------
# The whole point of Probe 2b (unlike the scope probe beside it) is that it
# cannot resolve an absent fixture into a green.
Assert-Problems "empty actions array -> 1 problem, not a pass" 1 (
    Get-EffectProbeProblems -Surfaces @(@{ Name = 'components-list'; Actions = @() }) `
        -ExpectedEffects $Expected) "no actions"

# --- Single-element arity ---------------------------------------------------
# PS 5.1 collapses a one-element Where-Object result to a bare scalar; the
# smoke gate runs under 5.1 (see test-smoke-summary.ps1's header), so pin it.
Assert-Problems "one expected effect, one action, all good -> 0 problems" 0 (
    Get-EffectProbeProblems -Surfaces @((Surface 'components-list' '[{"id":"list-tabs","effect":"read"}]')) `
        -ExpectedEffects @{ "list-tabs" = "read" })

Write-Host ""
if ($failures -gt 0) {
    Write-Host "test-effect-probe: $failures of $checks checks FAILED" -ForegroundColor Red
    exit 1
}
Write-Host "test-effect-probe: all $checks checks passed" -ForegroundColor Green
exit 0
