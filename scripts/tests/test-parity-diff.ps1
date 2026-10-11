#!/usr/bin/env pwsh
# test-parity-diff.ps1
#
# Pins the classifier in scripts/lib/parity-diff.ps1 -- the comparator core of
# plan 2026-08-31-published-build-parity-check, Phase 5. No binary, no install,
# no network: pure classification over JSON, runs in ~1s.
#
# THE FIXTURE IS A REAL MANIFEST, NOT A HAND-WRITTEN SHAPE
# --------------------------------------------------------
# fixtures/parity-manifest-dev-sample.json was emitted by the built binary
# (`--capability-manifest --json`) on 2026-09-02. Every case below is either
# that file compared with itself or that file with a NAMED mutation applied, so
# a test passing here is a statement about the real wire format rather than
# about a shape the test author imagined.
#
# WHAT THIS EXISTS TO CATCH
# -------------------------
# The comparator's dangerous failure is not a crash, it is a FALSE GREEN. On the
# cold CLI door 8 of the 9 rows read `unknown` on BOTH legs; a naive diff finds
# them equal and prints "0 differences", certifying parity while having observed
# one row. So the load-bearing assertions here are the ones that prove
# `unknown` == `unknown` is classified as UNOBSERVED and never as agreement, and
# that the verdict line refuses to let a 0 stand next to a thin denominator.
#
# RUN IT UNDER WINDOWS POWERSHELL 5.1 -- BUT pwsh 7 IS NOT A WEAKER SIGNAL
# ------------------------------------------------------------------------
# 5.1 remains the interpreter this must pass under: the exactly-one-row cases
# are where PS 5.1's scalar `Count` adapter returns $null for PSCustomObject
# (same reasoning as scripts/tests/test-smoke-summary.ps1), and the real gate
# runs under `powershell -File`. That much is unchanged.
#
# What an earlier revision of this comment got WRONG, at a real cost: it said
# passing under pwsh 7 "would green-light a regression that still breaks the
# real thing", which readers took to mean 7 is uninformative here. It is not.
# When this suite first ran on the Windows gate it died on its FIRST call into
# Compare-CapabilityManifests; fixing that exposed a SECOND defect, in
# Format-ParityReportText, which nothing had ever reached. Both reproduce
# verbatim under pwsh 7 on Linux -- same exception type, same message, same
# function. Neither was version-specific:
#
#   * `@($x)` where $x is a PSObject-wrapped List[Object] throws
#     `ArgumentException: Argument types do not match` out of the DLR binder
#     (PSEnumerableBinder.MaybeDebase), on 5.1 and on 7 alike;
#   * `$L.Add("..." -f $a, $b)` parses the comma as a METHOD-ARGUMENT
#     separator, so the format string gets one argument and `-f` throws. Same
#     grammar in both.
#
# So the accurate rule is DIRECTIONAL, not dismissive: a pwsh 7 failure is real
# and costs one second to find on any box, while only a 5.1 run attests to the
# `.Count` class. Reach for `pwsh -NoProfile -File` first when developing;
# never let it SUBSTITUTE for the 5.1 gate.
#
# WHERE IT RUNS TODAY
# -------------------
# The first step of .github/workflows/published-parity.yml, before any compile.
# It is NOT wired into ci.yml. The cost of doing so is real but smaller than an
# earlier version of this comment claimed: ci.yml is inside ci-integrity.yml's
# scope (that scope is the guard's own trigger, not a "guarded list" -- the
# hand-kept allowlist was replaced by the trigger itself), so a main-based PR
# editing ci.yml goes red until its author declares the change. That is NOT an
# operator review -- the guard's own error text says "No operator is involved."
# But note WHICH declaration, because the two are not interchangeable: adding a
# whole new JOB is additive and needs only `ci:gate-change=declared`, while
# adding a STEP to an existing job mutates that job's surface. The addition
# proposed below goes next to the `Contract-smoke summary invariants (PS 5.1)`
# step, which lives in ci.yml's `test` job, so it would need the stronger
# `ci:gate-change=alters-a-gate` plus a whole line `Gate-Change: ci#test` in the
# PR body, and a re-run of that job. If per-PR coverage is wanted, that is a
# deliberate one-line addition next to that step, made with that cost in mind.
#
# Usage:
#   powershell -File scripts/tests/test-parity-diff.ps1

$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "../lib/parity-diff.ps1")

$FixturePath = Join-Path $PSScriptRoot "fixtures/parity-manifest-dev-sample.json"
if (-not (Test-Path -LiteralPath $FixturePath)) {
    Write-Host "FATAL: missing fixture $FixturePath" -ForegroundColor Red
    exit 1
}
# -Encoding UTF8 is load-bearing on the real gate. The fixture is UTF-8 with NO
# BOM and carries non-ASCII on 9 lines (arrows and em-dashes inside `note` /
# `detail`), and Windows PowerShell 5.1 defaults Get-Content to the ANSI
# codepage -- so without this the 5.1 leg silently reads those fields as
# mojibake. No assertion touches them today, which is exactly why it would have
# gone unnoticed until the first assertion that did. The header above claims
# this file is "a statement about the real wire format"; that is only true if
# the bytes survive the read.
$FixtureText = Get-Content -LiteralPath $FixturePath -Raw -Encoding UTF8

$failures = 0
$checks = 0

function New-Manifest {
    # Re-parse rather than clone: PS 5.1 has no deep-copy for PSCustomObject,
    # and a shallow copy would let one case's mutation leak into the next.
    return ($script:FixtureText | ConvertFrom-Json)
}

function Assert-Equal {
    param([string]$Name, $Expected, $Actual)
    $script:checks++
    $e = if ($null -eq $Expected) { "<null>" } else { "$Expected" }
    $a = if ($null -eq $Actual) { "<null>" } else { "$Actual" }
    if ($e -eq $a) {
        Write-Host "  ok   $Name" -ForegroundColor DarkGray
    } else {
        Write-Host "  FAIL $Name -- expected '$e', got '$a'" -ForegroundColor Red
        $script:failures++
    }
}

function Assert-True {
    param([string]$Name, $Condition)
    Assert-Equal -Name $Name -Expected "True" -Actual ([bool]$Condition).ToString()
}

function Get-Row {
    param($Result, [string]$Id)
    return @($Result.Rows | Where-Object { $_.Id -eq $Id })[0]
}

function Set-Rung {
    param($Manifest, [string]$Id, [string]$Rung)
    foreach ($r in $Manifest.rows) { if ($r.id -eq $Id) { $r.rung = $Rung } }
    return $Manifest
}

function Remove-Row {
    param($Manifest, [string]$Id)
    $Manifest.rows = @($Manifest.rows | Where-Object { $_.id -ne $Id })
    return $Manifest
}

function Add-Row {
    # A NAMED mutation for a row the 2026-09-02 fixture predates (session_cli,
    # added 2026-09-27). Same wire shape as every fixture row.
    param($Manifest, [string]$Id, [string]$Rung)
    $row = [PSCustomObject]@{ id = $Id; rung = $Rung; rejected = $null; resolved_path = $null; detail = $null; note = $null }
    $Manifest.rows = @($Manifest.rows) + @($row)
    return $Manifest
}

Write-Host ""
Write-Host "test-parity-diff: classifier over the real 2026-09-02 manifest sample"
Write-Host ""

# ---------------------------------------------------------------------------
# 0. The fixture is what the rest of the file assumes it is.
# ---------------------------------------------------------------------------
Write-Host "[0] fixture shape"
$fx = New-Manifest
Assert-Equal "fixture schema_version"      1 $fx.schema_version
Assert-Equal "fixture row count"           9 (@($fx.rows).Count)
Assert-Equal "fixture unknown row count"   8 (@($fx.rows | Where-Object { $_.rung -eq 'unknown' }).Count)
Assert-Equal "fixture workspace_root rung" "operator_checkout" (Get-Row (Compare-CapabilityManifests -Dev $fx -Published $fx) 'workspace_root').DevRung

# ---------------------------------------------------------------------------
# 1. TWO COPIES -> zero differences, and the eight unknowns are UNOBSERVED.
#    This is the false-green case. If `unknown == unknown` ever counts as
#    agreement, `comparable` reads 9 here instead of 1 and the tool starts
#    certifying parity it never measured.
# ---------------------------------------------------------------------------
Write-Host "[1] identical manifests"
$r1 = Compare-CapabilityManifests -Dev (New-Manifest) -Published (New-Manifest)
Assert-Equal "schema not refused"        $false $r1.SchemaRefusal
Assert-Equal "parity_defects"            0 $r1.ParityDefectCount
Assert-Equal "match"                     1 $r1.MatchCount
Assert-Equal "comparable"                1 $r1.ComparableCount
Assert-Equal "unobserved"                8 $r1.UnobservedCount
Assert-Equal "expected_differences"      0 $r1.ExpectedDiffCount
Assert-Equal "only_in_dev"               0 $r1.OnlyInDevCount
Assert-Equal "only_in_published"         0 $r1.OnlyInPublishedCount
Assert-Equal "unknown row disposition"   "unobserved" (Get-Row $r1 'fleet_commands').Disposition
Assert-Equal "rows partition the union"  9 (@($r1.Rows).Count)

# The verdict line must not let "0 defects" stand alone on 1 comparable row.
$v1 = Format-ParityVerdictLine -Result $r1
Assert-True  "verdict warns THIN OBSERVATION" ($v1 -match 'THIN OBSERVATION')
Assert-True  "verdict states the denominator" ($v1 -match 'comparable=1')
Assert-True  "verdict states unobserved"      ($v1 -match 'unobserved=8')

