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
# RUN IT UNDER WINDOWS POWERSHELL 5.1, NOT pwsh 7
# -----------------------------------------------
# Same reasoning as scripts/tests/test-smoke-summary.ps1: the exactly-one-row
# cases are where PS 5.1's scalar `Count` adapter returns $null for
# PSCustomObject, and the real gate runs under `powershell -File`.
#
# WHERE IT RUNS TODAY
# -------------------
# The first step of .github/workflows/published-parity.yml, before any compile.
# It is NOT wired into ci.yml: ci.yml is on ci-integrity.yml's guarded list, so
# adding a step there turns every PR touching it red for operator review. If
# per-PR coverage is wanted, that is a deliberate one-line addition next to the
# `Contract-smoke summary invariants (PS 5.1)` step, made with that cost in mind.
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
$FixtureText = Get-Content -LiteralPath $FixturePath -Raw

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
$text1 = Format-ParityReportText -Result $r1
Assert-True "report says the allowlist is empty" ($text1 -match 'allowlist: \(empty\)')

# ---------------------------------------------------------------------------
# 8. THE SHIPPED ALLOWLIST. Measured 2026-09-02: no CAPABILITY_SPECS row is
#    resolved by a cfg-gated module, so it is empty. And workspace_root must
#    NEVER appear on it -- that row differing is the plan's central finding.
# ---------------------------------------------------------------------------
Write-Host "[8] the shipped allowlist"
Assert-Equal "shipped allowlist is empty" 0 (@($ParityExpectedDifferences).Count)
Assert-Equal "workspace_root is never allowlisted" 0 (@($ParityExpectedDifferences | Where-Object { $_.Id -eq 'workspace_root' }).Count)
foreach ($e in @($ParityExpectedDifferences)) {
    Assert-True "allowlist entry '$($e.Id)' carries a reason" (-not [string]::IsNullOrWhiteSpace($e.Reason))
}

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

Write-Host ""
if ($failures -gt 0) {
    Write-Host "PARITY-DIFF-TESTS FAILED: $failures of $checks checks" -ForegroundColor Red
    exit 1
}
Write-Host "PARITY-DIFF-TESTS OK: $checks checks passed" -ForegroundColor Green
exit 0
