# Regenerate atlas/exclude.txt from the live canonical PG.
#
# Runs every existing non-Atlas-managed table/sequence/view/MV/enum in the
# project, coord and orchestration schemas through `--exclude` so Atlas only
# diffs the pilot set (the tables `atlas/schema.hcl` declares).
#
# Re-run after alembic adds a new table; otherwise next `atlas schema apply`
# tries to drop it.
#
# Modes:
#   (default)  regenerate and rewrite atlas/exclude.txt in place.
#   -Check     regenerate to memory and DIFF against the committed
#              atlas/exclude.txt; exit 2 on drift (nothing is rewritten),
#              exit 0 when fresh. This is what CI runs -- see
#              .github/workflows/atlas-exclude-fresh.yml -- and what a human
#              should run before `atlas schema apply`.
#   -Out <p>   (with -Check) also write the freshly-regenerated list to <p>,
#              so CI can upload it as an artifact without a second full regen.
#   -PrintPilotSet
#              print the pilot set as `<schema>.<table>` lines and exit 0
#              WITHOUT touching the DB. This is the machine-readable export
#              `check_pilot_consistency.ps1` diffs against schema.hcl, so the
#              pilot set is declared in exactly one place.
#
# EXIT CODES (-Check) -- the contract CI keys on:
#   0  the committed list is fresh.
#   2  DRIFT: the committed list disagrees with a fresh regen. The regenerated
#      list is trustworthy; -Out (if given) holds it.
#   1  FAILURE: DB unreachable, a query failed, or the zero-pattern guard
#      tripped. NOTHING about the schema was established and -Out may be
#      absent or garbage.
#
# The 1-vs-2 split exists so callers never have to classify by grepping this
# script's human-readable prose. The freshness workflow self-heals (opens a
# refresh PR) on 2 and must NEVER self-heal on 1 -- a DB failure that read as
# drift would auto-author a PR dropping real exclusions, the exact data-loss
# footgun this script guards. Keep the split intact: a new failure mode must
# exit 1 (or throw), never 2.
#
# DB transport:
#   -Container <name>  (default "qontinui-canonical-postgres") runs psql via
#                      `docker exec <name>`.  Set -Container "" (empty) to use
#                      a NATIVE psql on the host, reached at -PgHost:-PgPort.
#                      The canonical dev PG is exposed on host port 5433
#                      (docker maps 5433->5432); CI reaches its `services:
#                      postgres` the same way, so both use -PgPort 5433.
#
# SAFETY: every psql/docker query is exit-status-checked and the run aborts
# (throws, writes nothing) if a query fails OR the regenerated set is empty --
# an empty exclude list would make `atlas schema apply` DROP every excluded
# table, the exact footgun this script guards against.

param(
    [string]$PgHost = "localhost",
    [int]$PgPort = 5433,
    [string]$PgUser = "qontinui_user",
    [string]$PgDb = "qontinui_db",
    [string]$PgPassword = "qontinui_dev_password",
    [string]$Container = "qontinui-canonical-postgres",
    [switch]$Check,
    [string]$Out = "",
    [switch]$PrintPilotSet,
    [switch]$PrintPilotSchemas,
    [switch]$PrintExcludedDeclarations
)

# ---------------------------------------------------------------------------
# THE PILOT SET -- the tables `atlas/schema.hcl` declares, and which Atlas
# therefore MANAGES. Everything else in these schemas is alembic/coord-owned
# and gets excluded.
#
# This list used to be encoded as SQL predicates (`relname NOT LIKE
# 'regression_%'`, `relname <> 'coordinator_shadow_decisions'`) that were never
# updated as schema.hcl grew, so schema.hcl's spec_proposals / proposal_events
# / orchestration.* declarations were silently listed as EXCLUSIONS by any
# regen run against a DB where the runner had self-healed them -- i.e. against
# this script's own DEFAULT transport, the local canonical PG. Atlas would then
# ignore four tables it is supposed to own. CI never caught it because its
# schema comes from `alembic upgrade head` alone, where those runner-authored
# tables do not exist.
#
# `check_pilot_consistency.ps1` now diffs this list against schema.hcl on every
# CI run, so the two cannot drift apart again. Keep them in lockstep.
$PilotTables = [ordered]@{
    project       = @(
        'regression_suites',
        'regression_runs',
        'regression_diagnoses',
        'regression_assertion_executions',
        'spec_proposals',
        'proposal_events'
    )
    coord         = @('coordinator_shadow_decisions')
    orchestration = @('runs', 'subtasks')
}

