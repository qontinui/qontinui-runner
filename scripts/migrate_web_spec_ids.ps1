# migrate_web_spec_ids.ps1
#
# spec-multi-app Stream F.2 — one-shot rename of 15 qontinui-web spec
# directories whose `id` fields drifted from the short-form slugs that
# `useDiscoveredSpec(...)` consumers expect.
#
# For each entry in $renames:
#   1. Rewrite `state-machine.derived.json`'s top-level `id` field to the
#      new value.
#   2. Rewrite `spec.uibridge.json`'s top-level `id` field if present (the
#      projection file is regenerated from IR by the Spec API, but we keep
#      it in sync here so the on-disk file doesn't lie until the next
#      author cycle).
#   3. `Rename-Item` the directory.
#
# Idempotent: skips entries whose old directory doesn't exist (already
# renamed). Safe to re-run.
#
# Usage:
#   pwsh -File D:/qontinui-root/qontinui-runner/scripts/migrate_web_spec_ids.ps1

$ErrorActionPreference = 'Stop'

$webSpecs = 'D:/qontinui-root/qontinui-web/frontend/specs/pages'

$renames = [ordered]@{
    'runs-runs'                                              = 'runs'
    'runs-active-active-runs'                                = 'active-runs'
    'runs-findings-findings'                                 = 'findings'
    'runs-statistics-statistics'                             = 'statistics'
    'build-contexts-contexts'                                = 'contexts'
    'build-templates-templates'                              = 'templates'
    'build-workflows-workflows'                              = 'workflows'
    'chat-chat'                                              = 'chat'
    'execute-execute'                                        = 'execute'
    'runners-runners'                                        = 'runners'
    'settings-ai-ai-settings'                                = 'ai-settings'
    'tools-error-monitor-error-monitor'                      = 'error-monitor'
    'tools-inspector-inspector'                              = 'inspector'
    'components-test-generators-snapshot-test-generator'     = 'snapshot-test-generator'
    'components-test-generators-snapshot-post-capture'       = 'snapshot-post-capture'
}

$renamed = 0
$skipped = 0
$errors  = 0

foreach ($entry in $renames.GetEnumerator()) {
    $oldName = $entry.Key
    $newName = $entry.Value
    $oldDir = Join-Path $webSpecs $oldName
    $newDir = Join-Path $webSpecs $newName

    if (-not (Test-Path $oldDir)) {
        Write-Host "SKIP   $oldName (no old dir)"
        $skipped++
        continue
    }

    if ((Test-Path $newDir) -and ($oldDir -ne $newDir)) {
        Write-Host "ERR    $oldName -> $newName : target dir already exists"
        $errors++
        continue
    }

    # UTF-8 WITHOUT BOM. PowerShell's `Set-Content -Encoding UTF8` prepends a
    # BOM on Windows PowerShell, which serde_json refuses to parse (fails with
    # "expected value at line 1 column 1" on the BOM byte). The runner reads
    # these files via spec_api/storage.rs::read_ir + serde_json::from_str, so
    # any BOM here cascades into HTTP 500 on /spec-check and into a missing
    # entry on /apps/<id>/spec/list.
    $noBomUtf8 = [System.Text.UTF8Encoding]::new($false)

    # 1. Rewrite IR id.
    $irPath = Join-Path $oldDir 'state-machine.derived.json'
    if (Test-Path $irPath) {
        try {
            $json = Get-Content $irPath -Raw -Encoding UTF8 | ConvertFrom-Json
            $json.id = $newName
            $irText = $json | ConvertTo-Json -Depth 100
            [System.IO.File]::WriteAllText($irPath, $irText, $noBomUtf8)
        } catch {
            Write-Host "ERR    $oldName : failed to rewrite IR: $_"
            $errors++
            continue
        }
    } else {
        Write-Host "WARN   $oldName : no state-machine.derived.json found"
    }

    # 2. Rewrite projection id if present.
    $projPath = Join-Path $oldDir 'spec.uibridge.json'
    if (Test-Path $projPath) {
        try {
            $proj = Get-Content $projPath -Raw -Encoding UTF8 | ConvertFrom-Json
            if ($proj.PSObject.Properties.Name -contains 'id') {
                $proj.id = $newName
                $projText = $proj | ConvertTo-Json -Depth 100
                [System.IO.File]::WriteAllText($projPath, $projText, $noBomUtf8)
            }
        } catch {
            Write-Host "WARN   $oldName : failed to rewrite projection id (non-fatal): $_"
        }
    }

    # 3. Rename directory.
    Rename-Item -Path $oldDir -NewName $newName
    Write-Host "OK     $oldName -> $newName"
    $renamed++
}

Write-Host ''
Write-Host "Summary: renamed=$renamed  skipped=$skipped  errors=$errors"
if ($errors -gt 0) { exit 1 }
