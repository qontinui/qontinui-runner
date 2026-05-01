#!/usr/bin/env pwsh
# add-testing-sections.ps1
# Adds testing sections to all runner page specs via the spec chat UI.
# Processes each spec that does not yet have a "testing" top-level field.
# Usage: powershell -File scripts/add-testing-sections.ps1 [-MaxMinutes 35]

param(
    [int]$MaxMinutes = 35,
    [string]$OnlySpec = ""   # Optional: process only this specId (e.g. runner:terminal)
)

$BASE        = "http://localhost:9876/ui-bridge"
$RUNNER_API  = "http://localhost:9876"
$SPEC_DIR    = "$env:APPDATA\com.qontinui.runner\user-specs"
$logFile     = "$PSScriptRoot\add-testing.log"
$UTF8_NO_BOM = New-Object System.Text.UTF8Encoding $false  # WriteAllText default adds BOM; Rust serde_json rejects BOM

New-Item -ItemType Directory -Force -Path $SPEC_DIR | Out-Null
"" | Out-File $logFile -Force

function Log {
    param([string]$msg)
    $ts = Get-Date -Format "HH:mm:ss"
    $line = "[$ts] $msg"
    Write-Host $line
    Add-Content -Path $logFile -Value $line
}

function Evaluate-JS {
    param([string]$Expr)
    try {
        $body = @{ expression = $Expr } | ConvertTo-Json -Compress
        $r = Invoke-WebRequest -Uri "$BASE/control/page/evaluate" -Method POST `
             -ContentType 'application/json' -Body $body -UseBasicParsing -ErrorAction Stop
        return ($r.Content | ConvertFrom-Json).data.result.value
    } catch {
        return $null
    }
}

function Get-UIElements {
    try {
        $r = Invoke-WebRequest -Uri "$BASE/control/elements" -Method GET -UseBasicParsing -ErrorAction Stop
        return ($r.Content | ConvertFrom-Json).data.elements
    } catch { return @() }
}

function Click-ByDataUiId {
    param([string]$UiId)
    # Use MouseEvent dispatch instead of .click() — WebView2 needs the full event for React handlers
    $expr = "(function(){var el=document.querySelector('[data-ui-id=""$UiId""]');if(!el)return 'notfound';el.scrollIntoView({block:'center'});el.dispatchEvent(new MouseEvent('click',{bubbles:true,cancelable:true,view:window}));return 'ok';})()"
    $result = Evaluate-JS $expr
    return $result -eq "ok"
}

function Set-TextareaValue {
    param([string]$UiId, [string]$Value)
    # Use the native setter to bypass React's controlled-input guard
    $escaped = $Value.Replace('\', '\\').Replace('"', '\"').Replace("`n", '\n')
    $expr = @"
(function(){
  var el = document.querySelector('[data-ui-id="$UiId"]');
  if(!el) return 'notfound';
  var setter = Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype,'value').set;
  setter.call(el, "$escaped");
  el.dispatchEvent(new Event('input', {bubbles:true}));
  el.dispatchEvent(new Event('change', {bubbles:true}));
  return 'ok';
})()
"@
    $result = Evaluate-JS $expr
    return $result -eq "ok"
}

function Get-AIResponseCount {
    # Returns the number of [/AI_RESPONSE] markers in the task output (one per AI turn).
    param([string]$TaskId)
    try {
        $resp = (Invoke-WebRequest -Uri "$RUNNER_API/task-runs/$TaskId/output?tail_chars=400000" `
                 -UseBasicParsing -ErrorAction Stop).Content
        $output = ($resp | ConvertFrom-Json).output
        return ([regex]::Matches($output, '\[/AI_RESPONSE\]')).Count
    } catch { return 0 }
}

function Wait-ForNewAIResponse {
    # Polls until [/AI_RESPONSE] count exceeds $PriorCount AND a JSON block exists.
    # Returns $true on success, $null on timeout.
    param([string]$TaskId, [int]$PriorCount, [int]$MaxMins = 10)
    if (-not $TaskId) { return $null }
    $maxSecs = $MaxMins * 60
    $elapsed = 0
    $interval = 20
    $fence = [char]96 + [char]96 + [char]96
    $pattern = "(?s)$fence(?:json)?\s*\n(\{.*?\})\s*\n$fence"
    while ($elapsed -lt $maxSecs) {
        Start-Sleep -Seconds $interval
        $elapsed += $interval
        try {
            $resp = (Invoke-WebRequest -Uri "$RUNNER_API/task-runs/$TaskId/output?tail_chars=400000" `
                     -UseBasicParsing -ErrorAction Stop).Content
            $output = ($resp | ConvertFrom-Json).output
            $currentCount = ([regex]::Matches($output, '\[/AI_RESPONSE\]')).Count
            if ($currentCount -gt $PriorCount) {
                $blocks = [regex]::Matches($output, $pattern)
                if ($blocks.Count -gt 0) { return $true }
                Log "  New AI response but no JSON block yet - waiting..."
            }
        } catch {}
        $elMin = [int]($elapsed / 60)
        Log "  Waiting for new AI response... (${elMin}m/${MaxMins}m)"
    }
    return $null
}

