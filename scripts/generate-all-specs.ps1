#!/usr/bin/env pwsh
# generate-all-specs.ps1
# Automatically generates, applies, and persists page specs for all unspecced runner pages.
# Runs against the primary runner (port 9876) on the Specs tab.
# Usage: powershell -File scripts/generate-all-specs.ps1 [-MaxMinutes 40]

param(
    [int]$MaxMinutes = 40
)

$BASE = "http://localhost:9876/ui-bridge"
$RUNNER_API = "http://localhost:9876"
$SPEC_DIR = "$env:APPDATA\com.qontinui.runner\user-specs"
$logFile = "$PSScriptRoot\spec-gen.log"

New-Item -ItemType Directory -Force -Path $SPEC_DIR | Out-Null
"" | Out-File $logFile -Force

function Log {
    param([string]$msg)
    $ts = Get-Date -Format "HH:mm:ss"
    $line = "[$ts] $msg"
    Write-Host $line
    Add-Content -Path $logFile -Value $line
}

function Get-UIElements {
    # Use /control/elements (hook-registered) not /control/discover (DOM scan).
    # Discover re-scans the DOM and misses dynamically-registered tree nodes;
    # elements returns what React hooks have registered, which includes the
    # spec tree no-spec buttons when the tree is expanded.
    try {
        $r = Invoke-WebRequest -Uri "$BASE/control/elements" -Method GET -UseBasicParsing -ErrorAction Stop
        return ($r.Content | ConvertFrom-Json).data.elements
    } catch {
        return @()
    }
}

function Click-UIElement {
    param([string]$Id)
    try {
        $r = Invoke-WebRequest -Uri "$BASE/control/element/$Id/action" -Method POST -ContentType 'application/json' -Body '{"action": "click"}' -UseBasicParsing -ErrorAction Stop
        return ($r.Content | ConvertFrom-Json).success -eq $true
    } catch {
        return $false
    }
}

function Expand-RunnerTree {
    # Only click the tree header if the no-spec buttons are NOT already visible.
    # If they are visible, the tree is already expanded - don't toggle it closed.
    $elements = Get-UIElements
    $anyNoSpec = $elements | Where-Object { $_.id -like "button-*-no-spec" } | Select-Object -First 1
    if ($anyNoSpec) {
        Log "  Tree already expanded"
        return
    }
    $treeBtn = $elements | Where-Object { $_.id -like "button-qontinui-runner-*" } | Select-Object -First 1
    if ($treeBtn) {
        $null = Click-UIElement $treeBtn.id
        Start-Sleep -Seconds 3
        Log "  Tree expand clicked"
    } else {
        Log "  WARNING: Could not find runner tree header"
    }
}

function Wait-ForApplyButton {
    # $SpecId: the runner:xxx specId for the page being generated. When provided, looks for
    # the specific data-testid-based button ID that SpecChatPanel now emits, so ID collisions
    # between pages with similar description prefixes no longer cause false timeouts.
    # Falls back to the old wildcard pattern for pages without a specId or for compatibility.
    param([int]$MaxMins = 35, [string]$ExcludeId = "", [string]$SpecId = "")
    $maxSecs = $MaxMins * 60
    $elapsed = 0
    $interval = 20
    # Derive the stable testid-based element ID: "apply-page-spec-runner-decision-trail"
    $specificId = if ($SpecId) { "apply-page-spec-$($SpecId -replace '[:/\\]', '-')" } else { $null }
    while ($elapsed -lt $maxSecs) {
        Start-Sleep -Seconds $interval
        $elapsed += $interval
        $elements = Get-UIElements
        # Prefer stable specId-based button (unique per page, no collision risk)
        $btn = if ($specificId) {
            $elements | Where-Object { $_.id -eq $specificId } | Select-Object -First 1
        } else { $null }
        # Fall back to old wildcard pattern (excludes any pre-existing button by ID)
        if (-not $btn) {
            $btn = $elements | Where-Object { $_.id -like "button-apply-as-page-spec*" -and $_.id -ne $ExcludeId } | Select-Object -First 1
        }
        if ($btn) { return $btn.id }
        Log "  Waiting... ($([int]($elapsed/60))m/$MaxMins m)"
    }
    return $null
}

