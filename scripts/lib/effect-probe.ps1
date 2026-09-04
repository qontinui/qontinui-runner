#!/usr/bin/env pwsh
# effect-probe.ps1
#
# The verdict half of contract-smoke's Probe 2b (per-action `effect`
# round-trip). Plan
# `2026-09-04-effect-calculus-joins-the-component-action-registry`, Phase 1.
#
# WHY THIS IS A SEPARATE FILE
# ---------------------------
# Probe 2b's job is to go RED when the `effect` annotation is stripped between
# the SDK and the runner's IPC consumers. Everything that touches a live runner
# (spawn, activate-tab, poll, HTTP) can only be exercised on a box with a
# supervisor and a Windows-gated CI lane — which means the probe's *decision*
# would otherwise never be observed failing on any developer machine. A check
# nobody has watched fail has not been shown to check anything.
#
# So the decision lives here, as a pure function over already-parsed JSON, and
# `scripts/tests/test-effect-probe.ps1` mutation-tests it (missing key, wrong
# value, absent action, one-surface-only regression) with no runner at all.
# Same split, and the same reason, as `lib/smoke-summary.ps1`.

<#
.SYNOPSIS
    Return the list of problems with the `effect` annotations on one or more
    serialized component surfaces. An EMPTY list means the probe passes.

.PARAMETER Surfaces
    One entry per surface to check, each a hashtable/PSCustomObject with:
      Name    — how the surface is named in a failure message
                (e.g. 'components-list', 'component-detail')
      Actions — the surface's `actions` array, already ConvertFrom-Json'd.
    Both `/control/components` and `/control/component/:id` are checked because
    they are SEPARATE `serializeComponent` call sites (useControlEvents.ts
    `get_components` vs `get_component`); a regression can hit one alone.

.PARAMETER ExpectedEffects
    Hashtable of actionId -> expected effect string.

.OUTPUTS
    [string[]] — one line per problem. Always an array, possibly empty.
#>
function Get-EffectProbeProblems {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)] $Surfaces,
        [Parameter(Mandatory = $true)] [hashtable]$ExpectedEffects
    )

    $problems = @()

    # @() on both loops is load-bearing for the same reason smoke-summary.ps1
    # wraps its Where-Object results: a single-element result collapses to a
    # bare scalar, and a zero-element one to $null.
    foreach ($surface in @($Surfaces)) {
        $surfaceName = $surface.Name
        $actions = @($surface.Actions)

        if ($actions.Count -eq 0) {
            # Deliberately a PROBLEM, not a skip. An empty action list is
            # exactly what a stripped projection looks like from here.
            $problems += "${surfaceName}: no actions on the fixture component"
            continue
        }

        foreach ($actionId in @($ExpectedEffects.Keys)) {
            $action = $actions | Where-Object { $_.id -eq $actionId } | Select-Object -First 1
            if (-not $action) {
                $problems += "${surfaceName}: action '$actionId' absent"
                continue
            }

            # Presence of the KEY is the thing under test, and it is distinct
            # from the value: `serializeComponent`'s per-action projection is a
            # closed allow-list, so a dropped field vanishes entirely rather
            # than arriving null.
            $names = @($action.PSObject.Properties.Name)
            if ($names -notcontains 'effect') {
                $problems += "${surfaceName}: '$actionId' has NO effect key -- stripped by serializeComponent's per-action allow-list (src/hooks/ui-bridge-events/utils.ts)"
                continue
            }

            $want = $ExpectedEffects[$actionId]
            $got = $action.effect
            if ($got -ne $want) {
                $rendered = if ($null -eq $got) { '<null>' } else { "$got" }
                $problems += "${surfaceName}: '$actionId' effect='$rendered', expected '$want'"
            }
        }
    }

    return , ([string[]]$problems)
}