function Click-UIElement {
    param([string]$Id)
    try {
        $r = Invoke-WebRequest -Uri "$BASE/control/element/$Id/action" -Method POST `
             -ContentType 'application/json' -Body '{"action": "click"}' `
             -UseBasicParsing -ErrorAction Stop
        return ($r.Content | ConvertFrom-Json).success -eq $true
    } catch { return $false }
}

function Get-RecentSpecTaskId {
    try {
        $resp = (Invoke-WebRequest -Uri "$RUNNER_API/task-runs/running" -UseBasicParsing -ErrorAction Stop).Content | ConvertFrom-Json
        $task = $resp | Where-Object { $_.task_name -like "Spec Chat:*" } | Select-Object -First 1
        if ($task) { return $task.id }
    } catch {}
    return $null
}

function Get-NewSpecTaskId {
    # Returns the first Spec Chat task whose ID is NOT in $ExcludeIds.
    param([string[]]$ExcludeIds)
    try {
        $resp = (Invoke-WebRequest -Uri "$RUNNER_API/task-runs/running" -UseBasicParsing -ErrorAction Stop).Content | ConvertFrom-Json
        $task = $resp | Where-Object { $_.task_name -like "Spec Chat:*" -and $ExcludeIds -notcontains $_.id } | Select-Object -First 1
        if ($task) { return $task.id }
    } catch {}
    return $null
}

function Get-TaskForSpec {
    # Find a running Spec Chat task that matches this spec's description prefix.
    # Falls back to any Spec Chat task not in ExcludeIds.
    param([string]$DescriptionPrefix, [string[]]$ExcludeIds = @())
    try {
        $resp = (Invoke-WebRequest -Uri "$RUNNER_API/task-runs/running" -UseBasicParsing -ErrorAction Stop).Content | ConvertFrom-Json
        $candidates = $resp | Where-Object { $_.task_name -like "Spec Chat:*" -and $ExcludeIds -notcontains $_.id }
        if ($DescriptionPrefix) {
            $match = $candidates | Where-Object { $_.task_name -like "Spec Chat: $DescriptionPrefix*" } | Select-Object -First 1
            if ($match) { return $match.id }
        }
        # Fallback: any spec chat task (for specs with no prior task)
        $any = $candidates | Select-Object -First 1
        if ($any) { return $any.id }
    } catch {}
    return $null
}

function Extract-AndSave-Spec {
    param([string]$taskId, [string]$specId, [string]$SpecFile)
    if (-not $taskId) { return }
    try {
        $resp = (Invoke-WebRequest -Uri "$RUNNER_API/task-runs/$taskId/output?tail_chars=300000" -UseBasicParsing).Content
        $output = ($resp | ConvertFrom-Json).output
        $fence = [char]96 + [char]96 + [char]96
        $pattern = "(?s)$fence(?:json)?\s*\n(\{.*?\})\s*\n$fence"
        $blocks = [regex]::Matches($output, $pattern)
        # Find the last JSON block that is actually valid (skip template/placeholder blocks)
        $json = $null
        $testingObj = $null
        for ($bi = $blocks.Count - 1; $bi -ge 0; $bi--) {
            $candidate = $blocks[$bi].Groups[1].Value.Trim()
            try {
                $testingObj = $candidate | ConvertFrom-Json
                $json = $candidate
                break
            } catch {}
        }
        if ($json) {
            # AI now outputs ONLY the testing section object (not the full spec).
            # Detect which case we have: if the object has a top-level "groups" or
            # "defaultTimeoutMs" key it IS the testing section; otherwise it might
            # be the full spec (fall back to old behaviour).
            $isTesting = ($null -ne $testingObj.groups -or $null -ne $testingObj.defaultTimeoutMs)
            if ($isTesting) {
                # Merge into the existing spec file
                if (-not (Test-Path $SpecFile)) {
                    Log "  WARNING: Spec file not found for merge: $SpecFile"
                    return
                }
                $specContent = Get-Content $SpecFile -Raw -Encoding UTF8
                $specObj = $specContent | ConvertFrom-Json
                $specObj | Add-Member -MemberType NoteProperty -Name "testing" -Value $testingObj -Force
                $merged = $specObj | ConvertTo-Json -Depth 20 -Compress
                # Use .NET writer — PowerShell 5.1's Set-Content -Encoding UTF8 adds BOM, rejected by Rust serde_json
                [System.IO.File]::WriteAllText($SpecFile, $merged, $UTF8_NO_BOM)
                Log "  Merged testing section into $specId ($($merged.Length) chars total)"
            } elseif ($testingObj.testing) {
                # Full spec was returned — save directly (legacy fallback)
                [System.IO.File]::WriteAllText($SpecFile, $json, $UTF8_NO_BOM)
                Log "  Saved full spec with testing section ($($json.Length) chars)"
            } else {
                Log "  WARNING: AI did not produce a recognisable testing section for $specId"
            }
        } else {
            Log "  WARNING: No JSON block found in task output for $specId"
        }
    } catch {
        Log "  WARNING: Failed to extract/save spec for $specId - $_"
    }
}

