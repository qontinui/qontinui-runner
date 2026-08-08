# Assert that the four places encoding "which tables does Atlas manage?" agree.
#
# NO DATABASE. This is pure text over four files in this repo, which is the
# whole point: it runs in seconds on every trigger and its verdict does NOT
# depend on qontinui-web's migration state. The DB-backed freshness check in
# the same workflow reds whenever web drifts -- a condition no runner PR can
# fix -- so it cannot be the guard for runner-side atlas mistakes. This can.
#
# The four encodings:
#
#   1. atlas/schema.hcl          `table "<t>" { schema = schema.<s> }` blocks
#                                -- the desired state Atlas applies.
#   2. atlas/atlas.hcl           `schemas = [...]` on the `runner_pilot` env
#                                -- what Atlas is scoped to inspect.
#   3. atlas/exclude.txt         the `--exclude` list -- what Atlas ignores.
#   4. atlas/scripts/regen_exclude.ps1
#                                `$PilotTables` / `$PilotSchemas` /
#                                `$ExcludedDeclarations`, read via its
#                                -PrintPilotSet / -PrintPilotSchemas /
#                                -PrintExcludedDeclarations switches (never
#                                re-parsed here -- one source, one reader).
#
# WHY THIS EXISTS. These drifted apart in exactly the way that is invisible
# until someone runs `atlas schema apply`:
#
#   - schema.hcl grew from the 5 pilot tables to 10 (spec_proposals,
#     proposal_events, apps, orchestration.runs, orchestration.subtasks),
#     while regen_exclude.ps1's carve-out stayed at the original 5. Any regen
#     against a DB where the runner had self-healed its own tables therefore
#     listed four Atlas-managed tables as EXCLUSIONS -- Atlas silently stops
#     managing what schema.hcl says it owns.
#   - schema.hcl declares `schema "orchestration"` and two tables in it, but
#     the `runner_pilot` env was scoped to project+coord only, so those
#     declarations were unreachable via the documented apply command.
#
# Neither is detectable by the DB-backed freshness check: that check compares
# exclude.txt against the live schema and never reads schema.hcl at all.
#
# Exit 0 when consistent, 1 with a per-violation report otherwise.

param(
    [string]$AtlasDir = (Join-Path $PSScriptRoot "..")
)

$ErrorActionPreference = "Stop"

$schemaHcl = Join-Path $AtlasDir "schema.hcl"
$atlasHcl = Join-Path $AtlasDir "atlas.hcl"
$excludeTxt = Join-Path $AtlasDir "exclude.txt"
$regenScript = Join-Path (Join-Path $AtlasDir "scripts") "regen_exclude.ps1"

foreach ($f in @($schemaHcl, $atlasHcl, $excludeTxt, $regenScript)) {
    if (-not (Test-Path $f)) { throw "Missing expected atlas file: $f" }
}

$violations = @()
function Add-Violation { param([string]$Message) ; $script:violations += $Message }

