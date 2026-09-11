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
            # Explicit enumeration, NOT `@($action.PSObject.Properties.Name)`.
            #
            # MEASURED (CI run 34574931033, windows-latest, 2026-09-11): with
            # that spelling, 6 of this file's 8 self-tests FAILED under Windows
            # PowerShell 5.1 while all 8 passed under pwsh 7 on Linux. Every
            # failure was this branch firing on an action that HAD the key —
            # including the explicit-null case, which reported a MISSING key for
            # one present with a null value. That shape is what an empty name
            # list produces, and it is the whole of what was observed.
            #
            # WHAT IS NOT ESTABLISHED, said plainly so nobody quotes a cause
            # that was never proven: the same `.PSObject.Properties.Name
            # -contains` spelling appears a dozen times in contract-smoke.ps1
            # and passes on that same lane, so "member enumeration is broken on
            # 5.1" does NOT survive contact with the evidence. The engine-level
            # reason is UNKNOWN. What is known is that the explicit loop below
            # is unambiguous on both engines and the spelling above is not, so
            # the fix does not depend on the diagnosis being right.
            $hasEffect = $false
            foreach ($prop in $action.PSObject.Properties) {
                if ($prop.Name -eq 'effect') { $hasEffect = $true; break }
            }
            if (-not $hasEffect) {
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