function Spec-HasTesting {
    param([string]$SpecFile)
    if (-not (Test-Path $SpecFile)) { return $false }
    try {
        $content = Get-Content $SpecFile -Raw
        $parsed = $content | ConvertFrom-Json
        return ($null -ne $parsed.testing)
    } catch { return $false }
}

# ── Build spec list ───────────────────────────────────────────────────────────
# Each entry maps: display name → specId (runner:slug)
# Keys are the data-ui-id suffixes for spec tree items

$specs = [ordered]@{
    "runner:api-surface"                  = "runner_api-surface.json"
    "runner:architecture"                 = "runner_architecture.json"
    "runner:autoresearch"                 = "runner_autoresearch.json"
    "runner:dag-workflow-editor"          = "runner_dag-workflow-editor.json"
    "runner:decision-trail"               = "runner_decision-trail.json"
    "runner:demo-video"                   = "runner_demo-video.json"
    "runner:development-intelligence"     = "runner_development-intelligence.json"
    "runner:image-quality-tests"          = "runner_image-quality-tests.json"
    "runner:llm-analytics"                = "runner_llm-analytics.json"
    "runner:memory"                       = "runner_memory.json"
    "runner:meta-optimizer"               = "runner_meta-optimizer.json"
    "runner:product-tours"                = "runner_product-tours.json"
    "runner:project-explainer"            = "runner_project-explainer.json"
    "runner:session-recap"                = "runner_session-recap.json"
    "runner:settings-backend-connection"  = "runner_settings-backend-connection.json"
    "runner:settings-agentic"             = "runner_settings-agentic.json"
    "runner:settings-ai"                  = "runner_settings-ai.json"
    "runner:settings-backup"              = "runner_settings-backup.json"
    "runner:settings-debug"               = "runner_settings-debug.json"
    "runner:settings-general"             = "runner_settings-general.json"
    "runner:settings-log-sources"         = "runner_settings-log-sources.json"
    "runner:settings-mcp"                 = "runner_settings-mcp.json"
    "runner:settings-mobile"              = "runner_settings-mobile.json"
    "runner:settings-playwright"          = "runner_settings-playwright.json"
    "runner:settings-security"            = "runner_settings-security.json"
    "runner:settings-self-healing"        = "runner_settings-self-healing.json"
    "runner:settings-storage"             = "runner_settings-storage.json"
    "runner:settings-updates"             = "runner_settings-updates.json"
    "runner:settings-world-state-verifier" = "runner_settings-world-state-verifier.json"
    "runner:specs"                        = "runner_specs.json"
    "runner:vga"                          = "runner_vga.json"
    "runner:visual-dashboard"             = "runner_visual-dashboard.json"
}

$FENCE = [char]96 + [char]96 + [char]96  # triple backtick — avoids escape headaches inside the here-string

$MSG = @"
Add a testing section to this spec.

The testing section describes HOW to test each group end-to-end (async strategies, fixtures, scenario steps). Only add entries for groups that have behavioral complexity, async operations, or external dependencies -- skip groups with only structural assertions (exists, visible, hasText).

Output ONLY the testing section object as a single JSON code block. Do NOT output the full spec -- just the testing value itself, like:

${FENCE}json
{
  "defaultTimeoutMs": 5000,
  "defaultPollIntervalMs": 200,
  "groups": {
    "group-id": {
      "scenarios": [...],
      ...
    }
  }
}
${FENCE}

Output ONLY that JSON object, nothing else inside the code block.
"@

Log "=== Add testing sections. $($specs.Count) specs to check. ==="

# Navigate to Specs page
try {
    $null = Invoke-WebRequest -Uri "$BASE/control/page/navigate" -Method POST `
            -ContentType 'application/json' -Body '{"url": "/specs", "mode": "soft"}' `
            -UseBasicParsing -ErrorAction Stop
    Log "Navigated to /specs"
} catch {
    Log "WARNING: Could not navigate to /specs - $_"
}
Start-Sleep -Seconds 2