# --- 1. schema.hcl: the declared tables -------------------------------------
# A block looks like:
#   table "regression_suites" {
#     schema = schema.project
# so the table name comes first and its schema on a following line. Track the
# pending table name and resolve it at the first `schema = schema.<x>` seen.
$declared = @()
$pendingTable = $null
foreach ($line in (Get-Content -LiteralPath $schemaHcl)) {
    if ($line -match '^\s*table\s+"([^"]+)"\s*\{') {
        if ($pendingTable) {
            Add-Violation "schema.hcl: table `"$pendingTable`" has no `schema = schema.<name>` assignment."
        }
        $pendingTable = $Matches[1]
        continue
    }
    if ($pendingTable -and $line -match '^\s*schema\s*=\s*schema\.([A-Za-z0-9_]+)\s*$') {
        $declared += "$($Matches[1]).$pendingTable"
        $pendingTable = $null
    }
}
if ($pendingTable) {
    Add-Violation "schema.hcl: table `"$pendingTable`" has no `schema = schema.<name>` assignment."
}
if ($declared.Count -eq 0) {
    throw "Parsed ZERO table declarations out of $schemaHcl. The parser or the file shape changed; refusing to report a vacuous pass."
}

# --- 2. atlas.hcl: the env's schema scope -----------------------------------
# `schemas = ["project", "coord", "orchestration"]`, possibly wrapped.
$atlasText = (Get-Content -LiteralPath $atlasHcl -Raw)
if ($atlasText -notmatch '(?s)schemas\s*=\s*\[(.*?)\]') {
    throw "Could not find a `schemas = [...]` list in $atlasHcl."
}
$envSchemas = @([regex]::Matches($Matches[1], '"([^"]+)"') | ForEach-Object { $_.Groups[1].Value })
if ($envSchemas.Count -eq 0) {
    throw "Parsed an EMPTY `schemas = [...]` list out of $atlasHcl."
}

# --- 3. exclude.txt ---------------------------------------------------------
$excluded = @{}
foreach ($line in (Get-Content -LiteralPath $excludeTxt)) {
    $t = $line.Trim()
    if ($t) { $excluded[$t] = $true }
}
if ($excluded.Count -eq 0) {
    throw "Parsed ZERO patterns out of $excludeTxt."
}

# --- 4. regen_exclude.ps1's declared sets -----------------------------------
# Invoked as a child process rather than dot-sourced: dot-sourcing would run
# its DB path. Each switch returns before any psql call.
function Invoke-RegenExport {
    param([Parameter(Mandatory = $true)][string]$Switch)
    # Use the same host that is running this script (pwsh in CI, powershell.exe
    # on a Windows dev box) rather than hardcoding one of them.
    $exe = (Get-Process -Id $PID).Path
    $out = & $exe -NoProfile -File $regenScript $Switch
    if ($LASTEXITCODE -ne 0) {
        throw "regen_exclude.ps1 $Switch failed (exit $LASTEXITCODE)."
    }
    return @($out | ForEach-Object { "$_".Trim() } | Where-Object { $_ })
}

$pilotSet = Invoke-RegenExport -Switch "-PrintPilotSet"
$pilotSchemas = Invoke-RegenExport -Switch "-PrintPilotSchemas"
$excludedDecls = Invoke-RegenExport -Switch "-PrintExcludedDeclarations"
if ($pilotSet.Count -eq 0) { throw "regen_exclude.ps1 -PrintPilotSet returned nothing." }
if ($pilotSchemas.Count -eq 0) { throw "regen_exclude.ps1 -PrintPilotSchemas returned nothing." }

$excludedDeclSet = @{}
foreach ($d in $excludedDecls) { $excludedDeclSet[$d] = $true }

# --- Assertions -------------------------------------------------------------

# (a) Every schema a declared table lives in must be in the env's scope, or
#     Atlas never inspects it and the declaration is inert.
$declaredSchemas = @($declared | ForEach-Object { $_.Split('.')[0] } | Sort-Object -Unique)
foreach ($s in $declaredSchemas) {
    if ($envSchemas -notcontains $s) {
        Add-Violation "schema.hcl declares table(s) in schema '$s', but atlas.hcl's runner_pilot env scopes Atlas to [$($envSchemas -join ', ')]. Those declarations are unreachable via 'atlas schema apply --env runner_pilot'. Add '$s' to `schemas` in atlas.hcl."
    }
}

# (b) The regen script must SCAN every schema Atlas is scoped to. A scoped
#     schema it does not scan contributes no exclusions, so every non-pilot
#     object there is a DROP candidate.
foreach ($s in $envSchemas) {
    if ($pilotSchemas -notcontains $s) {
        Add-Violation "atlas.hcl scopes Atlas to schema '$s', but regen_exclude.ps1's `$PilotSchemas is [$($pilotSchemas -join ', ')] and never scans it. Non-pilot objects in '$s' would not be excluded, i.e. they are DROP candidates. Add '$s' to `$PilotSchemas."
    }
}

# (c) A declared table must not also be excluded -- that is a dead declaration:
#     schema.hcl claims Atlas owns it while exclude.txt tells Atlas to ignore
#     it. Deliberate exceptions are named in $ExcludedDeclarations.
foreach ($d in $declared) {
    if ($excluded.ContainsKey($d)) {
        if (-not $excludedDeclSet.ContainsKey($d)) {
            Add-Violation "'$d' is declared in schema.hcl but ALSO listed in exclude.txt, so Atlas ignores it and the declaration is dead. Either drop it from schema.hcl, or add it to `$PilotTables in regen_exclude.ps1 (and regenerate exclude.txt) if Atlas should own it."
        }
    }
    elseif ($excludedDeclSet.ContainsKey($d)) {
        Add-Violation "'$d' is listed in regen_exclude.ps1's `$ExcludedDeclarations (declared-but-deliberately-excluded) but is NOT in exclude.txt. The exception is stale -- Atlas now manages it. Remove it from `$ExcludedDeclarations, and make sure schema.hcl's declaration is complete before the next apply."
    }
}

# (d) The pilot carve-out and the declared set must be the same set, modulo
#     the named exceptions. Both directions matter:
#       - declared but not pilot => regen will list it as an exclusion, and
#         Atlas silently stops managing a table schema.hcl says it owns;
#       - pilot but not declared => regen carves out a table Atlas has no
#         desired state for, so it is neither managed nor excluded.
$pilotLookup = @{}
foreach ($p in $pilotSet) { $pilotLookup[$p] = $true }
foreach ($d in $declared) {
    if ($excludedDeclSet.ContainsKey($d)) { continue }
    if (-not $pilotLookup.ContainsKey($d)) {
        Add-Violation "'$d' is declared in schema.hcl but missing from regen_exclude.ps1's `$PilotTables. A regen against a DB containing it would add it to exclude.txt, silently un-managing it. Add it to `$PilotTables."
    }
}
$declaredLookup = @{}
foreach ($d in $declared) { $declaredLookup[$d] = $true }
foreach ($p in $pilotSet) {
    if (-not $declaredLookup.ContainsKey($p)) {
        Add-Violation "'$p' is carved out as pilot-owned in regen_exclude.ps1's `$PilotTables but is NOT declared in schema.hcl. It would be neither excluded nor managed. Declare it in schema.hcl, or drop it from `$PilotTables."
    }
}

# (e) An $ExcludedDeclarations entry that names something schema.hcl does not
#     declare is stale bookkeeping.
foreach ($e in $excludedDecls) {
    if (-not $declaredLookup.ContainsKey($e)) {
        Add-Violation "'$e' is in regen_exclude.ps1's `$ExcludedDeclarations but schema.hcl does not declare it. The exception is stale; remove it."
    }
}

# --- Report -----------------------------------------------------------------
if ($violations.Count -gt 0) {
    foreach ($v in $violations) { Write-Host "::error::$v" }
    Write-Host ""
    Write-Host "atlas pilot consistency: $($violations.Count) violation(s)."
    Write-Host "See atlas/README.md - 'Who owns which table' for the ownership rules."
    exit 1
}

Write-Host "atlas pilot consistency OK:"
Write-Host "  $($declared.Count) table(s) declared in schema.hcl across [$($declaredSchemas -join ', ')]"
Write-Host "  env scope [$($envSchemas -join ', ')]; regen scans [$($pilotSchemas -join ', ')]"
Write-Host "  $($pilotSet.Count) pilot carve-out(s); $($excluded.Count) exclusion pattern(s)"
if ($excludedDecls.Count -gt 0) {
    Write-Host "  declared-but-deliberately-excluded: $($excludedDecls -join ', ')"
}
exit 0