# ---------------------------------------------------------------------------
# 2. THE HAND-MUTATED COPY -> exactly the mutated row, and nothing else.
#    workspace_root operator_checkout -> unresolved is the plan's central true
#    positive: a dev box has $QONTINUI_ROOT, a clean runner does not.
# ---------------------------------------------------------------------------
Write-Host "[2] one row mutated (workspace_root -> unresolved on the published leg)"
$r2 = Compare-CapabilityManifests -Dev (New-Manifest) -Published (Set-Rung (New-Manifest) 'workspace_root' 'unresolved')
Assert-Equal "parity_defects"           1 $r2.ParityDefectCount
Assert-Equal "rung_differs"             1 $r2.RungDifferCount
Assert-Equal "match"                    0 $r2.MatchCount
Assert-Equal "comparable"               1 $r2.ComparableCount
Assert-Equal "unobserved unchanged"     8 $r2.UnobservedCount
Assert-Equal "the defect is that row"   "defect" (Get-Row $r2 'workspace_root').Disposition
Assert-Equal "defect dev rung"          "operator_checkout" (Get-Row $r2 'workspace_root').DevRung
Assert-Equal "defect published rung"    "unresolved" (Get-Row $r2 'workspace_root').PublishedRung

# ---------------------------------------------------------------------------
# 3. A mutation on a row the DEV side never observed is NOT a defect. Half an
#    observation is not a comparison.
# ---------------------------------------------------------------------------
Write-Host "[3] published leg observes a row the dev leg did not"
$r3 = Compare-CapabilityManifests -Dev (New-Manifest) -Published (Set-Rung (New-Manifest) 'fleet_commands' 'embedded')
Assert-Equal "parity_defects"        0 $r3.ParityDefectCount
Assert-Equal "still unobserved"      "unobserved" (Get-Row $r3 'fleet_commands').Disposition
Assert-Equal "unobserved count"      8 $r3.UnobservedCount
Assert-Equal "dev side not observed" $false (Get-Row $r3 'fleet_commands').DevObserved
Assert-Equal "pub side observed"     $true  (Get-Row $r3 'fleet_commands').PublishedObserved

# ---------------------------------------------------------------------------
# 4. Three rows observed on both legs and all differing -> exactly 3. Proves the
#    count scales with the mutations and does not saturate at 1.
# ---------------------------------------------------------------------------
Write-Host "[4] three genuinely comparable rows, all differing"
$devA = Set-Rung (Set-Rung (New-Manifest) 'bundled_resources' 'dev_checkout') 'spec_pages' 'operator_checkout'
$pubA = Set-Rung (Set-Rung (Set-Rung (New-Manifest) 'bundled_resources' 'bundle_resource') 'spec_pages' 'embedded') 'workspace_root' 'unresolved'
$r4 = Compare-CapabilityManifests -Dev $devA -Published $pubA
Assert-Equal "parity_defects"  3 $r4.ParityDefectCount
Assert-Equal "comparable"      3 $r4.ComparableCount
Assert-Equal "unobserved"      6 $r4.UnobservedCount
Assert-Equal "bundled_resources is a defect" "defect" (Get-Row $r4 'bundled_resources').Disposition
Assert-Equal "spec_pages is a defect"        "defect" (Get-Row $r4 'spec_pages').Disposition

# ---------------------------------------------------------------------------
# 5. THE SCHEMA GATE. Two formats are not diffable, and the refusal must report
#    parity_defects as <null> -- never 0, which would be a claim.
# ---------------------------------------------------------------------------
Write-Host "[5] schema_version mismatch"
$pubV2 = New-Manifest
$pubV2.schema_version = 2
$r5 = Compare-CapabilityManifests -Dev (New-Manifest) -Published $pubV2
Assert-Equal "refused"                $true  $r5.SchemaRefusal
Assert-Equal "no defect count"        $null  $r5.ParityDefectCount
Assert-Equal "no comparable count"    $null  $r5.ComparableCount
Assert-Equal "no rows diffed"         0      (@($r5.Rows).Count)
Assert-True  "reason names both"      ($r5.SchemaRefusalReason -match '1' -and $r5.SchemaRefusalReason -match '2')
Assert-True  "identity still reported" ($r5.Identity.DevGitSha -eq (New-Manifest).git_sha)
$v5 = Format-ParityVerdictLine -Result $r5
Assert-True  "verdict says refused"   ($v5 -match 'PARITY-REFUSED')
Assert-True  "verdict says n/a"       ($v5 -match 'parity_defects=n/a')
Assert-True  "report text refuses"    ((Format-ParityReportText -Result $r5) -match 'REFUSED')

# A manifest with no schema_version at all is the same refusal, not a guess.
Write-Host "[5b] schema_version absent"
$noSchema = New-Manifest | Select-Object -Property * -ExcludeProperty schema_version
$r5b = Compare-CapabilityManifests -Dev (New-Manifest) -Published $noSchema
Assert-Equal "refused on absence" $true $r5b.SchemaRefusal
Assert-Equal "still no count"     $null $r5b.ParityDefectCount

# ---------------------------------------------------------------------------
# 6. ROSTER DIFFERENCES. Direction matters: dev-only (observed) is a defect,
#    published-only is a roster finding, dev-only (unobserved) is neither.
# ---------------------------------------------------------------------------
Write-Host "[6] rows present on one leg only"
$r6 = Compare-CapabilityManifests -Dev (New-Manifest) -Published (Remove-Row (New-Manifest) 'workspace_root')
Assert-Equal "observed dev-only row is a defect" 1 $r6.ParityDefectCount
Assert-Equal "counted as only_in_dev"            1 $r6.OnlyInDevCount
Assert-Equal "rung_differs stays 0"              0 $r6.RungDifferCount
Assert-Equal "disposition"        "only_in_dev" (Get-Row $r6 'workspace_root').Disposition

$r6b = Compare-CapabilityManifests -Dev (New-Manifest) -Published (Remove-Row (New-Manifest) 'fleet_commands')
Assert-Equal "unobserved dev-only row is NOT a defect" 0 $r6b.ParityDefectCount
Assert-Equal "it is unobserved instead" "only_in_dev_unobserved" (Get-Row $r6b 'fleet_commands').Disposition
Assert-Equal "and counts as unobserved" 8 $r6b.UnobservedCount

$r6c = Compare-CapabilityManifests -Dev (Remove-Row (New-Manifest) 'workspace_root') -Published (New-Manifest)
Assert-Equal "published-only row is not a defect" 0 $r6c.ParityDefectCount
Assert-Equal "counted as only_in_published"       1 $r6c.OnlyInPublishedCount
Assert-Equal "disposition"  "only_in_published" (Get-Row $r6c 'workspace_root').Disposition

# ---------------------------------------------------------------------------
# 7. THE ALLOWLIST. It must excuse the exact designed difference and nothing
#    adjacent, and an excused row must never enter the defect count.
# ---------------------------------------------------------------------------
Write-Host "[7] allowlist"
$allow = @([PSCustomObject]@{
    Id = 'bundled_resources'; DevRung = 'dev_checkout'; PublishedRung = 'bundle_resource'
    Reason = 'test entry: designed debug-vs-release difference'
})
$devB = Set-Rung (New-Manifest) 'bundled_resources' 'dev_checkout'
$pubB = Set-Rung (New-Manifest) 'bundled_resources' 'bundle_resource'
$r7 = Compare-CapabilityManifests -Dev $devB -Published $pubB -Allowlist $allow
Assert-Equal "allowlisted row is not a defect" 0 $r7.ParityDefectCount
Assert-Equal "expected_differences"            1 $r7.ExpectedDiffCount
Assert-Equal "disposition" "expected_difference" (Get-Row $r7 'bundled_resources').Disposition
Assert-Equal "reason carried" "test entry: designed debug-vs-release difference" (Get-Row $r7 'bundled_resources').AllowlistReason
Assert-Equal "still counted as comparable"     2 $r7.ComparableCount

# Rung-pinned: the SAME id with a DIFFERENT published rung is still a defect.
$pubB2 = Set-Rung (New-Manifest) 'bundled_resources' 'unresolved'
$r7b = Compare-CapabilityManifests -Dev $devB -Published $pubB2 -Allowlist $allow
Assert-Equal "a different difference is not excused" 1 $r7b.ParityDefectCount
Assert-Equal "disposition" "defect" (Get-Row $r7b 'bundled_resources').Disposition

# A '*' entry excuses any rung pair on that id.
$allowStar = @([PSCustomObject]@{ Id = 'bundled_resources'; DevRung = '*'; PublishedRung = '*'; Reason = 'wildcard' })
$r7c = Compare-CapabilityManifests -Dev $devB -Published $pubB2 -Allowlist $allowStar
Assert-Equal "wildcard entry excuses" 0 $r7c.ParityDefectCount

# The report always PRINTS the allowlist, empty or not -- a silent allowlist
# would be as dishonest as a missing one.
$text7 = Format-ParityReportText -Result $r7
Assert-True "report prints the allowlist entry" ($text7 -match 'designed debug-vs-release difference')
$text1 = Format-ParityReportText -Result (Compare-CapabilityManifests -Dev (New-Manifest) -Published (New-Manifest) -Allowlist @())
Assert-True "report says the allowlist is empty" ($text1 -match 'allowlist: \(empty\)')
# ...and the SHIPPED allowlist is printed entry by entry, reason included.
$textShipped = Format-ParityReportText -Result $r1
Assert-True "report prints the shipped session_cli entry" ($textShipped -match "allowlist: session_cli  dev='exe_relative_checkout' published='bundle_resource'")
Assert-True "report prints the shipped entry's reason"    ($textShipped -match 'both are working deliveries')

