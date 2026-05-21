# backfill_app_id.ps1
#
# spec-multi-app Stream F.3 — bulk backfill of `provenance.app_id` on every
# on-disk IR file at `<app_repo_root>/specs/pages/<page_id>/state-machine.derived.json`.
#
# Stream A's IrProvenance type added `app_id: String` (with serde default).
# Pre-multi-app IRs either have `provenance` missing entirely, or have
# `provenance` but no `app_id`. This script:
#
#   1. Walks every known app's specs/pages directory.
#   2. For each `state-machine.derived.json`:
#      - If `provenance` is missing, synthesize `{ source: "migrated", app_id: <app> }`.
#      - If `provenance` exists but `app_id` is empty/missing, set `app_id`.
#      - Otherwise, leave untouched.
#   3. Writes back UTF-8.
#
# Idempotent. Safe to re-run. Reports per-app counts.
#
# Apps are hard-coded to the three known dev repos. Pre-flight does not
# require the runner or Postgres to be up — this is a pure-disk migration.
#
# Usage:
#   pwsh -File D:/qontinui-root/qontinui-runner/scripts/backfill_app_id.ps1

$ErrorActionPreference = 'Stop'

$apps = @(
    @{ id = 'qontinui-runner';     specsRoot = 'D:/qontinui-root/qontinui-runner/specs/pages' }
    @{ id = 'qontinui-web';        specsRoot = 'D:/qontinui-root/qontinui-web/frontend/specs/pages' }
    @{ id = 'qontinui-supervisor'; specsRoot = 'D:/qontinui-root/qontinui-supervisor/frontend/specs/pages' }
)

$totalUpdated = 0
$totalSkipped = 0
$totalMissing = 0

foreach ($app in $apps) {
    $appId    = $app.id
    $specsDir = $app.specsRoot

    if (-not (Test-Path $specsDir)) {
        Write-Host "MISS  $appId : $specsDir does not exist (skipping)"
        $totalMissing++
        continue
    }

    $updated = 0
    $skipped = 0

    Get-ChildItem -Path $specsDir -Directory -ErrorAction SilentlyContinue | ForEach-Object {
        $irPath = Join-Path $_.FullName 'state-machine.derived.json'
        if (-not (Test-Path $irPath)) { return }

        $needWrite = $false
        try {
            $raw = Get-Content $irPath -Raw -Encoding UTF8
            $json = $raw | ConvertFrom-Json

            # Top-level provenance handling. PowerShell's ConvertFrom-Json
            # parses missing fields as $null. PSCustomObject lets us add
            # properties dynamically.
            if (-not $json.PSObject.Properties.Name -contains 'provenance' -or $null -eq $json.provenance) {
                # Synthesize provenance.
                $json | Add-Member -NotePropertyName 'provenance' -NotePropertyValue ([pscustomobject]@{
                    source = 'migrated'
                    appId  = $appId
                }) -Force
                $needWrite = $true
            } else {
                $prov = $json.provenance
                $hasAppId = $prov.PSObject.Properties.Name -contains 'appId'
                $appIdVal = if ($hasAppId) { $prov.appId } else { $null }
                if (-not $hasAppId -or [string]::IsNullOrEmpty($appIdVal)) {
                    if ($hasAppId) {
                        $prov.appId = $appId
                    } else {
                        $prov | Add-Member -NotePropertyName 'appId' -NotePropertyValue $appId -Force
                    }
                    $needWrite = $true
                }
            }

            if ($needWrite) {
                # Use UTF-8 WITHOUT BOM. PowerShell's `Set-Content -Encoding UTF8`
                # prepends a BOM on Windows PowerShell, which serde_json refuses
                # to parse (fails with "expected value at line 1 column 1" on
                # the BOM byte). The runner reads these files via
                # spec_api/storage.rs::read_ir + serde_json::from_str, so any
                # BOM here cascades into HTTP 500 on /spec-check and into a
                # missing entry on /apps/<id>/spec/list. .NET's UTF8Encoding
                # constructor with byte-order-mark=false suppresses the BOM.
                $jsonText = $json | ConvertTo-Json -Depth 100
                [System.IO.File]::WriteAllText($irPath, $jsonText, [System.Text.UTF8Encoding]::new($false))
                $updated++
            } else {
                $skipped++
            }
        } catch {
            Write-Host "ERR   $appId : failed on $irPath : $_"
        }
    }

    Write-Host "$appId : updated=$updated  skipped=$skipped"
    $totalUpdated += $updated
    $totalSkipped += $skipped
}

Write-Host ''
Write-Host "TOTAL : updated=$totalUpdated  skipped=$totalSkipped  missing-app-dirs=$totalMissing"
