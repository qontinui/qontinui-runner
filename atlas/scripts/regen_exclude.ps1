# Regenerate atlas/exclude.txt from the live canonical PG.
#
# Runs every existing non-Atlas-managed table/sequence/view/MV/enum in the
# project and coord schemas through `--exclude` so Atlas only diffs the
# pilot set (project.regression_* + coord.coordinator_shadow_decisions).
#
# Re-run after alembic adds a new table; otherwise next `atlas schema apply`
# tries to drop it.
#
# Modes:
#   (default)  regenerate and rewrite atlas/exclude.txt in place.
#   -Check     regenerate to memory and DIFF against the committed
#              atlas/exclude.txt; exit 1 on drift (nothing is rewritten),
#              exit 0 when fresh. This is what CI runs -- see
#              .github/workflows/atlas-exclude-fresh.yml -- and what a human
#              should run before `atlas schema apply`.
#   -Out <p>   (with -Check) also write the freshly-regenerated list to <p>,
#              so CI can upload it as an artifact without a second full regen.
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
    [string]$Out = ""
)

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

$projectTables = Invoke-PgQuery @"
SELECT relname FROM pg_class c
  JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE n.nspname = 'project'
   AND c.relkind IN ('r','S','v','m')
   AND relname NOT LIKE 'regression_%'
 ORDER BY 1;
"@

$coordTables = Invoke-PgQuery @"
SELECT relname FROM pg_class c
  JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE n.nspname = 'coord'
   AND c.relkind IN ('r','S','v','m')
   AND relname <> 'coordinator_shadow_decisions'
 ORDER BY 1;
"@

$projectEnums = Invoke-PgQuery @"
SELECT t.typname FROM pg_type t
  JOIN pg_namespace n ON n.oid = t.typnamespace
 WHERE n.nspname = 'project' AND t.typtype = 'e'
 ORDER BY 1;
"@

$coordEnums = Invoke-PgQuery @"
SELECT t.typname FROM pg_type t
  JOIN pg_namespace n ON n.oid = t.typnamespace
 WHERE n.nspname = 'coord' AND t.typtype = 'e'
 ORDER BY 1;
"@

$patterns = @()
$projectTables | ForEach-Object { if ($_) { $patterns += "project.$_" } }
$coordTables   | ForEach-Object { if ($_) { $patterns += "coord.$_" } }
$projectEnums  | ForEach-Object { if ($_) { $patterns += "project.$_" } }
$coordEnums    | ForEach-Object { if ($_) { $patterns += "coord.$_" } }

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
    # explicit `exit 1`.
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
    exit 1
}

[System.IO.File]::WriteAllText($outFile, $content)
"wrote $($patterns.Count) exclusion patterns to $outFile"