# The three report blocks NO EXISTING CASE RENDERS. Measured with breakpoint
# hit-counts over this suite: the defect-detail lines and the published-only
# line were executed ZERO times, because every result the file handed to
# Format-ParityReportText had no `defect` / `only_in_dev` row and no
# `only_in_published` row. That is why a defect that made those exact lines
# throw survived here undetected -- the gate rendered a report that never
# reached them. The results below already exist further up the file; only the
# assertions are new, so this costs three renders and closes the hole.
$textDefect = Format-ParityReportText -Result $r2
Assert-True "defect block prints the dev leg"        ($textDefect -match 'dev       : operator_checkout')
Assert-True "defect block prints the published leg"  ($textDefect -match 'published : unresolved')
# workspace_root carries resolved_path in the fixture, so this also covers the
# TRUE arm of the `$(if ($r.DevPath))` suffix on both of those lines.
Assert-True "defect block prints the resolved path"  ($textDefect -match 'operator_checkout  <- /')

# only_in_dev: the published row is ABSENT, which is the other arm -- the
# `<row absent>` sentinel plus the empty-string suffix.
$textOnlyDev = Format-ParityReportText -Result $r6
Assert-True "absent published row is named"          ($textOnlyDev -match 'published : <row absent>')

# only_in_published renders its own block, from a separate line.
$textPubOnly = Format-ParityReportText -Result $r6c
Assert-True "published-only block names the row"     ($textPubOnly -match 'workspace_root: published=operator_checkout')

# ---------------------------------------------------------------------------
# 8. THE SHIPPED ALLOWLIST. Measured 2026-09-02: no CAPABILITY_SPECS row is
#    resolved by a cfg-gated module. Since 2026-09-27 it holds exactly ONE
#    entry, the placement-exclusive session_cli pair, rung-pinned on both
#    sides. And workspace_root must NEVER appear on it -- that row differing
#    is the plan's central finding.
# ---------------------------------------------------------------------------
Write-Host "[8] the shipped allowlist"
Assert-Equal "shipped allowlist holds exactly one entry" 1 (@($ParityExpectedDifferences).Count)
$cliEntry = @($ParityExpectedDifferences | Where-Object { $_.Id -eq 'session_cli' })[0]
Assert-Equal "that entry is session_cli"            "session_cli" $cliEntry.Id
Assert-Equal "its dev rung is pinned"               "exe_relative_checkout" $cliEntry.DevRung
Assert-Equal "its published rung is pinned"        "bundle_resource" $cliEntry.PublishedRung
Assert-Equal "no shipped entry is a wildcard" 0 (@($ParityExpectedDifferences | Where-Object { $_.DevRung -eq '*' -or $_.PublishedRung -eq '*' }).Count)
Assert-Equal "workspace_root is never allowlisted" 0 (@($ParityExpectedDifferences | Where-Object { $_.Id -eq 'workspace_root' }).Count)
foreach ($e in @($ParityExpectedDifferences)) {
    Assert-True "allowlist entry '$($e.Id)' carries a reason" (-not [string]::IsNullOrWhiteSpace($e.Reason))
}

# ---------------------------------------------------------------------------
# 8b. session_cli UNDER THE SHIPPED ALLOWLIST. Both legs delivering is the
#     designed difference; a published leg that REFUSED or lacks the sidecar
#     is the defect the row exists to surface, and is never excused.
# ---------------------------------------------------------------------------
Write-Host "[8b] session_cli: designed placement difference vs a missing published CLI"
$devCli = Add-Row (New-Manifest) 'session_cli' 'exe_relative_checkout'
$r8a = Compare-CapabilityManifests -Dev $devCli -Published (Add-Row (New-Manifest) 'session_cli' 'bundle_resource')
Assert-Equal "both delivering is not a defect"  0 $r8a.ParityDefectCount
Assert-Equal "it is the expected difference"    "expected_difference" (Get-Row $r8a 'session_cli').Disposition
Assert-Equal "expected_differences"             1 $r8a.ExpectedDiffCount
Assert-Equal "and it is comparable"             2 $r8a.ComparableCount

$r8b = Compare-CapabilityManifests -Dev (Add-Row (New-Manifest) 'session_cli' 'exe_relative_checkout') -Published (Add-Row (New-Manifest) 'session_cli' 'unresolved')
Assert-Equal "a published unresolved CLI is a defect" 1 $r8b.ParityDefectCount
Assert-Equal "disposition"                            "defect" (Get-Row $r8b 'session_cli').Disposition

# The allowlist is directional: the reverse pair is not the designed one.
$r8c = Compare-CapabilityManifests -Dev (Add-Row (New-Manifest) 'session_cli' 'bundle_resource') -Published (Add-Row (New-Manifest) 'session_cli' 'exe_relative_checkout')
Assert-Equal "the reverse pair is not excused" 1 $r8c.ParityDefectCount

# ---------------------------------------------------------------------------
# 9. THE ZERO-COMPARISON CASE. Every row unknown on both legs: the verdict must
#    say NOTHING WAS COMPARED rather than printing a bare 0.
# ---------------------------------------------------------------------------
Write-Host "[9] nothing observed at all"
$blind = New-Manifest
$blind = Set-Rung $blind 'workspace_root' 'unknown'
$r9 = Compare-CapabilityManifests -Dev $blind -Published (Set-Rung (New-Manifest) 'workspace_root' 'unknown')
Assert-Equal "comparable"     0 $r9.ComparableCount
Assert-Equal "unobserved"     9 $r9.UnobservedCount
Assert-Equal "parity_defects" 0 $r9.ParityDefectCount
$v9 = Format-ParityVerdictLine -Result $r9
Assert-True "verdict refuses to imply parity" ($v9 -match 'NOTHING WAS COMPARED')
Assert-True "verdict says it is not parity"   ($v9 -match 'NOT a statement of parity')

# ---------------------------------------------------------------------------
# 10. The machine artifact carries the three numbers and the denominator.
# ---------------------------------------------------------------------------
Write-Host "[10] machine-readable report object"
$obj = ConvertTo-ParityReportObject -Result $r2 -GeneratedAt "2026-09-02T00:00:00Z" -Observability ([PSCustomObject]@{ door = 'test' })
Assert-Equal "report_kind" "published-build-capability-parity" $obj.report_kind
Assert-Equal "counts.parity_defects"       1 $obj.counts.parity_defects
Assert-Equal "counts.unobserved"           8 $obj.counts.unobserved
Assert-Equal "counts.expected_differences" 0 $obj.counts.expected_differences
Assert-Equal "counts.comparable"           1 $obj.counts.comparable
Assert-Equal "rows carried"                9 (@($obj.rows).Count)
Assert-Equal "generated_at carried" "2026-09-02T00:00:00Z" $obj.generated_at
# It must survive a JSON round-trip at the depth the workflow serializes it.
$round = ($obj | ConvertTo-Json -Depth 10) | ConvertFrom-Json
Assert-Equal "round-trips parity_defects" 1 $round.counts.parity_defects
Assert-Equal "round-trips row disposition" "defect" (@($round.rows | Where-Object { $_.id -eq 'workspace_root' })[0].disposition)
# A refusal serializes parity_defects as null, never 0.
$objR = ConvertTo-ParityReportObject -Result $r5 -GeneratedAt "2026-09-02T00:00:00Z" -Observability ([PSCustomObject]@{ door = 'test' })
Assert-Equal "refusal serializes null"  $null $objR.counts.parity_defects
Assert-Equal "refusal flag serialized"  $true $objR.schema_refused

Write-Host "[11] the filesystem witness rules (Phase 5)"
# A witness is the harness's OWN directory listing. These rules decide when it
# CONTRADICTS the manifest's self-report -- the check that stops this harness
# certifying provisioning it never saw land.
$mObserved = New-Manifest   # the real fixture: every provisioning row is `unknown`
# Give three provisioning rows an observed rung so the "claims units" arm is reachable.
foreach ($row in @($mObserved.rows)) {
    if ($row.id -eq 'fleet_commands')   { $row.rung = 'embedded' }
    if ($row.id -eq 'fleet_skills')     { $row.rung = 'embedded' }
    if ($row.id -eq 'agent_definitions'){ $row.rung = 'operator_checkout' }
}

# (a) observed rung over an EMPTY directory -> a disagreement naming the row.
$witnessEmpty = [PSCustomObject]@{ commands = 0; skills = 0; agents = 0 }
$dis = @(Get-ParitySelfReportDisagreements -Manifest $mObserved -Witness $witnessEmpty)
Assert-True  "empty dirs contradict observed rows" (@($dis).Count -ge 3)
Assert-True  "the kind names the direction" (@($dis | Where-Object { $_.kind -eq 'row_claims_units_but_directory_is_empty' }).Count -ge 3)
Assert-True  "fleet_commands is named"      (@($dis | Where-Object { $_.id -eq 'fleet_commands' }).Count -eq 1)

# (b) a count that could NOT be taken contradicts nothing. `$null` is "could not
# look"; 0 is "looked and found nothing". Conflating them would manufacture
# findings out of an unreadable directory.
$witnessUnknown = [PSCustomObject]@{ commands = $null; skills = $null; agents = $null }
$disU = @(Get-ParitySelfReportDisagreements -Manifest $mObserved -Witness $witnessUnknown)
Assert-Equal "unknown counts contradict nothing" 0 (@($disU).Count)

# (c) files present under a row that took no reading -> the opposite direction.
$mUnknown = New-Manifest    # untouched: every provisioning row is `unknown`
$witnessFull = [PSCustomObject]@{ commands = 73; skills = 12; agents = 9 }
$disR = @(Get-ParitySelfReportDisagreements -Manifest $mUnknown -Witness $witnessFull)
Assert-True  "files under an unknown row are reported" (@($disR | Where-Object { $_.kind -eq 'directory_has_units_but_row_is_unknown' }).Count -ge 1)