function Get-RecentSpecTaskId {
    try {
        $resp = (Invoke-WebRequest -Uri "$RUNNER_API/task-runs/running" -UseBasicParsing -ErrorAction Stop).Content | ConvertFrom-Json
        $task = $resp | Where-Object { $_.task_name -like "Spec Chat:*" } | Select-Object -First 1
        if ($task) { return $task.id }
    } catch {}
    return $null
}

function Extract-AndSave-Spec {
    param([string]$taskId, [string]$specId)
    if (-not $taskId) { return }
    try {
        $resp = (Invoke-WebRequest -Uri "$RUNNER_API/task-runs/$taskId/output?tail_chars=300000" -UseBasicParsing).Content
        $output = ($resp | ConvertFrom-Json).output
        $fence = [char]96 + [char]96 + [char]96
        $pattern = "(?s)$fence(?:json)?\s*\n(\{.*?\})\s*\n$fence"
        $blocks = [regex]::Matches($output, $pattern)
        if ($blocks.Count -gt 0) {
            $json = $blocks[$blocks.Count - 1].Groups[1].Value.Trim()
            $null = $json | ConvertFrom-Json  # validate
            $safeName = $specId -replace ':', '_'
            $path = "$SPEC_DIR\$safeName.json"
            # Use .NET writer directly — PowerShell 5.1's Set-Content -Encoding UTF8 adds a BOM,
            # which causes Rust's serde_json to reject the file silently.
            [System.IO.File]::WriteAllText($path, $json, [System.Text.Encoding]::UTF8)
            Log "  Saved $specId to disk ($($json.Length) chars)"
        } else {
            Log "  WARNING: No JSON block found in task output for $specId"
        }
    } catch {
        Log "  WARNING: Failed to extract/save spec for $specId - $_"
    }
}

# Parallel arrays: button slug (from page label) and correct spec ID (runner:{navId})
$pageBtns = @(
    "account", "advanced-ai", "ai-providers",
    "architecture", "autoresearch", "backup", "dag-editor", "dashboard",
    "debug", "decision-trail", "demo-videos", "dev-intelligence", "explainer",
    "general", "image-quality", "llm-analytics", "log-sources", "mcp-servers",
    "memory", "meta-optimizer", "mobile", "playwright", "product-tours",
    "security", "self-healing", "session-recap", "specs", "storage",
    "updates", "visual-gui", "world-state-verifier"
)
$pageSpecIds = @(
    "runner:settings-account", "runner:settings-agentic", "runner:settings-ai",
    "runner:architecture", "runner:autoresearch", "runner:settings-backup", "runner:dag-workflow-editor", "runner:visual-dashboard",
    "runner:settings-debug", "runner:decision-trail", "runner:demo-video", "runner:development-intelligence", "runner:project-explainer",
    "runner:settings-general", "runner:image-quality-tests", "runner:llm-analytics", "runner:settings-log-sources", "runner:settings-mcp",
    "runner:memory", "runner:meta-optimizer", "runner:settings-mobile", "runner:settings-playwright", "runner:product-tours",
    "runner:settings-security", "runner:settings-self-healing", "runner:session-recap", "runner:specs", "runner:settings-storage",
    "runner:settings-updates", "runner:vga", "runner:settings-world-state-verifier"
)

Log "=== Spec generation loop. $($pageBtns.Count) pages to process. ==="

# Activate Specs tab
try {
    $null = Invoke-WebRequest -Uri "$BASE/control/tab/activate" -Method POST -ContentType 'application/json' -Body '{"tabId":"specs"}' -UseBasicParsing -ErrorAction Stop
    Log "Specs tab activated"
} catch {
    Log "WARNING: Could not activate Specs tab - $_"
}
Start-Sleep -Seconds 2