# Declared in schema.hcl but deliberately NOT pilot-owned, so it stays in the
# exclude list. qontinui-web's alembic chain co-authors `project.apps`
# (`project_apps_p1a_auto_fresh_fields` CREATEs it and adds fleet-fresh
# columns), and the runner's own `pg/mod.rs` self-heal adds six columns beyond
# what schema.hcl declares. Letting Atlas manage it against today's HCL would
# DROP those six columns. Resolving the ownership properly is a cross-repo
# change (it needs a matching `ATLAS_OWNED_TABLES` entry in qontinui-web's
# alembic `env.py`), so it is tracked rather than silently half-done.
# `check_pilot_consistency.ps1` knows about this exception by name.
$ExcludedDeclarations = @('project.apps')

# The schemas Atlas is scoped to. Must match `schemas` on the `runner_pilot`
# env in atlas.hcl -- check_pilot_consistency.ps1 asserts that too.
$PilotSchemas = @('project', 'coord', 'orchestration')

if ($PrintPilotSet) {
    foreach ($schema in $PilotTables.Keys) {
        foreach ($table in $PilotTables[$schema]) { "$schema.$table" }
    }
    exit 0
}

if ($PrintPilotSchemas) {
    $PilotSchemas | ForEach-Object { $_ }
    exit 0
}

if ($PrintExcludedDeclarations) {
    $ExcludedDeclarations | ForEach-Object { $_ }
    exit 0
}

$env:PGPASSWORD = $PgPassword

# Run one -At (unaligned, tuples-only) query, native or via docker exec, and
# FAIL LOUDLY on a non-zero exit. Native path requires -h/-p (docker exec
# reaches the container's local socket, so it does not).
function Invoke-PgQuery {
    param([Parameter(Mandatory = $true)][string]$Sql)
    if ([string]::IsNullOrWhiteSpace($Container)) {
        $result = psql -h $PgHost -p $PgPort -U $PgUser -d $PgDb -At -c $Sql
    }
    else {
        $result = docker exec $Container psql -U $PgUser -d $PgDb -At -c $Sql
    }
    if ($LASTEXITCODE -ne 0) {
        throw "psql query failed (exit $LASTEXITCODE). DB unreachable, wrong container/port, or bad auth? Refusing to build an exclude list from a failed query."
    }
    return $result
}

# Relations (tables, sequences, views, materialized views) in one schema.
# A schema that does not exist yields zero rows and exit 0 -- correct, and the
# normal case for `orchestration` in CI, whose DB comes from alembic alone.
function Get-SchemaRelations {
    param([Parameter(Mandatory = $true)][string]$Schema)
    # $Schema is always one of $PilotSchemas (literals in this file), never
    # user input, but keep the shape assertion so that stays true.
    if ($Schema -notmatch '^[a-z_]+$') { throw "Refusing to query unexpected schema name '$Schema'." }
    return Invoke-PgQuery @"
SELECT relname FROM pg_class c
  JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE n.nspname = '$Schema'
   AND c.relkind IN ('r','S','v','m')
 ORDER BY 1;
"@
}

function Get-SchemaEnums {
    param([Parameter(Mandatory = $true)][string]$Schema)
    if ($Schema -notmatch '^[a-z_]+$') { throw "Refusing to query unexpected schema name '$Schema'." }
    return Invoke-PgQuery @"
SELECT t.typname FROM pg_type t
  JOIN pg_namespace n ON n.oid = t.typnamespace
 WHERE n.nspname = '$Schema' AND t.typtype = 'e'
 ORDER BY 1;
"@
}

# Fast membership set for the pilot carve-out.
$pilotSet = @{}
foreach ($schema in $PilotTables.Keys) {
    foreach ($table in $PilotTables[$schema]) { $pilotSet["$schema.$table"] = $true }
}

# Serialization order is part of the file's identity (a reordering would read
# as drift), so it is fixed here: all relations schema-by-schema, then all
# enums schema-by-schema -- which reproduces the original
# project/coord/project-enums/coord-enums order, with the two orchestration
# groups appended inside their halves.
$patterns = @()
foreach ($schema in $PilotSchemas) {
    foreach ($relname in (Get-SchemaRelations -Schema $schema)) {
        if (-not $relname) { continue }
        # Filtered here rather than in SQL so the pilot set is declared exactly
        # once, above, instead of being smeared across per-schema predicates
        # that nobody remembers to update.
        if ($pilotSet.ContainsKey("$schema.$relname")) { continue }
        $patterns += "$schema.$relname"
    }
}
foreach ($schema in $PilotSchemas) {
    foreach ($typname in (Get-SchemaEnums -Schema $schema)) {
        if (-not $typname) { continue }
        # schema.hcl declares no enums, so every enum is alembic-owned. If that
        # ever changes, the enum belongs in $PilotTables and the consistency
        # checker will say so.
        if ($pilotSet.ContainsKey("$schema.$typname")) { continue }
        $patterns += "$schema.$typname"
    }
}

