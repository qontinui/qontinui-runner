# Check reflection fixes via the insights SQL path
# The insights endpoint queries reflection_fixes internally

# Check what the DB actually has
$dbPath = "C:/Users/Joshua/AppData/Roaming/com.qontinui.runner/runner.db"

# Try using the runner's own SQL endpoint if it exists
Write-Host "=== Schema version ===" -ForegroundColor Cyan
try {
    $resp = Invoke-WebRequest -Uri 'http://localhost:9876/debug/db-version' -UseBasicParsing
    Write-Host $resp.Content
} catch {
    Write-Host "No debug endpoint - checking schema via artifacts endpoint column list"
}

# Check if we can get reflection data through existing endpoints
Write-Host "`n=== Task run detail (reflection run) ===" -ForegroundColor Cyan
try {
    # Get the most recent reflection task run
    $resp = Invoke-WebRequest -Uri 'http://localhost:9876/task-runs' -UseBasicParsing
    $runs = ($resp.Content | ConvertFrom-Json)
    if ($runs.data) { $runs = $runs.data }
    $reflections = $runs | Where-Object { $_.workflow_name -like "Reflection:*" } | Select-Object -First 2
    foreach ($r in $reflections) {
        $name = if ($r.workflow_name.Length -gt 60) { $r.workflow_name.Substring(0,60) } else { $r.workflow_name }
        Write-Host "  $($r.id.Substring(0,8)) | $($r.status) | $($r.created_at.Substring(0,19)) | $name"

        # Check if there's an output/summary
        try {
            $detail = Invoke-WebRequest -Uri "http://localhost:9876/task-runs/$($r.id)" -UseBasicParsing
            $d = $detail.Content | ConvertFrom-Json
            if ($d.data) { $d = $d.data }
            $ol = $d.output_log
            if ($ol -and $ol.Length -gt 0) {
                $preview = if ($ol.Length -gt 300) { $ol.Substring($ol.Length-300) } else { $ol }
                Write-Host "    Output tail: $preview" -ForegroundColor DarkGray
            }
        } catch {}
    }
} catch {
    Write-Host "Error: $_"
}

# Check generation stats endpoint (which includes reflection-related counts)
Write-Host "`n=== Generator stats ===" -ForegroundColor Cyan
try {
    $resp = Invoke-WebRequest -Uri 'http://localhost:9876/generator-eval/stats' -UseBasicParsing
    $stats = $resp.Content | ConvertFrom-Json
    Write-Host ($stats | ConvertTo-Json -Depth 3)
} catch {
    Write-Host "Stats endpoint error: $_"
}