Log "Expanding runner spec tree..."
Expand-RunnerTree

$done = 0
$skipped = 0
$failed = 0

for ($i = 0; $i -lt $pageBtns.Count; $i++) {
    $btn = $pageBtns[$i]
    $specId = $pageSpecIds[$i]
    $pageBtn = "button-$btn-no-spec"

    # Get current elements
    $elements = Get-UIElements
    $btnElem = $elements | Where-Object { $_.id -eq $pageBtn } | Select-Object -First 1

    if (-not $btnElem) {
        # Check if tree is collapsed (no no-spec buttons at all)
        $anyNoSpec = $elements | Where-Object { $_.id -like "button-*-no-spec" } | Select-Object -First 1
        if (-not $anyNoSpec) {
            # Tree is collapsed - expand it and retry
            Log "  Tree collapsed, expanding..."
            $treeBtn = $elements | Where-Object { $_.id -like "button-qontinui-runner-*" } | Select-Object -First 1
            if ($treeBtn) {
                $null = Click-UIElement $treeBtn.id
                Start-Sleep -Seconds 3
                $elements = Get-UIElements
                $btnElem = $elements | Where-Object { $_.id -eq $pageBtn } | Select-Object -First 1
            }
        }
    }

    if (-not $btnElem) {
        Log "$btn - already specced (no button found), skipping"
        $skipped++
        continue
    }

    Log "=== Processing: $btn ($specId) ==="

    # Note any pre-existing apply button so we can ignore it while waiting
    $preExistingApplyBtn = $elements | Where-Object { $_.id -like "button-apply-as-page-spec*" } | Select-Object -First 1
    $preExistingApply = if ($preExistingApplyBtn) { $preExistingApplyBtn.id } else { "" }

    # Click page button to select the unspecced page in the tree
    if (-not (Click-UIElement $pageBtn)) {
        Log "  ERROR: Failed to click $pageBtn"
        $failed++
        continue
    }
    Start-Sleep -Seconds 2

    # Click Generate Spec button in the detail panel
    if (-not (Click-UIElement "button-generate-spec")) {
        Log "  ERROR: Failed to click generate-spec for $btn"
        $failed++
        continue
    }
    Start-Sleep -Seconds 5

    # Get the task ID that was just started
    $taskId = Get-RecentSpecTaskId
    Log "  Task ID: $taskId"

    # Wait for a NEW apply button - use specId-based detection to avoid ID collisions
    Log "  Waiting up to $MaxMinutes min for spec (excluding: $preExistingApply)..."
    $applyId = Wait-ForApplyButton -MaxMins $MaxMinutes -ExcludeId $preExistingApply -SpecId $specId
    if (-not $applyId) {
        Log "  ERROR: Timed out waiting for $btn (>${MaxMinutes}m) - falling back to task output extraction"
        $failed++
        if (-not $taskId) { $taskId = Get-RecentSpecTaskId }
        Extract-AndSave-Spec -taskId $taskId -specId $specId
        continue
    }

    Log "  Apply button: $applyId - clicking..."
    Start-Sleep -Seconds 2
    if (Click-UIElement $applyId) {
        Log "  Applied $btn in memory"
        $done++
    } else {
        Log "  WARNING: Failed to click apply for $btn"
    }

    # Extract JSON from task output and save to disk
    if (-not $taskId) {
        $taskId = Get-RecentSpecTaskId
    }
    Extract-AndSave-Spec -taskId $taskId -specId $specId
    Start-Sleep -Seconds 3
}

Log "=== Done! Applied: $done, Skipped: $skipped, Failed: $failed ==="
Log "Saved specs in: $SPEC_DIR"
Get-ChildItem $SPEC_DIR | Select-Object Name, Length | Format-Table