# Zero patterns is never legitimate -- the project/coord schemas always contain
# non-pilot objects. An empty set means the queries returned nothing (silent
# connection/schema failure that still exited 0). Abort before writing.
if ($patterns.Count -eq 0) {
    throw "Regeneration produced ZERO exclusion patterns. The pilot always has non-pilot project/coord objects to exclude, so an empty set means the DB queries returned nothing (wrong DB/schema?). Refusing to write or diff."
}

# Nested Join-Path (not the 3-arg form) so this works on Windows PowerShell
# 5.1 too -- 5.1's Join-Path takes only -Path + -ChildPath; the 3-positional
# form is pwsh-Core-only.
$outFile = Join-Path (Join-Path $PSScriptRoot "..") "exclude.txt"

# Canonical serialization: LF line endings + trailing newline, regardless of
# host OS. Windows PowerShell's Set-Content defaults to CRLF, which would make
# a Windows-dev regen differ from CI's pwsh-on-Linux output as pure EOL noise;
# writing bytes directly with `n forces LF so the diff only ever reflects real
# schema drift. The committed exclude.txt is LF (.gitattributes: *.txt eol=lf).
$content = ($patterns -join "`n") + "`n"

if ($Check) {
    # Emit the fresh list for a caller that wants it (CI artifact upload)
    # without a second full regen.
    if (-not [string]::IsNullOrWhiteSpace($Out)) {
        [System.IO.File]::WriteAllText($Out, $content)
    }

    $committed = ""
    if (Test-Path $outFile) {
        # Read raw and normalize CRLF -> LF before comparing, so an
        # accidentally-CRLF committed file still diffs on content only.
        $committed = ([System.IO.File]::ReadAllText($outFile)) -replace "`r`n", "`n"
    }

    # Case-SENSITIVE compare (-ceq) -- a mis-cased identifier is real drift.
    if ($committed -ceq $content) {
        "atlas/exclude.txt is up to date ($($patterns.Count) exclusion patterns)."
        exit 0
    }

    # `::error::` is the GitHub Actions error-annotation prefix (harmless plain
    # text locally). Use Write-Host, not Write-Error: writing to the error
    # stream makes powershell.exe -File override the exit code, masking our
    # explicit `exit 2`.
    Write-Host "::error::atlas/exclude.txt is STALE relative to a fresh regen against the live schema."
    Write-Host "A qontinui-web alembic migration likely added or removed a project/coord table."
    Write-Host "Regenerate + commit locally:"
    Write-Host "  pwsh atlas/scripts/regen_exclude.ps1   # rewrites atlas/exclude.txt"
    Write-Host "Or download the 'atlas-exclude-fresh' artifact from this workflow run and commit it."
    Write-Host ""
    Write-Host "Drift (committed vs fresh):"

    # Use the built-in set-difference cmdlet rather than a hand-rolled O(n*m)
    # loop. -split leaves a trailing empty element from the trailing newline on
    # both sides, so Compare-Object treats them as equal.
    $committedLines = $committed -split "`n"
    $freshLines = $content -split "`n"
    $diff = Compare-Object -ReferenceObject $committedLines -DifferenceObject $freshLines
    if ($diff) {
        foreach ($d in $diff) {
            if ($d.SideIndicator -eq '=>') { Write-Host "  + (fresh only)     $($d.InputObject)" }
            else { Write-Host "  - (committed only) $($d.InputObject)" }
        }
    }
    else {
        # -ceq failed but no line-level set difference: the delta is
        # case / whitespace / trailing-newline only.
        Write-Host "  (no line-set difference -- delta is case/whitespace/newline only;"
        Write-Host "   compare the committed file against the 'atlas-exclude-fresh' artifact byte-for-byte)"
    }
    # 2, not 1: DRIFT, distinct from the failure paths above which throw (or
    # exit 1). See the EXIT CODES block at the top of this file.
    exit 2
}

[System.IO.File]::WriteAllText($outFile, $content)
"wrote $($patterns.Count) exclusion patterns to $outFile"
