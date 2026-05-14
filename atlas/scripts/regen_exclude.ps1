# Regenerate atlas/exclude.txt from the live canonical PG.
#
# Runs every existing non-Atlas-managed table/sequence/view/MV/enum in the
# project and coord schemas through `--exclude` so Atlas only diffs the
# pilot set (project.regression_* + coord.coordinator_shadow_decisions).
#
# Re-run after alembic adds a new table; otherwise next `atlas schema apply`
# tries to drop it.

param(
    [string]$PgHost = "localhost",
    [int]$PgPort = 5433,
    [string]$PgUser = "qontinui_user",
    [string]$PgDb = "qontinui_db",
    [string]$PgPassword = "qontinui_dev_password",
    [string]$Container = "qontinui-canonical-postgres"
)

$env:PGPASSWORD = $PgPassword

$projectTables = docker exec $Container psql -U $PgUser -d $PgDb -At -c @"
SELECT relname FROM pg_class c
  JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE n.nspname = 'project'
   AND c.relkind IN ('r','S','v','m')
   AND relname NOT LIKE 'regression_%'
 ORDER BY 1;
"@

$coordTables = docker exec $Container psql -U $PgUser -d $PgDb -At -c @"
SELECT relname FROM pg_class c
  JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE n.nspname = 'coord'
   AND c.relkind IN ('r','S','v','m')
   AND relname <> 'coordinator_shadow_decisions'
 ORDER BY 1;
"@

$projectEnums = docker exec $Container psql -U $PgUser -d $PgDb -At -c @"
SELECT t.typname FROM pg_type t
  JOIN pg_namespace n ON n.oid = t.typnamespace
 WHERE n.nspname = 'project' AND t.typtype = 'e'
 ORDER BY 1;
"@

$coordEnums = docker exec $Container psql -U $PgUser -d $PgDb -At -c @"
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

$out = Join-Path $PSScriptRoot ".." "exclude.txt"
Set-Content -Path $out -Encoding ASCII -Value $patterns
"wrote $($patterns.Count) exclusion patterns to $out"