# Reload specs from disk to pick up the latest user-specs files
try {
    $null = Invoke-WebRequest -Uri "$BASE/control/element/specs-btn-bundled/action" `
            -Method POST -ContentType 'application/json' -Body '{"action": "click"}' `
            -UseBasicParsing -ErrorAction Stop
    Log "Reloaded bundled+user specs from disk"
    Start-Sleep -Seconds 4
} catch {
    Log "WARNING: Could not click specs-btn-bundled: $_"
}

# Expand runner tree if collapsed
$treeExpanded = Evaluate-JS "document.querySelectorAll('[data-ui-id^=spec-tree-runner]').length > 2"
if ($treeExpanded -ne "True") {
    $null = Click-ByDataUiId "spec-tree-app:Qontinui Runner"
    Start-Sleep -Seconds 2
    Log "Expanded runner spec tree"
}

$done    = 0
$skipped = 0
$failed  = 0

foreach ($entry in $specs.GetEnumerator()) {
    $specId   = $entry.Key
    $fileName = $entry.Value
    $specFile = "$SPEC_DIR\$fileName"

    if ($OnlySpec -and $specId -ne $OnlySpec) { continue }

    # Skip if already has testing section
    if (Spec-HasTesting $specFile) {
        Log "$specId - already has testing section, skipping"
        $skipped++
        continue
    }

    Log "=== Processing: $specId ==="

    # Snapshot all running Spec Chat task IDs before any interaction
    $preSendIds = @()
    try {
        $pre = (Invoke-WebRequest -Uri "$RUNNER_API/task-runs/running" -UseBasicParsing -ErrorAction Stop).Content | ConvertFrom-Json
        $preSendIds = @($pre | Where-Object { $_.task_name -like "Spec Chat:*" } | Select-Object -ExpandProperty id)
    } catch {}

    # Click the spec tree item to navigate to this spec's chat
    $treeId = "spec-tree-$specId"
    $clicked = Click-ByDataUiId $treeId
    if (-not $clicked) {
        Log "  ERROR: Could not click tree item $treeId"
        $failed++
        continue
    }
    Start-Sleep -Seconds 3

    # Ensure Review mode is selected
    $null = Click-ByDataUiId "spec-chat-mode-review"
    Start-Sleep -Seconds 1

    # Click "New" to reset the session — this clears accumulated history from other specs
    $null = Click-ByDataUiId "spec-chat-new-session"
    Start-Sleep -Seconds 2
    Log "  Cleared previous session context"

    # Set the message in the spec chat input
    $set = Set-TextareaValue -UiId "spec-chat-input" -Value $MSG
    if (-not $set) {
        Log "  ERROR: Could not set spec-chat-input for $specId"
        $failed++
        continue
    }
    Start-Sleep -Seconds 1

    # Send the message — this creates a NEW task for this spec (fresh context)
    $sent = Click-ByDataUiId "spec-chat-send"
    if (-not $sent) {
        Log "  ERROR: Could not click spec-chat-send for $specId"
        $failed++
        continue
    }
    Start-Sleep -Seconds 5

    # Find the newly-created task (it won't be in preSendIds)
    $taskId = Get-NewSpecTaskId -ExcludeIds $preSendIds
    if (-not $taskId) {
        # Fallback: any spec chat task (in case task was in preSendIds due to reuse)
        $taskId = Get-RecentSpecTaskId
    }
    $priorResponseCount = 0
    Log "  Task ID: $taskId (fresh session)"

    if (-not $taskId) {
        Log "  ERROR: Could not find spec chat task for $specId"
        $failed++
        continue
    }

    # Wait for a NEW AI response (response count > prior)
    Log "  Waiting up to $MaxMinutes min for new AI response (prior count: $priorResponseCount)..."
    $responded = Wait-ForNewAIResponse -TaskId $taskId -PriorCount $priorResponseCount -MaxMins $MaxMinutes
    if (-not $responded) {
        Log "  ERROR: Timed out waiting for new AI response ($specId)"
        $failed++
        Extract-AndSave-Spec -taskId $taskId -specId $specId -SpecFile $specFile
        continue
    }

    # Extract testing section and merge into spec file
    Extract-AndSave-Spec -taskId $taskId -specId $specId -SpecFile $specFile

    # Re-check to confirm save succeeded
    if (Spec-HasTesting $specFile) {
        $done++
    } else {
        Log "  ERROR: Spec file still has no testing section after extraction for $specId"
        $failed++
    }

    Start-Sleep -Seconds 3
}

Log ""
Log "=== Done: $done saved, $skipped skipped (already have testing), $failed failed ==="
