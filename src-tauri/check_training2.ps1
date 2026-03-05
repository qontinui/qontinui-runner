# Check training data inputs from recent workflow runs

# 1. Check for artifacts matching today's runs
Write-Host "=== PIPELINE ARTIFACTS (last 20) ===" -ForegroundColor Cyan
try {
    $resp = Invoke-WebRequest -Uri 'http://localhost:9876/generator-eval/artifacts?limit=20' -UseBasicParsing
    $data = ($resp.Content | ConvertFrom-Json).data
    foreach ($a in $data) {
        $id = $a.id.Substring(0, 8)
        $desc = if ($a.description.Length -gt 60) { $a.description.Substring(0, 60) } else { $a.description }
        $date = $a.created_at.Substring(0, 19)
        Write-Host "$id | $date | success=$($a.success) | $desc"
    }
    Write-Host "Total: $($data.Count) artifacts"
} catch {
    Write-Host "Error: $_"
}

# 2. Check reflection fixes
Write-Host "`n=== REFLECTION FIXES (checking API) ===" -ForegroundColor Cyan
try {
    $resp = Invoke-WebRequest -Uri 'http://localhost:9876/generator-eval/reflection-fixes?limit=20' -UseBasicParsing
    Write-Host $resp.Content.Substring(0, [Math]::Min(1000, $resp.Content.Length))
} catch {
    Write-Host "Reflection fixes endpoint: $($_.Exception.Message)"
    # Try alternate endpoint
    try {
        $resp = Invoke-WebRequest -Uri 'http://localhost:9876/task-runs' -UseBasicParsing
        $runs = ($resp.Content | ConvertFrom-Json)
        if ($runs.data) { $runs = $runs.data }
        $reflections = $runs | Where-Object { $_.workflow_name -like "Reflection:*" -and $_.created_at -gt "2026-03-04" }
        Write-Host "Today's reflection runs: $($reflections.Count)"
        foreach ($r in $reflections) {
            Write-Host "  $($r.id.Substring(0,8)) | $($r.status) | $($r.workflow_name.Substring(0, [Math]::Min(70, $r.workflow_name.Length)))"
        }
    } catch {
        Write-Host "Task runs error: $_"
    }
}

# 3. Check a specific artifact for prompt data
Write-Host "`n=== CHECKING MOST RECENT ARTIFACT FOR PROMPT DATA ===" -ForegroundColor Cyan
try {
    $resp = Invoke-WebRequest -Uri 'http://localhost:9876/generator-eval/artifacts?limit=1' -UseBasicParsing
    $data = ($resp.Content | ConvertFrom-Json).data
    if ($data.Count -gt 0) {
        $id = $data[0].id
        Write-Host "Artifact ID: $id"
        $detail = Invoke-WebRequest -Uri "http://localhost:9876/generator-eval/artifacts/$id" -UseBasicParsing
        $artifact = ($detail.Content | ConvertFrom-Json).data
        $props = $artifact | Get-Member -MemberType NoteProperty | Select-Object -ExpandProperty Name
        Write-Host "Available properties: $($props -join ', ')"

        # Check for prompt-related fields
        foreach ($p in @('specification_prompt', 'builder_prompt', 'hardener_prompt', 'specification_criteria')) {
            $val = $artifact.$p
            if ($val) {
                $display = if ($val.ToString().Length -gt 100) { $val.ToString().Substring(0, 100) + "..." } else { $val.ToString() }
                Write-Host "  $p = $display" -ForegroundColor Green
            } else {
                Write-Host "  $p = (null/missing)" -ForegroundColor Yellow
            }
        }
    }
} catch {
    Write-Host "Error getting artifact detail: $_"
}

# 4. Check insights endpoint
Write-Host "`n=== INSIGHTS ===" -ForegroundColor Cyan
try {
    $resp = Invoke-WebRequest -Uri 'http://localhost:9876/generator-eval/insights' -UseBasicParsing
    $data = ($resp.Content | ConvertFrom-Json).data
    Write-Host "Insight count: $($data.Count)"
    foreach ($i in $data) {
        Write-Host "  [$($i.agent)] $($i.insight_type): $($i.description.Substring(0, [Math]::Min(100, $i.description.Length)))"
    }
} catch {
    Write-Host "Error: $_"
}

# 5. Check training data endpoint
Write-Host "`n=== TRAINING DATA ===" -ForegroundColor Cyan
foreach ($agent in @('specification', 'builder', 'hardener')) {
    try {
        $resp = Invoke-WebRequest -Uri "http://localhost:9876/generator-eval/training-data?agent=$agent&min_score=0.0&limit=10" -UseBasicParsing
        $data = ($resp.Content | ConvertFrom-Json).data
        Write-Host "  $agent`: $($data.Count) examples"
    } catch {
        $err = $_.Exception.Message
        if ($err.Length -gt 150) { $err = $err.Substring(0, 150) + "..." }
        Write-Host "  $agent`: ERROR - $err" -ForegroundColor Red
    }
}