# (d) EVERY provisioning row observed, WITH files -> nothing to report.
# Note $mObserved gave only three rows a rung, so against a full listing the
# four still-`unknown` rows legitimately disagree (arm (c)) -- which is why this
# case needs its own fully-observed manifest rather than reusing that one. The
# first version of this assertion got that wrong and the rule was right.
$mAllObserved = New-Manifest
foreach ($row in @($mAllObserved.rows)) {
    if ($script:ParityWitnessDirs.ContainsKey($row.id)) { $row.rung = 'embedded' }
}
$disOk = @(Get-ParitySelfReportDisagreements -Manifest $mAllObserved -Witness $witnessFull)
Assert-Equal "agreement reports nothing" 0 (@($disOk).Count)
# ... and the partially-observed manifest against the same listing reports
# exactly the rows that took no reading.
$disPartial = @(Get-ParitySelfReportDisagreements -Manifest $mObserved -Witness $witnessFull)
# TWO. The witness map covers SIX provisioning rows (slash_commands is
# deliberately not one -- it writes nothing into a session workdir), three were
# given a rung above, and of the three remaining one has no row in this fixture
# at all: `agent_skills_registry`, which did not exist when the fixture was
# emitted on 2026-09-02. A capability the manifest does not carry is SKIPPED
# rather than invented, which is the behaviour this number pins -- and it is the
# arm that matters when the published leg is an older release whose roster is
# genuinely shorter.
Assert-Equal "only the unread rows present in the manifest are named" 2 (@($disPartial).Count)
Assert-True  "and all of them in the unknown-row direction" (@($disPartial | Where-Object { $_.kind -eq 'directory_has_units_but_row_is_unknown' }).Count -eq 2)
Assert-Equal "a capability absent from the manifest is never invented" 0 (@($disPartial | Where-Object { $_.id -eq 'agent_skills_registry' }).Count)
# The regression this pins: `slash_commands` is an IMPORT of a checkout's
# commands, not a provision into the session workdir, so the workdir listing can
# neither confirm nor contradict it. Mapping it to .claude/commands made every
# run emit a finding about this harness whose note was false in both halves --
# and the first version of this suite pinned that wrong behaviour as "3".
Assert-Equal "slash_commands is never witnessed against a workdir listing" 0 (@($disPartial | Where-Object { $_.id -eq 'slash_commands' }).Count)
$disAll = @(Get-ParitySelfReportDisagreements -Manifest $mUnknown -Witness $witnessFull)
Assert-Equal "not even when every row is unknown and the dirs are full" 0 (@($disAll | Where-Object { $_.id -eq 'slash_commands' }).Count)

# (e) a missing manifest or witness is UNKNOWN, never a finding.
Assert-Equal "null manifest yields nothing" 0 (@(Get-ParitySelfReportDisagreements -Manifest $null -Witness $witnessFull).Count)
Assert-Equal "null witness yields nothing"  0 (@(Get-ParitySelfReportDisagreements -Manifest $mObserved -Witness $null).Count)

Write-Host "[11a] rung 'unresolved' claims NO units (correction over #1844)"
# agent_runtime returns `unresolved` for agent_definitions BY DESIGN on any
# install with no qontinui-claude-config checkout -- every normal published leg.
# The rule first read it as "claims units", which inverted both answers.
$mUnres = [PSCustomObject]@{ rows = @(
    [PSCustomObject]@{ id = 'fleet_agents';      rung = 'unresolved' },
    [PSCustomObject]@{ id = 'agent_definitions'; rung = 'unresolved' }) }
Assert-Equal "unresolved over 0 files is consistent, not a finding" 0 (@(Get-ParitySelfReportDisagreements -Manifest $mUnres -Witness @{ agents = 0 }).Count)
$disUn = @(Get-ParitySelfReportDisagreements -Manifest $mUnres -Witness @{ agents = 5 })
Assert-Equal "unresolved over N files is a disagreement, once per row" 2 $disUn.Count
Assert-True  "named in the unresolved direction" (@($disUn | Where-Object { $_.kind -eq 'directory_has_units_but_row_is_unresolved' }).Count -eq 2)
Assert-Equal "and never as 'row claims units'" 0 (@($disUn | Where-Object { $_.kind -eq 'row_claims_units_but_directory_is_empty' }).Count)
# The NORMAL published shape: fleet_agents writes its embedded floor into the
# SAME .claude/agents that agent_definitions reports `unresolved` for. Those
# files are explained by the sibling row and contradict nothing -- a rule that
# flagged them would fire on every published leg in the mirror direction.
$mPubShape = [PSCustomObject]@{ rows = @(
    [PSCustomObject]@{ id = 'fleet_agents';      rung = 'embedded' },
    [PSCustomObject]@{ id = 'agent_definitions'; rung = 'unresolved' }) }
Assert-Equal "unresolved beside a sibling that claims the dir: N files consistent" 0 (@(Get-ParitySelfReportDisagreements -Manifest $mPubShape -Witness @{ agents = 9 }).Count)
$disPubEmpty = @(Get-ParitySelfReportDisagreements -Manifest $mPubShape -Witness @{ agents = 0 })
Assert-Equal "...but 0 files still contradicts the sibling's claim, and only it" 1 $disPubEmpty.Count
Assert-Equal "...naming the sibling" 'fleet_agents' $disPubEmpty[0].id
Assert-True  "unresolved is NOT an unobserved rung for the parity count" (Test-ParityRungObserved -Rung 'unresolved')

Write-Host "[11c] the witness is gated on the terminal door (correction over #1844)"
# Commands and skills are written only by POST /terminals. When that door did
# not create a terminal, the empty directory is "never provisioned" -- the
# counts must be UNKNOWN, or a refusal becomes a fabricated parity defect.
$wtRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("pp-witness-" + [System.Guid]::NewGuid().ToString('N').Substring(0, 8))
$wdDev = Join-Path $wtRoot 'dev'; $wdPub = Join-Path $wtRoot 'pub'
New-Item -ItemType Directory -Force -Path (Join-Path $wdDev '.claude/commands') | Out-Null
New-Item -ItemType Directory -Force -Path $wdPub | Out-Null
foreach ($n in 1..3) { Set-Content -LiteralPath (Join-Path $wdDev ".claude/commands/c$n.md") -Value 'x' }
try {
    $wDev = Get-ParityProvisionWitness -Workdir $wdDev -TerminalOutcome 'created(abc)'
    Assert-Equal "created terminal: commands counted" 3 $wDev.commands
    Assert-Equal "created terminal: a missing skills dir is a real 0" 0 $wDev.skills
    Assert-Equal "no probe workdir: agents UNKNOWN" $null $wDev.agents
    $wDevClose = Get-ParityProvisionWitness -Workdir $wdDev -TerminalOutcome 'created(abc);close_failed'
    Assert-Equal "created-then-close-failed still provisioned" 3 $wDevClose.commands

    $wPubRefused = Get-ParityProvisionWitness -Workdir $wdPub -TerminalOutcome 'unknown(terminal_create_refused: HTTP 403 x)'
    Assert-Equal "refused terminal: commands UNKNOWN, not 0" $null $wPubRefused.commands
    Assert-Equal "refused terminal: skills UNKNOWN, not 0"   $null $wPubRefused.skills
    Assert-Equal "omitted outcome: commands UNKNOWN" $null (Get-ParityProvisionWitness -Workdir $wdDev).commands

    # Refused on ONE leg: the reviewer's repro shape (dev=41 vs a refused published leg).
    Assert-Equal "refused on the published leg -> unknown, never provisioned_fewer" 'unknown(no_command_listing_on_the_published_leg)' (Get-ParitySlashCommandsStatus -DevWitness ([PSCustomObject]@{ commands = 41; skills = 12; agents = 9 }) -PublishedWitness $wPubRefused)
    Assert-Equal "refused on the dev leg -> unknown naming dev" 'unknown(no_command_listing_on_the_dev_leg)' (Get-ParitySlashCommandsStatus -DevWitness $wPubRefused -PublishedWitness $wDev)
    # Refused on BOTH legs: not none_provisioned.
    $wDevRefused = Get-ParityProvisionWitness -Workdir $wdPub -TerminalOutcome 'not_attempted'
    Assert-Equal "refused on both legs -> unknown, never none_provisioned" 'unknown(no_command_listing_on_either_leg)' (Get-ParitySlashCommandsStatus -DevWitness $wDevRefused -PublishedWitness $wPubRefused)
    # And a refused leg's witness contradicts no manifest row.
    Assert-Equal "a refused leg's witness raises no self-report finding" 0 (@(Get-ParitySelfReportDisagreements -Manifest $mObserved -Witness $wPubRefused).Count)
} finally {
    Remove-Item -LiteralPath $wtRoot -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "[11b] a HASHTABLE witness behaves exactly like a PSCustomObject one"
# The shape this file's own doc comments describe. The two need different
# accessors and the difference is SILENT -- on a hashtable
# `$w.PSObject.Properties.Name` enumerates IsReadOnly/Keys/Count/... and never
# the keys, so a rule written for one shape reports "nothing to say" for the
# other. That is a false clean, which is the one failure class this file exists
# to prevent, so both shapes are pinned here.
$htEmpty = @{ commands = 0; skills = 0; agents = 0 }
$htFull  = @{ commands = 73; skills = 12; agents = 9 }
Assert-Equal "hashtable: empty dirs contradict observed rows" (@(Get-ParitySelfReportDisagreements -Manifest $mObserved -Witness $witnessEmpty).Count) (@(Get-ParitySelfReportDisagreements -Manifest $mObserved -Witness $htEmpty).Count)
Assert-Equal "hashtable: full dirs agree with observed rows"  (@(Get-ParitySelfReportDisagreements -Manifest $mAllObserved -Witness $witnessFull).Count) (@(Get-ParitySelfReportDisagreements -Manifest $mAllObserved -Witness $htFull).Count)
Assert-Equal "hashtable: the verdict resolves, not unknown" 'provisioned_fewer(dev=93,published=72)' (Get-ParitySlashCommandsStatus -DevWitness @{ commands = 93 } -PublishedWitness @{ commands = 72 })
# An absent key is still "could not look", on either shape.
Assert-Equal "hashtable: an absent key is unknown, not zero" 'unknown(no_command_listing_on_the_dev_leg)' (Get-ParitySlashCommandsStatus -DevWitness @{ skills = 1 } -PublishedWitness @{ commands = 5 })
Assert-Equal 'hashtable: a present $null key is unknown too' 'unknown(no_command_listing_on_the_published_leg)' (Get-ParitySlashCommandsStatus -DevWitness @{ commands = 5 } -PublishedWitness @{ commands = $null })

Write-Host "[12] the typed slash-commands verdict (Phase 5, exit (d))"
Assert-Equal "equal counts" 'provisioned_equal' (Get-ParitySlashCommandsStatus -DevWitness ([PSCustomObject]@{ commands = 73 }) -PublishedWitness ([PSCustomObject]@{ commands = 73 }))
# The metric's baseline defect: a dev box with a checkout resolves more commands
# than a published install carrying only its embedded bundle.
Assert-Equal "published fewer is typed WITH both counts" 'provisioned_fewer(dev=93,published=72)' (Get-ParitySlashCommandsStatus -DevWitness ([PSCustomObject]@{ commands = 93 }) -PublishedWitness ([PSCustomObject]@{ commands = 72 }))
Assert-Equal "published MORE is stated, not folded" 'provisioned_more(dev=10,published=11)' (Get-ParitySlashCommandsStatus -DevWitness ([PSCustomObject]@{ commands = 10 }) -PublishedWitness ([PSCustomObject]@{ commands = 11 }))
Assert-Equal "both zero"  'none_provisioned' (Get-ParitySlashCommandsStatus -DevWitness ([PSCustomObject]@{ commands = 0 }) -PublishedWitness ([PSCustomObject]@{ commands = 0 }))
# An unreadable leg is UNKNOWN and names WHICH leg -- never 0, never 'equal'.
Assert-Equal "dev unreadable"       'unknown(no_command_listing_on_the_dev_leg)'       (Get-ParitySlashCommandsStatus -DevWitness ([PSCustomObject]@{ commands = $null }) -PublishedWitness ([PSCustomObject]@{ commands = 5 }))
Assert-Equal "published unreadable" 'unknown(no_command_listing_on_the_published_leg)' (Get-ParitySlashCommandsStatus -DevWitness ([PSCustomObject]@{ commands = 5 }) -PublishedWitness ([PSCustomObject]@{ commands = $null }))
Assert-Equal "neither leg"          'unknown(no_command_listing_on_either_leg)'        (Get-ParitySlashCommandsStatus -DevWitness $null -PublishedWitness $null)

Write-Host "[13] ConvertFrom-VerbatimPath (the CI defect that lost both control legs)"
# Rust's std::fs::canonicalize hands back a VERBATIM path on Windows, and the
# provision-probe's `provisioned_into` is exactly that. PowerShell cannot carry
# it: `Join-Path` throws *"the value of argument \"drive\" is null"*. Measured on
# run 36615500004 -- the throw escaped the witness, was caught as a manifest-read
# failure, and reported NEGATIVE-CONTROL-UNAVAILABLE for BOTH legs, so the
# instrument certified nothing while the job stayed green.
Assert-Equal "verbatim drive path is stripped"  'D:\a\_temp\x\probe-abc' (ConvertFrom-VerbatimPath '\\?\D:\a\_temp\x\probe-abc')
Assert-Equal "verbatim UNC becomes a real UNC"  '\\srv\share\x'          (ConvertFrom-VerbatimPath '\\?\UNC\srv\share\x')
Assert-Equal "an ordinary path is untouched"    'D:\plain\path'            (ConvertFrom-VerbatimPath 'D:\plain\path')
Assert-Equal "a posix path is untouched"        '/tmp/x'                   (ConvertFrom-VerbatimPath '/tmp/x')
Assert-Equal "empty in, empty out"              ''                         (ConvertFrom-VerbatimPath '')
# `[string]$Path` coerces $null to '', which is the safe landing: every caller
# guards with IsNullOrWhiteSpace, so '' and $null behave identically downstream.
Assert-Equal "null in, empty out"               ''                         (ConvertFrom-VerbatimPath $null)
# The property that actually matters: the OUTPUT no longer carries the prefix
# that PowerShell cannot parse. Asserted as a STRING, because `Join-Path`
# resolves drive qualifiers against the LOCAL platform -- a `D:` assertion throws
# "Cannot find drive" on Linux and would make this suite platform-bound, while
# the whole point of it is that it runs anywhere and gates on 5.1.
Assert-True  "the result carries no verbatim prefix" (-not ((ConvertFrom-VerbatimPath '\\?\D:\a\x') -like '\\?\*'))
Assert-Equal "and is the plain drive path"      'D:\a\x' (ConvertFrom-VerbatimPath '\\?\D:\a\x')

# The Join-Path half is the real regression, so pin it where it is meaningful:
# on Windows, where the drive exists and the raw form is what threw in CI.
if ($IsWindows -or $env:OS -eq 'Windows_NT') {
    $joined = Join-Path (ConvertFrom-VerbatimPath ('\\?\' + $env:SystemDrive + '\a\x')) '.claude'
    Assert-True  "the normalized result joins on Windows" ($joined -like '*a\x\.claude')
    $threw = $false
    try { $null = Join-Path ('\\?\' + $env:SystemDrive + '\a\x') '.claude' } catch { $threw = $true }
    Assert-True  "and the raw verbatim form still throws" $threw
} else {
    Write-Host "  skip Join-Path arms (not Windows; drive qualifiers do not resolve here)" -ForegroundColor DarkGray
}

# ---------------------------------------------------------------------------
# 14. THE SUMMARY HEADLINE (plan
#     2026-09-20-published-runner-parity-count-comes-from-a-run-not-from-reports,
#     Phase 2). The job summary is what a human reads, and its first line is
#     often ALL they read. A line that says "parity" while rows went unobserved
#     must carry the unobserved count on that same line -- otherwise a heading
#     reading "capability parity" above `parity_defects = 0` certifies parity
#     over a run that compared nothing.
# ---------------------------------------------------------------------------
Write-Host "[14] the summary never says 'parity' without the unobserved count"

# Every line naming parity must also name the unobserved count, on that line.
function Get-ParityLinesMissingUnobserved {
    param([string[]]$Lines, [int]$Unobserved)
    $n = [regex]::Escape("$Unobserved")
    return @($Lines | Where-Object {
        ($_ -match '(?i)parity') -and
        -not ($_ -match "(?i)(\b$n\b[^.|]{0,40}unobserved|unobserved[^.|]{0,20}\b$n\b)")
    })
}

# The all-unknown pair: r9, every one of the 9 rows `unknown` on both legs.
$md9 = @(Format-ParitySummaryMarkdown -Result $r9)
$bare9 = @(Get-ParityLinesMissingUnobserved -Lines $md9 -Unobserved 9)
Assert-True  "the all-unknown summary names parity at all (the check is not vacuous)" (@($md9 | Where-Object { $_ -match '(?i)parity' }).Count -ge 1)
Assert-Equal "no line says parity without the unobserved count (all rows unknown)" 0 $bare9.Count
foreach ($b in $bare9) { Write-Host "         offending line: $b" -ForegroundColor Red }
Assert-True  "first line says UNOBSERVED (9 of 9)" ($md9[0] -match 'UNOBSERVED' -and $md9[0] -match '\b9 of 9\b')

# The threshold. 8 (identical fixture copies) and exactly 7 are THIN; 6 is not,
# but still states its count.
$md1 = @(Format-ParitySummaryMarkdown -Result $r1)
Assert-True  "unobserved=8: first line says UNOBSERVED" ($md1[0] -match 'UNOBSERVED' -and $md1[0] -match '\b8 of 9\b')
Assert-Equal "unobserved=8: no bare parity line" 0 (@(Get-ParityLinesMissingUnobserved -Lines $md1 -Unobserved 8)).Count
$r7u = Compare-CapabilityManifests -Dev (Set-Rung (New-Manifest) 'bundled_resources' 'dev_checkout') -Published (Set-Rung (New-Manifest) 'bundled_resources' 'bundle_resource')
Assert-Equal "fixture for the boundary has exactly 7 unobserved" 7 $r7u.UnobservedCount
$md7 = @(Format-ParitySummaryMarkdown -Result $r7u)
Assert-True  "unobserved=7 (the threshold): first line says UNOBSERVED" ($md7[0] -match 'UNOBSERVED' -and $md7[0] -match '\b7 of 9\b')
$md4 = @(Format-ParitySummaryMarkdown -Result $r4)
Assert-True  "unobserved=6: first line is not THIN" (-not ($md4[0] -match 'THIN'))
Assert-True  "unobserved=6: first line still states the count" ($md4[0] -match '\b6 of 9 rows unobserved\b')
Assert-Equal "unobserved=6: no bare parity line" 0 (@(Get-ParityLinesMissingUnobserved -Lines $md4 -Unobserved 6)).Count
# A refusal compared NOTHING, so every row is unobserved -- and it says so with
# the count, not as a carve-out from the rule.
$md5 = @(Format-ParitySummaryMarkdown -Result $r5)
Assert-True  "refusal: first line says REFUSED" ($md5[0] -match 'REFUSED')
Assert-True  "refusal: first line says 9 of 9 rows UNOBSERVED (refused)" ($md5[0] -match '\b9 of 9 rows UNOBSERVED \(refused\)')
Assert-Equal "refusal: no bare parity line" 0 (@(Get-ParityLinesMissingUnobserved -Lines $md5 -Unobserved 9)).Count

# A comparison that could not run at all (published-parity.ps1's exit-2 paths)
# writes one line, and it too states the unobserved state and the skew.
$provX = New-ParityProvenance -SkewCommits 'unknown(shallow_clone)'
$lineX = Format-ParityUnavailableSummaryLine -Reason 'manifest_read_failed' -Provenance $provX
Assert-True  "unavailable: the line says every row is UNOBSERVED" ($lineX -match 'all rows UNOBSERVED')
Assert-True  "unavailable: the line names the reason"            ($lineX -match 'UNAVAILABLE \(manifest_read_failed\)')
Assert-True  "unavailable: the line carries skew_commits verbatim" ($lineX -match 'skew_commits: unknown\(shallow_clone\)')
Assert-True  "unavailable: never a parity_defects number"         ($lineX -match 'parity_defects: n/a')

# ---------------------------------------------------------------------------
# 15. PROVENANCE PASSES THROUGH UNTOUCHED. skew_commits states what the count
#     was computed across; the comparator carries it and never changes it --
#     and the one value nothing may invent is 0.
# ---------------------------------------------------------------------------
Write-Host "[15] provenance and skew_commits survive the comparator"
$sibs = @([PSCustomObject]@{ repo = 'qontinui/qontinui-schemas'; path = 'x'; sha = ('a' * 40) })
$prov = New-ParityProvenance -DevSha ('d' * 40) -PublishedTag 'v1.0.11' -PublishedSha ('e' * 40) -SkewCommits 736 `
    -RunId '123' -RunEvent 'schedule' -GeneratedAt '2026-10-04T07:00:00Z' -ManifestAxis 'observed' `
    -BehaviouralAxis 'unknown(not_yet_measured)' -Siblings $sibs
Assert-Equal "constructor keeps the count" 736 $prov.skew_commits
$objP = ConvertTo-ParityReportObject -Result $r2 -GeneratedAt '2026-10-04T07:00:00Z' -Observability ([PSCustomObject]@{ door = 'test' }) -Provenance $prov
Assert-True  "the report carries the SAME provenance object" ([object]::ReferenceEquals($prov, $objP.provenance))
Assert-Equal "report_version bumped for the new block" 2 $objP.report_version
Assert-Equal "skew_commits survives the comparator" 736 $objP.provenance.skew_commits
$roundP = ($objP | ConvertTo-Json -Depth 10) | ConvertFrom-Json
Assert-Equal "skew_commits survives the JSON round-trip" 736 $roundP.provenance.skew_commits
Assert-Equal "manifest axis round-trips" 'observed' $roundP.provenance.axes.manifest
Assert-Equal "sibling sha round-trips, as recorded" ('a' * 40) (@($roundP.provenance.siblings)[0].sha)
Assert-True  "siblings are never claimed same-SHA" ($roundP.provenance.siblings_note -match 'NOT at the release')
Assert-Equal "the counts are untouched by the block" 1 $roundP.counts.parity_defects
$mdP = @(Format-ParitySummaryMarkdown -Result $r2 -Provenance $prov)
Assert-True  "the summary states skew_commits verbatim" (@($mdP | Where-Object { $_ -match 'skew_commits: 736\b' }).Count -eq 1)

# Same-SHA: 0 is a reading, and survives as 0 -- not null, not unknown.
$prov0 = New-ParityProvenance -DevSha ('d' * 40) -PublishedTag 'v1.0.12' -PublishedSha ('d' * 40) -SkewCommits 0 -ManifestAxis 'observed'
$round0 = (ConvertTo-ParityReportObject -Result $r2 -GeneratedAt 'x' -Provenance $prov0 | ConvertTo-Json -Depth 10) | ConvertFrom-Json
Assert-Equal "a same-SHA 0 survives as 0" '0' "$($round0.provenance.skew_commits)"
# Not computed is UNKNOWN, never 0.
$provN = New-ParityProvenance -DevSha ('d' * 40) -PublishedTag 'v1.0.11' -SkewCommits $null
Assert-Equal "a null skew becomes unknown, never 0" 'unknown(not_computed)' $provN.skew_commits
$provU = New-ParityProvenance -SkewCommits 'unknown(shallow_clone)'
$roundU = (ConvertTo-ParityReportObject -Result $r9 -GeneratedAt 'x' -Provenance $provU | ConvertTo-Json -Depth 10) | ConvertFrom-Json
Assert-Equal "an unknown skew survives verbatim" 'unknown(shallow_clone)' $roundU.provenance.skew_commits
Assert-True  "and the summary says unknown, not 0" (@(Format-ParitySummaryMarkdown -Result $r9 -Provenance $provU | Where-Object { $_ -match 'skew_commits: unknown\(shallow_clone\)' }).Count -eq 1)
Assert-Equal "a garbage skew string is unknown, never passed through" 'unknown(unparseable_skew: lots)' (New-ParityProvenance -SkewCommits 'lots').skew_commits
Assert-Equal "an unrecorded axis is unknown" 'unknown(not_recorded)' $provN.axes.manifest

# The manifest axis is `observed` only when something was compared.
Assert-Equal "axis: comparable rows -> observed"     'observed' (Get-ParityManifestAxis -Result $r2)
Assert-Equal "axis: all unknown -> unknown"          'unknown(no_comparable_rows)' (Get-ParityManifestAxis -Result $r9)
Assert-True  "axis: schema refusal -> unknown"       ((Get-ParityManifestAxis -Result $r5) -like 'unknown(schema_refused*')

# The behavioural stamp touches ONE field -- skew_commits least of all.
$stamped = ($objP | ConvertTo-Json -Depth 10) | ConvertFrom-Json
Assert-True  "stamp applies"                          (Set-ParityBehaviouralAxis -Report $stamped -Axis 'observed')
Assert-Equal "behavioural axis stamped"               'observed' $stamped.provenance.axes.behavioural
Assert-Equal "stamping leaves skew_commits alone"     736 $stamped.provenance.skew_commits
Assert-Equal "stamping leaves the manifest axis alone" 'observed' $stamped.provenance.axes.manifest
$null = Set-ParityBehaviouralAxis -Report $stamped -Axis 'fine i guess'
Assert-Equal "a malformed axis is recorded as unknown" 'unknown(unparseable_axis_value: fine i guess)' $stamped.provenance.axes.behavioural
Assert-Equal "a report with no provenance is not stamped" $false (Set-ParityBehaviouralAxis -Report ([PSCustomObject]@{ report_version = 1 }) -Axis 'observed')

# The file-to-file stamp the workflow step calls: atomic, one field, honest
# about a missing artifact or a missing axis file.
$stRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("pp-stamp-" + [System.Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force -Path $stRoot | Out-Null
try {
    $jp = Join-Path $stRoot 'published-parity.json'
    $ap = Join-Path $stRoot 'behavioural-axis.txt'
    [System.IO.File]::WriteAllText($jp, ($objP | ConvertTo-Json -Depth 10), (New-Object System.Text.UTF8Encoding($false)))
    Assert-Equal "stamp file: no axis file still stamps" 'stamped' (Update-ParityReportBehaviouralAxis -JsonPath $jp -AxisPath $ap)
    $after1 = Get-Content -LiteralPath $jp -Raw | ConvertFrom-Json
    Assert-Equal "stamp file: a missing axis file is unknown, never observed" 'unknown(behavioural_step_did_not_report)' $after1.provenance.axes.behavioural
    Set-Content -LiteralPath $ap -Value 'observed'
    Assert-Equal "stamp file: stamped" 'stamped' (Update-ParityReportBehaviouralAxis -JsonPath $jp -AxisPath $ap)
    $after2 = Get-Content -LiteralPath $jp -Raw | ConvertFrom-Json
    Assert-Equal "stamp file: observed written"           'observed' $after2.provenance.axes.behavioural
    Assert-Equal "stamp file: skew_commits untouched"     736 $after2.provenance.skew_commits
    Assert-Equal "stamp file: counts untouched"           1 $after2.counts.parity_defects
    Assert-Equal "stamp file: no .tmp left behind"        $false (Test-Path -LiteralPath "$jp.tmp")
    Assert-Equal "stamp file: a missing artifact is reported" 'no_artifact' (Update-ParityReportBehaviouralAxis -JsonPath (Join-Path $stRoot 'absent.json') -AxisPath $ap)
    $old = Join-Path $stRoot 'v1.json'
    Set-Content -LiteralPath $old -Value '{"report_version":1}'
    Assert-Equal "stamp file: a v1 artifact is left alone" 'no_axes_block' (Update-ParityReportBehaviouralAxis -JsonPath $old -AxisPath $ap)
} finally {
    Remove-Item -LiteralPath $stRoot -Recurse -Force -ErrorAction SilentlyContinue
}

# ---------------------------------------------------------------------------
# 16. skew_commits FROM A REAL GIT REPO. The shapes CI meets: a tag behind
#     HEAD, the tag AT HEAD (the same-SHA dispatch), a tag the checkout does
#     not have, and a shallow clone -- where `rev-list --count` exits 0 with an
#     undercount, so the only honest answer is unknown.
# ---------------------------------------------------------------------------
Write-Host "[16] skew_commits over a real repository"
function Invoke-TestGit {
    param([string]$Dir, [string[]]$GitArgs)
    $ErrorActionPreference = 'Continue'
    $o = & git -C $Dir -c user.name=parity-test -c user.email=parity-test@invalid -c commit.gpgsign=false -c tag.gpgsign=false @GitArgs 2>&1
    if ($LASTEXITCODE -ne 0) { throw "git $($GitArgs -join ' ') failed: $($o | Out-String)" }
}
$gitCmd = Get-Command git -ErrorAction SilentlyContinue
Assert-True "git is available (these cases pin the never-0 rule and must not skip)" ($null -ne $gitCmd)
if ($null -ne $gitCmd) {
    $gRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("pp-skew-" + [System.Guid]::NewGuid().ToString('N').Substring(0, 8))
    $repo = Join-Path $gRoot 'qontinui-runner'
    New-Item -ItemType Directory -Force -Path $repo | Out-Null
    try {
        Invoke-TestGit $repo @('init', '-q')
        Invoke-TestGit $repo @('commit', '-q', '--allow-empty', '-m', 'release commit')
        Invoke-TestGit $repo @('tag', 'v9.9.9')
        $sk0 = Get-ParitySkewProvenance -RepoDir $repo -Tag 'v9.9.9'
        Assert-Equal "tag AT HEAD: skew 0 (the same-SHA leg)" '0' "$($sk0.skew_commits)"
        Assert-Equal "tag AT HEAD: dev_sha == published_sha" $sk0.dev_sha $sk0.published_sha

        Invoke-TestGit $repo @('commit', '-q', '--allow-empty', '-m', 'after 1')
        Invoke-TestGit $repo @('commit', '-q', '--allow-empty', '-m', 'after 2')
        Invoke-TestGit $repo @('commit', '-q', '--allow-empty', '-m', 'after 3')
        $sk3 = Get-ParitySkewProvenance -RepoDir $repo -Tag 'v9.9.9'
        Assert-Equal "three commits past the tag: skew 3" '3' "$($sk3.skew_commits)"
        Assert-True  "dev_sha is a full sha" ($sk3.dev_sha -match '^[0-9a-f]{40}$')
        Assert-True  "published_sha differs from dev_sha" ($sk3.published_sha -ne $sk3.dev_sha)

        $skMissing = Get-ParitySkewProvenance -RepoDir $repo -Tag 'v0.0.0-not-here'
        Assert-Equal "missing tag: unknown, never 0" 'unknown(tag_not_in_checkout: v0.0.0-not-here)' $skMissing.skew_commits
        Assert-Equal "missing tag: published_sha unknown too" 'unknown(tag_not_in_checkout: v0.0.0-not-here)' $skMissing.published_sha
        Assert-Equal "no tag given: unknown" 'unknown(no_published_tag)' (Get-ParitySkewProvenance -RepoDir $repo -Tag '').skew_commits

        # The shallow clone: the tag IS present (fetched by name), HEAD is not
        # the tag, and the history between them is not -- the CI default.
        $shallow = Join-Path $gRoot 'shallow'
        # A file:// URL, not a plain path: git ignores --depth for a local-path clone
        # (it hardlinks the whole object store), which would make this clone deep.
        # ::new, not a [System.Uri] cast: the cast parses a POSIX path as RELATIVE
        # and AbsoluteUri then reads empty.
        $url = [System.Uri]::new((Resolve-Path -LiteralPath $repo).Path).AbsoluteUri
        Invoke-TestGit $gRoot @('clone', '-q', '--depth', '1', '--no-tags', $url, $shallow)
        Invoke-TestGit $shallow @('fetch', '-q', '--depth', '1', 'origin', 'tag', 'v9.9.9')
        $skShallow = Get-ParitySkewProvenance -RepoDir $shallow -Tag 'v9.9.9'
        Assert-True  "shallow: the tag was resolved (the arm under test is reached)" ($skShallow.published_sha -match '^[0-9a-f]{40}$')
        Assert-Equal "shallow: unknown, never the undercount" 'unknown(shallow_clone)' $skShallow.skew_commits

        $notRepo = Join-Path $gRoot 'not-a-repo'
        New-Item -ItemType Directory -Force -Path $notRepo | Out-Null
        $skNone = Get-ParitySkewProvenance -RepoDir $notRepo -Tag 'v9.9.9'
        Assert-True  "not a checkout: dev_sha unknown" ($skNone.dev_sha -like 'unknown(dev_checkout_unreadable*')
        Assert-True  "not a checkout: skew never 0" ("$($skNone.skew_commits)" -ne '0')

        # Siblings are read where checkout-sibling puts them: beside the runner.
        $sibRows = @(Get-ParitySiblingProvenance -RepoRoot $repo -Repos @('qontinui/shallow', 'qontinui/ui-bridge'))
        Assert-True  "a present sibling is recorded by sha" ((@($sibRows | Where-Object { $_.repo -eq 'qontinui/shallow' })[0].sha) -match '^[0-9a-f]{40}$')
        Assert-Equal "an absent sibling is unknown" 'unknown(not_checked_out)' (@($sibRows | Where-Object { $_.repo -eq 'qontinui/ui-bridge' })[0].sha)

        # ANCESTRY. `rev-list --count <tag>..HEAD` is a skew only when the tag
        # is an ancestor of HEAD. HEAD BEHIND the tag counts 0 while the SHAs
        # differ (a fabricated same-SHA reading); diverged histories undercount.
        # Both must be unknown -- never a number.
        Invoke-TestGit $repo @('tag', 'v9.9.10')            # at the tip: 3 past v9.9.9
        Invoke-TestGit $repo @('checkout', '-q', '--detach', 'v9.9.9')
        $skBehind = Get-ParitySkewProvenance -RepoDir $repo -Tag 'v9.9.10'
        Assert-Equal "HEAD behind the tag: unknown, never 0" 'unknown(tag_not_ancestor_of_head)' $skBehind.skew_commits
        Assert-True  "HEAD behind the tag: not a number" ($skBehind.skew_commits -is [string])
        Assert-True  "HEAD behind the tag: the SHAs really differ" ($skBehind.dev_sha -ne $skBehind.published_sha)
        Assert-Equal "HEAD behind the tag: divergence dev_only" 0 $skBehind.divergence.dev_only_commits
        Assert-Equal "HEAD behind the tag: divergence published_only" 3 $skBehind.divergence.published_only_commits

        Invoke-TestGit $repo @('checkout', '-q', '-b', 'side')
        Invoke-TestGit $repo @('commit', '-q', '--allow-empty', '-m', 'side commit')
        $skSide = Get-ParitySkewProvenance -RepoDir $repo -Tag 'v9.9.10'
        Assert-Equal "diverged history: unknown, never the undercount" 'unknown(tag_not_ancestor_of_head)' $skSide.skew_commits
        Assert-True  "diverged history: not a number" ($skSide.skew_commits -is [string])
        Assert-Equal "diverged history: divergence dev_only" 1 $skSide.divergence.dev_only_commits
        Assert-Equal "diverged history: divergence published_only" 3 $skSide.divergence.published_only_commits
        # ...and the divergence rides in the provenance block, beside -- not
        # inside -- the unknown skew.
        $provD = New-ParityProvenance -SkewCommits $skSide.skew_commits -Divergence $skSide.divergence
        Assert-Equal "divergence carried in provenance" 1 $provD.divergence.dev_only_commits
        Assert-Equal "skew stays unknown beside it" 'unknown(tag_not_ancestor_of_head)' $provD.skew_commits
        # The linear case is unaffected: a tag that IS an ancestor still counts.
        Assert-Equal "linear: a tag that IS an ancestor still counts (1 commit past v9.9.9)" '1' "$((Get-ParitySkewProvenance -RepoDir $repo -Tag 'v9.9.9').skew_commits)"
        Assert-Equal "linear: no divergence recorded" $null (Get-ParitySkewProvenance -RepoDir $repo -Tag 'v9.9.9').divergence
    } finally {
        Remove-Item -LiteralPath $gRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# ---------------------------------------------------------------------------
# [17] Platform helpers (Phase 6B). The dev leg's name and build-dir guard were
# Windows literals; on Linux they would have refused every dev binary.
# ---------------------------------------------------------------------------
Write-Host "[17] platform helpers: dev exe name, build-dir guard, /proc/<pid>/stat"
Assert-Equal "windows dev exe"                 'qontinui-runner.exe' (Get-ParityDevExeName -Platform 'windows')
Assert-Equal "linux dev exe"                   'qontinui-runner'     (Get-ParityDevExeName -Platform 'linux')
Assert-True  "host platform is one of three"   (@('windows', 'linux', 'macos') -contains (Get-ParityHostPlatform))
Assert-True  "backslash build dir accepted"    (Test-ParityDevBuildPath 'D:\a\qontinui-runner\target\debug\qontinui-runner.exe')
Assert-True  "slash build dir accepted"        (Test-ParityDevBuildPath '/home/runner/work/qontinui-runner/target/debug/qontinui-runner')
Assert-True  "release profile accepted"        (Test-ParityDevBuildPath '/w/target/release/qontinui-runner')
Assert-True  "an unpacked prefix is NOT one"   (-not (Test-ParityDevBuildPath '/tmp/prefix/usr/bin/qontinui-runner'))
Assert-True  "an install dir is NOT one"       (-not (Test-ParityDevBuildPath 'C:\Users\u\AppData\Local\Qontinui Runner\qontinui-runner.exe'))
Assert-True  "a target-agent dir is NOT one"   (-not (Test-ParityDevBuildPath '/w/target-agent/debug/qontinui-runner'))
Assert-True  "empty is NOT one"                (-not (Test-ParityDevBuildPath ''))

# A real /proc/<pid>/stat line shape, including a comm with a space and a ')'
# inside it -- the case a naive split on whitespace gets wrong by one field.
$stat = '4242 (WebKit Net)work) S 4100 4242 4100 0 -1 4194560 1234 0 0 0 10 5 0 0 20 0 7 0 987654 123456789 3000 18446744073709551615 1 1 0 0 0 0 0 4096 0 0 0 0 17 3 0 0 0 0 0'
$ps = ConvertFrom-ParityProcStat -Line $stat
Assert-Equal "stat: pid"                       4242     $ps.ProcessId
Assert-Equal "stat: ppid (counted from the LAST paren)" 4100 $ps.ParentProcessId
Assert-Equal "stat: starttime (field 22)"      987654   $ps.CreationDate
Assert-Equal "stat: comm keeps its paren"      'WebKit Net)work' $ps.Name
Assert-Equal "stat: empty line"                $null    (ConvertFrom-ParityProcStat -Line '')
Assert-Equal "stat: truncated line"            $null    (ConvertFrom-ParityProcStat -Line '12 (x) S 1 2')
Assert-Equal "stat: non-numeric pid"           $null    (ConvertFrom-ParityProcStat -Line ('x (y) ' + (@(1..25) -join ' ')))

# ---------------------------------------------------------------------------
# [18] Cross-platform, published side only. The reports are built by the REAL
# pipeline (Compare-CapabilityManifests -> ConvertTo-ParityReportObject) and
# round-tripped through JSON, because the workflow compares the two JSON
# artifacts the legs uploaded, not in-memory objects.
# ---------------------------------------------------------------------------
Write-Host "[18] cross-platform comparison over two real-shaped reports"
function New-PlatformReport {
    param($Published, [string]$Platform)
    $res = Compare-CapabilityManifests -Dev (New-Manifest) -Published $Published
    $prov = New-ParityProvenance -GeneratedAt '2026-10-09T00:00:00Z' -Platform $Platform
    $obj = ConvertTo-ParityReportObject -Result $res -GeneratedAt '2026-10-09T00:00:00Z' -Observability $null -Provenance $prov
    return (($obj | ConvertTo-Json -Depth 10) | ConvertFrom-Json)
}
$pubObserved = { Set-Rung (Set-Rung (New-Manifest) 'bundled_resources' 'bundle_resource') 'spec_pages' 'embedded' }

$winR = New-PlatformReport (& $pubObserved) 'windows'
$linR = New-PlatformReport (& $pubObserved) 'linux'
$c1 = Compare-ParityPublishedAcrossPlatforms -WindowsReport $winR -LinuxReport $linR
Assert-True  "identical: available"            $c1.Available
Assert-Equal "identical: differs"              0 $c1.DifferCount
Assert-Equal "identical: same (3 observed rows)" 3 $c1.SameCount
Assert-Equal "identical: unobserved rows are NOT same" 6 $c1.UnobservedCount
Assert-Equal "identical: version carried"      '1.0.10' $c1.WindowsVersion

$linDiff = New-PlatformReport (Set-Rung (& $pubObserved) 'bundled_resources' 'unresolved') 'linux'
$c2 = Compare-ParityPublishedAcrossPlatforms -WindowsReport $winR -LinuxReport $linDiff
Assert-Equal "one differing row: differs"      1 $c2.DifferCount
$row = @($c2.Rows | Where-Object { $_.id -eq 'bundled_resources' })[0]
Assert-Equal "  the row is bundled_resources"  'differs' $row.disposition
Assert-Equal "  windows rung"                  'bundle_resource' $row.windows_published_rung
Assert-Equal "  linux rung"                    'unresolved' $row.linux_published_rung

# unknown on ONE platform is unobserved, never a difference and never agreement.
$linUnknown = New-PlatformReport (Set-Rung (& $pubObserved) 'spec_pages' 'unknown') 'linux'
$c3 = Compare-ParityPublishedAcrossPlatforms -WindowsReport $winR -LinuxReport $linUnknown
Assert-Equal "unknown on linux: not a difference" 0 $c3.DifferCount
Assert-Equal "unknown on linux: unobserved"    'unobserved' (@($c3.Rows | Where-Object { $_.id -eq 'spec_pages' })[0]).disposition

# A row only one roster carries.
$linExtra = New-PlatformReport (Add-Row (& $pubObserved) 'session_cli' 'bundle_resource') 'linux'
$c4 = Compare-ParityPublishedAcrossPlatforms -WindowsReport $winR -LinuxReport $linExtra
Assert-Equal "roster difference is labelled"   'only_on_linux' (@($c4.Rows | Where-Object { $_.id -eq 'session_cli' })[0]).disposition
Assert-Equal "  and is not counted as differs" 0 $c4.DifferCount

# ---------------------------------------------------------------------------
# [19] The refusals. Each one would otherwise produce a number that means
# something else: a 0 from a missing leg, or "platform difference" from two
# different releases.
# ---------------------------------------------------------------------------
Write-Host "[19] cross-platform refusals are UNKNOWN, never 0"
$r1 = Compare-ParityPublishedAcrossPlatforms -WindowsReport $winR -LinuxReport $null
Assert-True  "missing linux: unavailable"      (-not $r1.Available)
Assert-Equal "missing linux: reason"           'linux_report_missing' $r1.Reason
Assert-Equal "missing linux: count is null, not 0" $null $r1.DifferCount
Assert-Equal "missing windows: reason"         'windows_report_missing' (Compare-ParityPublishedAcrossPlatforms -WindowsReport $null -LinuxReport $linR).Reason

$newer = & $pubObserved
$newer.app_version = '1.0.12'
$r2 = Compare-ParityPublishedAcrossPlatforms -WindowsReport $winR -LinuxReport (New-PlatformReport $newer 'linux')
Assert-Equal "versions differ: reason"         'published_versions_differ' $r2.Reason
Assert-Equal "versions differ: count is null"  $null $r2.DifferCount
Assert-Equal "versions differ: linux version named" '1.0.12' $r2.LinuxVersion

$refused = New-PlatformReport (& $pubObserved) 'linux'
$refused.schema_refused = $true
Assert-Equal "schema-refused: reason"          'linux_report_schema_refused' (Compare-ParityPublishedAcrossPlatforms -WindowsReport $winR -LinuxReport $refused).Reason

# Nothing observed on both platforms: UNKNOWN, never "differs 0".
$allUnknown = { Set-Rung (New-Manifest) 'workspace_root' 'unknown' }
$r5 = Compare-ParityPublishedAcrossPlatforms -WindowsReport (New-PlatformReport (& $allUnknown) 'windows') -LinuxReport (New-PlatformReport (& $allUnknown) 'linux')
Assert-True  "no row on both: unavailable"     (-not $r5.Available)
Assert-Equal "no row on both: reason"          'no_row_observed_on_both' $r5.Reason
Assert-Equal "no row on both: count is null, not 0" $null $r5.DifferCount
Assert-Equal "no row on both: the rows are kept for diagnosis" 9 @($r5.Rows).Count
Assert-Equal "roster-only row is counted"      1 $c4.OnlyOnOneCount

$noVer = & $pubObserved
$noVer.app_version = $null
Assert-Equal "version unknown: reason"         'published_version_unknown' (Compare-ParityPublishedAcrossPlatforms -WindowsReport $winR -LinuxReport (New-PlatformReport $noVer 'linux')).Reason

# ---------------------------------------------------------------------------
# [20] The platform lives in the PROVENANCE block (Phase 6B on top of Phase 2).
# It is a fact about the run, recorded where the other run facts are, and the
# summary's first line names it so the two legs' summaries are told apart.
# ---------------------------------------------------------------------------
Write-Host "[20] provenance.platform, and the summary heading that names it"
$pLin = New-ParityProvenance -GeneratedAt '2026-10-09T00:00:00Z' -Platform 'linux'
Assert-Equal "provenance records the platform"  'linux' $pLin.platform
$pNone = New-ParityProvenance -GeneratedAt '2026-10-09T00:00:00Z'
Assert-True  "no platform is unknown, never empty" ([string]$pNone.platform -match '^unknown\(')
Assert-Equal "report carries it under provenance" 'linux' $linR.provenance.platform
Assert-True  "and not as a top-level field"     ($null -eq $linR.PSObject.Properties['platform'])
$mdLin = @(Format-ParitySummaryMarkdown -Result $r9 -Provenance $pLin)
Assert-True  "heading names the platform"       ($mdLin[0] -match '^### Published-build capability parity \(linux\) -- ')
Assert-True  "heading still carries the unobserved count" ($mdLin[0] -match '\b9 of 9\b')
$mdNone = @(Format-ParitySummaryMarkdown -Result $r9 -Provenance $pNone)
Assert-True  "an unknown platform adds no label" ($mdNone[0] -match '^### Published-build capability parity -- ')
$unl = Compare-CapabilityManifests -Dev (New-Manifest) -Published (& $pubObserved)
$unlR = ((ConvertTo-ParityReportObject -Result $unl -GeneratedAt '2026-10-09T00:00:00Z' -Observability $null -Provenance $pNone) | ConvertTo-Json -Depth 10) | ConvertFrom-Json
Assert-True  "an unknown provenance.platform is not a platform" ([string]$unlR.provenance.platform -match '^unknown\(')

Write-Host ""
if ($failures -gt 0) {
    Write-Host "PARITY-DIFF-TESTS FAILED: $failures of $checks checks" -ForegroundColor Red
    exit 1
}
Write-Host "PARITY-DIFF-TESTS OK: $checks checks passed" -ForegroundColor Green
exit 0
